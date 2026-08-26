"""ファイルを vcode のフレーム列 (PNG + ループ再生用 HTML) に変換する。

PWA / アプリの送信側 (`core-wasm` の `VcodeTx`) と同じバイト列を、Python の標準
ライブラリだけで組み立てる。出力ディレクトリの `index.html` をブラウザで全画面表示
すれば、そのままアプリ・PWA の「V受信」で受け取れる。

    python tools/vcode_encode.py photo.jpg
    python tools/vcode_encode.py photo.jpg -o out --grid 9x8 --bpc 2 --fps 15

パイプライン (docs/vcode_format.md と core-wasm/src/lib.rs の VcodeTx に対応):

    ファイル
      -> wrap_file()     [\0VF1][ver][name][mime][data]  受信側で元名/MIME を復元する
      -> wrap_payload()  先頭に CRC-32 (エンドツーエンドの最終検証)
      -> RaptorQ         パケット = [SBN 1B][ESI 3B][symbol]
      -> vcode フレーム   1 ブロック = 1 パケット + CRC-32 を格子 grid_w x grid_h に敷き詰め

制約: RaptorQ は **source パケットだけ** を生成する (リペアパケット無し)。RFC 6330 の
中間シンボル生成は GF(256) の連立方程式を解く必要があり、純 Python には重すぎるため。
source シンボルはシステマティック符号なので元データそのままで、受信側の RaptorQ
デコーダはこれだけで復号できる。代償は取りこぼしたブロックを次の周回まで待つこと
(リペアがあれば別のパケットで埋められる)。
"""

from __future__ import annotations

import argparse
import mimetypes
import pathlib
import struct
import sys
import zlib

# ======================================================================
# vcode フレームフォーマット (docs/vcode_format.md / vcode/src/lib.rs と 1:1)
# ======================================================================

STRIP_H = 6      # 上下ストリップの高さ (セル)
CORNER = 6       # コーナーマーカーの一辺 (セル)
BLOCK = 20       # データブロックの一辺 (セル)
MAGIC = 0xB9     # ヘッダ先頭のマジックバイト
HEADER_LEN = 24  # ヘッダのシリアライズ長 (CRC-32 込み)
VERSION = 1      # フォーマットバージョン (v0 とは非互換)

LEVEL_GRAY = (0, 85, 170, 255)  # bpc=2 の輝度 4 値。レベル 0 = 黒
BLACK, WHITE = 0, 255


def frame_size(grid_w: int, grid_h: int) -> tuple[int, int]:
    """フレームのセル数 (幅, 高さ)。"""
    return grid_w * BLOCK, grid_h * BLOCK + 2 * STRIP_H


def block_bytes(bpc: int) -> int:
    return BLOCK * BLOCK * bpc // 8


def block_payload_len(bpc: int) -> int:
    """CRC-32 を除いたブロックペイロード長 (シリアライズ済みパケットがそのまま入る)。"""
    return block_bytes(bpc) - 4


def packet_size(bpc: int) -> int:
    """このレイアウトに合わせる RaptorQ の packet_size (= 4B payload ID を引いた分)。"""
    return block_payload_len(bpc) - 4


def crc32(data: bytes) -> int:
    """CRC-32/ISO-HDLC。zlib と vcode::crc32 は同一多項式・同一初期値。"""
    return zlib.crc32(data) & 0xFFFFFFFF


# ---- ペイロードのラップ (vcode::wrap_file / wrap_payload) ----------------

FILE_MAGIC = b"\x00VF1"  # 先頭 NUL で実ファイルの先頭バイトとの衝突を避ける


def wrap_file(name: str, mime: str, data: bytes) -> bytes:
    """[MAGIC 4][ver 1][name_len u16 LE][name][mime_len u16 LE][mime][data]。"""
    nb = name.encode("utf-8")[:0xFFFF]
    mb = mime.encode("utf-8")[:0xFFFF]
    return b"".join([
        FILE_MAGIC, b"\x01",
        struct.pack("<H", len(nb)), nb,
        struct.pack("<H", len(mb)), mb,
        data,
    ])


def wrap_payload(data: bytes) -> bytes:
    """fountain 符号化の前に付けるエンドツーエンド CRC-32 (BE)。"""
    return struct.pack(">I", crc32(data)) + data


# ---- 既知セル (コーナー / 較正パターン) ----------------------------------


def corner_black(which: str, r: int, c: int) -> bool:
    """外周 1 セルは全コーナー共通で黒。内部 4x4 で回転・鏡像を判定する。"""
    if r == 0 or r == CORNER - 1 or c == 0 or c == CORNER - 1:
        return True
    if which == "TL":
        return True                              # 塗りつぶし
    if which == "TR":
        return False                             # 白抜きリング
    if which == "BL":
        return 2 <= r < 4 and 2 <= c < 4         # 中央 2x2 のみ黒
    return (r + c) % 2 == 0                      # BR: 市松


def calib_black(r: int, c: int) -> bool:
    """座標ハッシュによる擬似ランダム既知パターン (周期パターンの位相トラップを避ける)。"""
    x = ((r * 0x9E3779B1) & 0xFFFFFFFF) ^ ((c * 0x85EBCA77) & 0xFFFFFFFF)
    x ^= x >> 13
    x = (x * 0xC2B2AE3D) & 0xFFFFFFFF
    x ^= x >> 16
    return x & 1 == 1


# ---- フレームヘッダ ------------------------------------------------------


def frame_header(bpc: int, grid_w: int, grid_h: int, frame_seq: int, oti: bytes) -> bytes:
    """magic|version|bpc|block|grid_w|grid_h|frame_seq(u16 LE)|OTI(12B)|CRC-32(BE)。"""
    buf = bytearray(HEADER_LEN)
    buf[0] = MAGIC
    buf[1] = VERSION
    buf[2] = bpc
    buf[3] = BLOCK
    buf[4] = grid_w
    buf[5] = grid_h
    buf[6:8] = struct.pack("<H", frame_seq & 0xFFFF)
    buf[8:20] = oti
    buf[20:24] = struct.pack(">I", crc32(bytes(buf[:20])))
    return bytes(buf)


# ---- セル展開テーブル ----------------------------------------------------
# バイト -> セルのグレー値列。ビットは MSB-first、1 = 黒。
# bpc=1: 1 バイト = 8 セル / bpc=2: 1 バイト = 4 セル (ビットペア -> 輝度 4 値)。

_CELLS_1BPC = [
    bytes(BLACK if (b >> (7 - j)) & 1 else WHITE for j in range(8))
    for b in range(256)
]
_CELLS_2BPC = [
    bytes(LEVEL_GRAY[(b >> (6 - 2 * j)) & 3] for j in range(4))
    for b in range(256)
]
_HEADER_BITS = 8 * HEADER_LEN


def frame_template(grid_w: int, grid_h: int) -> bytearray:
    """フレームごとに変わらない既知セル (コーナー + 上下ストリップ) を敷いた土台。"""
    w, h = frame_size(grid_w, grid_h)
    cells = bytearray(b"\xff" * (w * h))

    for which, orow, ocol in (
        ("TL", 0, 0), ("TR", 0, w - CORNER),
        ("BL", h - CORNER, 0), ("BR", h - CORNER, w - CORNER),
    ):
        for r in range(CORNER):
            row = (orow + r) * w
            for c in range(CORNER):
                cells[row + ocol + c] = BLACK if corner_black(which, r, c) else WHITE

    # 行 0: 上端タイミング行。下ストリップ: 較正パターン (水平スケール誤差の拘束)
    for r in [0] + list(range(h - STRIP_H, h)):
        row = r * w
        for c in range(CORNER, w - CORNER):
            cells[row + c] = BLACK if calib_black(r, c) else WHITE
    return cells


def encode_frame(template: bytearray, header: bytes, blocks: list[bytes],
                 grid_w: int, grid_h: int, bpc: int) -> bytearray:
    """テンプレートにヘッダとデータブロックを載せて 1 フレーム分のセル列を返す。"""
    w, _ = frame_size(grid_w, grid_h)
    cells = bytearray(template)

    # ヘッダ: 行 1..STRIP_H のコーナー間に入るだけコピーを繰り返す (端数は白のまま)
    hdr = b"".join(_CELLS_1BPC[b] for b in header)
    span = w - 2 * CORNER
    total = (STRIP_H - 1) * span
    for i in range(total // _HEADER_BITS * _HEADER_BITS):
        r, c = 1 + i // span, CORNER + i % span
        cells[r * w + c] = hdr[i % _HEADER_BITS]

    # データブロック: ペイロード + CRC-32 を行優先で敷き詰める
    table = _CELLS_1BPC if bpc == 1 else _CELLS_2BPC
    for bi, payload in enumerate(blocks):
        content = payload + struct.pack(">I", crc32(payload))
        flat = b"".join(table[b] for b in content)
        orow = STRIP_H + (bi // grid_w) * BLOCK
        ocol = (bi % grid_w) * BLOCK
        for r in range(BLOCK):
            off = (orow + r) * w + ocol
            cells[off:off + BLOCK] = flat[r * BLOCK:(r + 1) * BLOCK]
    return cells


# ======================================================================
# RaptorQ (RFC 6330) — source パケットのみ
# raptorq クレート 2.x (ObjectTransmissionInformation / Encoder) と同じ結果を出す
# ======================================================================

MAX_SOURCE_SYMBOLS_PER_BLOCK = 56403  # K'_max
DECODER_MEMORY = 10 * 1024 * 1024     # raptorq の with_defaults 既定値


def _partition(i: int, j: int) -> tuple[int, int, int, int]:
    """Partition[I, J] (RFC 6330 4.4.1.2) -> (IL, IS, JL, JS)。"""
    il = -(-i // j)
    is_ = i // j
    jl = i - is_ * j
    return il, is_, jl, j - jl


class Oti:
    """Object Transmission Information (12 バイト)。全フレームのヘッダに載る。"""

    def __init__(self, transfer_length: int, symbol_size: int,
                 source_blocks: int, sub_blocks: int, alignment: int):
        self.transfer_length = transfer_length
        self.symbol_size = symbol_size
        self.source_blocks = source_blocks
        self.sub_blocks = sub_blocks
        self.alignment = alignment

    def serialize(self) -> bytes:
        f = self.transfer_length
        return bytes([
            (f >> 32) & 0xFF, (f >> 24) & 0xFF, (f >> 16) & 0xFF, (f >> 8) & 0xFF, f & 0xFF,
            0,  # Reserved
            self.symbol_size >> 8, self.symbol_size & 0xFF,
            self.source_blocks,
            self.sub_blocks >> 8, self.sub_blocks & 0xFF,
            self.alignment,
        ])


def oti_with_defaults(transfer_length: int, max_packet_size: int) -> Oti:
    """raptorq の ObjectTransmissionInformation::with_defaults と同じ導出。"""
    alignment, sub_symbol_size = (8, 8) if max_packet_size >= 8 * 8 else (1, 1)
    symbol_size = max_packet_size - (max_packet_size % alignment)
    kt = -(-transfer_length // symbol_size)
    n_max = symbol_size // (sub_symbol_size * alignment)

    def kl(n: int) -> int:
        # 本来は K' の表を逆順に走査して上限以下の最大値を採る。vcode の packet_size
        # (bpc=1 で 42 / bpc=2 で 92) では上限が K'_max を常に超えるので分岐しない。
        x = -(-symbol_size // (alignment * n))
        if DECODER_MEMORY // (alignment * x) < MAX_SOURCE_SYMBOLS_PER_BLOCK:
            raise NotImplementedError(
                f"symbol_size={symbol_size} では K' の表引きが必要 (この最小実装の範囲外)")
        return MAX_SOURCE_SYMBOLS_PER_BLOCK

    z = -(-kt // kl(n_max))
    n = 1
    for i in range(1, n_max + 1):
        n = i
        if -(-kt // z) <= kl(i):
            break
    return Oti(transfer_length, symbol_size, z, n, alignment)


def source_packets(data: bytes, oti: Oti) -> list[bytes]:
    """全 source ブロックの source パケット [SBN 1B][ESI 3B BE][symbol] を順に返す。"""
    t = oti.symbol_size
    kt = -(-oti.transfer_length // t)
    kl, ks, zl, zs = _partition(kt, oti.source_blocks)

    packets: list[bytes] = []
    offset = 0
    for sbn, symbols_in_block in enumerate([kl] * zl + [ks] * zs):
        end = offset + symbols_in_block * t
        block = data[offset:end]
        block += b"\x00" * (end - offset - len(block))  # 末尾ブロックはゼロ埋め
        offset = end
        for esi, symbol in enumerate(_split_symbols(block, oti)):
            packets.append(bytes([sbn, (esi >> 16) & 0xFF, (esi >> 8) & 0xFF, esi & 0xFF]) + symbol)
    return packets


def _split_symbols(block: bytes, oti: Oti) -> list[bytes]:
    """ソースブロックをシンボルに切る。N > 1 ならサブブロック interleave (4.4.1.2)。"""
    t = oti.symbol_size
    count = len(block) // t
    if oti.sub_blocks <= 1:
        return [block[i * t:(i + 1) * t] for i in range(count)]

    symbols = [bytearray() for _ in range(count)]
    tl, ts, nl, ns = _partition(t // oti.alignment, oti.sub_blocks)
    offset = 0
    for sub in range(nl + ns):
        size = (tl if sub < nl else ts) * oti.alignment
        for symbol in symbols:
            symbol += block[offset:offset + size]
            offset += size
    return [bytes(s) for s in symbols]


# ======================================================================
# PNG 書き出し (zlib のみ)
# ======================================================================

_REPEAT = [bytes([v]) for v in range(256)]


def png_bytes(cells: bytearray, w: int, h: int, scale: int) -> bytes:
    """セル列をグレースケール PNG にする。scale はセルあたりのピクセル数。"""
    raw = bytearray()
    for r in range(h):
        row = bytes(cells[r * w:(r + 1) * w])
        if scale > 1:
            row = b"".join(_REPEAT[v] * scale for v in row)
        for _ in range(scale):
            raw.append(0)  # フィルタタイプ: None
            raw += row

    def chunk(tag: bytes, body: bytes) -> bytes:
        return (struct.pack(">I", len(body)) + tag + body
                + struct.pack(">I", zlib.crc32(tag + body) & 0xFFFFFFFF))

    return b"".join([
        b"\x89PNG\r\n\x1a\n",
        chunk(b"IHDR", struct.pack(">IIBBBBB", w * scale, h * scale, 8, 0, 0, 0, 0)),
        chunk(b"IDAT", zlib.compress(bytes(raw), 9)),
        chunk(b"IEND", b""),
    ])


# ======================================================================
# 再生用 HTML (PWA の VcodeSender と同じ描画: 最近傍拡大 + アスペクト維持)
# ======================================================================

HTML_TEMPLATE = """<!DOCTYPE html>
<html lang="ja">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>vcode {label}</title>
<style>
  html, body {{ margin: 0; height: 100%; background: #fff; overflow: hidden; }}
  #c {{ display: block; width: 100vw; height: 100vh; }}
  #hud {{ position: fixed; left: 8px; top: 8px; padding: 4px 8px; border-radius: 4px;
         font: 12px/1.5 system-ui, sans-serif; color: #666; background: rgba(255,255,255,.85); }}
  #hud.off {{ display: none; }}
</style>
</head>
<body>
<canvas id="c"></canvas>
<div id="hud">読み込み中...</div>
<script>
const N = {n_frames}, W = {w}, H = {h}, BYTES = {payload_len}, GRID = "{grid}", BPC = {bpc};
const fps = Number(new URLSearchParams(location.search).get("fps")) || {fps};
const canvas = document.getElementById("c"), ctx = canvas.getContext("2d");
const hud = document.getElementById("hud");

const frames = Array.from({{ length: N }}, (_, i) => {{
  const img = new Image();
  img.src = "frame_" + String(i).padStart(4, "0") + ".png";
  return img;
}});

function resize() {{
  const dpr = window.devicePixelRatio || 1;
  canvas.width = Math.round(innerWidth * dpr);
  canvas.height = Math.round(innerHeight * dpr);
}}
addEventListener("resize", resize);
resize();

// PWA の VcodeSender._drawFrame と同じ: 白背景 + 最近傍でアスペクト維持フィット
function draw(img) {{
  ctx.fillStyle = "#fff";
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.imageSmoothingEnabled = false;
  const s = Math.min(canvas.width / W, canvas.height / H);
  const dw = Math.floor(W * s), dh = Math.floor(H * s);
  ctx.drawImage(img, 0, 0, img.width, img.height,
                (canvas.width - dw) >> 1, (canvas.height - dh) >> 1, dw, dh);
}}

let i = 0, pass = 1;
function tick() {{
  draw(frames[i]);
  hud.textContent = `${{BYTES}} B · ${{GRID}} · ${{BPC}}bit/cell · ${{fps}}fps · `
    + `frame ${{i + 1}}/${{N}} · ${{pass}} 巡目 · クリックで全画面 / h で表示切替`;
  i = (i + 1) % N;
  if (i === 0) pass++;
  setTimeout(tick, 1000 / fps);
}}

Promise.all(frames.map(img => img.decode().catch(() => {{}}))).then(tick);
addEventListener("click", () => (document.fullscreenElement
  ? document.exitFullscreen() : document.documentElement.requestFullscreen()));
addEventListener("keydown", e => {{ if (e.key === "h") hud.classList.toggle("off"); }});
</script>
</body>
</html>
"""


# ======================================================================
# CLI
# ======================================================================


def build(payload: bytes, grid_w: int, grid_h: int, bpc: int):
    """ペイロードを vcode フレーム (セル列) の列にする。VcodeTx と同じ割り当て。"""
    oti = oti_with_defaults(len(payload), packet_size(bpc))
    packets = source_packets(payload, oti)
    oti_bytes = oti.serialize()

    block_count = grid_w * grid_h
    n_frames = -(-len(packets) // block_count)
    pad_len = block_payload_len(bpc)
    template = frame_template(grid_w, grid_h)

    frames = []
    for f in range(n_frames):
        # パケットは循環参照して全フレームを満杯にする (端数フレームも情報を運ぶ)。
        # raptorq のシンボル丸めでパケットが短い分はゼロパディング。
        blocks = [
            packets[(f * block_count + j) % len(packets)].ljust(pad_len, b"\x00")
            for j in range(block_count)
        ]
        header = frame_header(bpc, grid_w, grid_h, f, oti_bytes)
        frames.append(encode_frame(template, header, blocks, grid_w, grid_h, bpc))
    return frames, oti, packets


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(
        description="ファイルを vcode のフレーム列 (PNG + 再生用 HTML) に変換する",
        epilog="出力先の index.html をブラウザで開き、全画面にして「V受信」をかざす。",
    )
    p.add_argument("input", type=pathlib.Path, help="送りたいファイル")
    p.add_argument("-o", "--out", type=pathlib.Path,
                   help="出力ディレクトリ (既定: <入力名>_vcode)")
    p.add_argument("--grid", default="7x6",
                   help="格子 GWxGH。受信側の自動検出は 5x4 / 7x6 / 9x8 のみ (既定: 7x6)")
    p.add_argument("--bpc", type=int, default=2, choices=(1, 2),
                   help="1 セルあたりのビット数。2 = 輝度 4 値 (既定: 2)")
    p.add_argument("--fps", type=int, default=12,
                   help="HTML の再生 fps。1 フレームは 2 リフレッシュ周期以上必要 (既定: 12)")
    p.add_argument("--scale", type=int, default=1,
                   help="PNG のセルあたりピクセル数。HTML 側で最近傍拡大するので通常は 1 で足りる")
    p.add_argument("--name", help="受信側に伝えるファイル名 (既定: 入力ファイル名)")
    p.add_argument("--mime", help="受信側に伝える MIME (既定: 拡張子から推定)")
    p.add_argument("--raw", action="store_true",
                   help="ファイル名/MIME ヘッダを付けず生バイトを送る")
    p.add_argument("--no-html", action="store_true", help="PNG だけ出力する")
    args = p.parse_args(argv)

    try:
        grid_w, grid_h = (int(v) for v in args.grid.lower().split("x"))
    except ValueError:
        p.error(f"--grid の書式が不正: {args.grid} (例: 7x6)")
    if not (2 <= grid_w <= 12 and 2 <= grid_h <= 12):
        p.error("--grid の各辺は 2..12")

    data = args.input.read_bytes()
    name = args.name or args.input.name
    mime = args.mime or mimetypes.guess_type(name)[0] or "application/octet-stream"
    payload = wrap_payload(data if args.raw else wrap_file(name, mime, data))

    frames, oti, packets = build(payload, grid_w, grid_h, args.bpc)
    w, h = frame_size(grid_w, grid_h)

    out = args.out or args.input.with_name(args.input.stem + "_vcode")
    out.mkdir(parents=True, exist_ok=True)
    for i, cells in enumerate(frames):
        (out / f"frame_{i:04d}.png").write_bytes(png_bytes(cells, w, h, args.scale))

    if not args.no_html:
        (out / "index.html").write_text(HTML_TEMPLATE.format(
            label=f"{args.input.name} ({len(data)} B)",
            n_frames=len(frames), w=w, h=h, payload_len=len(data),
            grid=f"{grid_w}x{grid_h}", bpc=args.bpc, fps=args.fps,
        ), encoding="utf-8")

    per_frame = grid_w * grid_h * block_payload_len(args.bpc)
    loop_sec = len(frames) / args.fps
    print(f"{args.input} ({len(data)} B, {mime})")
    print(f"  ペイロード  {len(payload)} B ({'生バイト' if args.raw else 'ファイル名/MIME 付き'} + CRC)")
    print(f"  RaptorQ    symbol_size={oti.symbol_size} source_blocks={oti.source_blocks} "
          f"packets={len(packets)}")
    print(f"  フレーム    {len(frames)} 枚 / {w}x{h} セル / {per_frame} B/枚")
    print(f"  1 巡        {loop_sec:.1f} 秒 @ {args.fps}fps "
          f"(理論 {len(data) / loop_sec / 1024:.1f} KB/s)")
    print(f"  出力       {out}{'' if args.no_html else ' の index.html をブラウザで全画面表示'}")
    if (grid_w, grid_h) not in ((5, 4), (7, 6), (9, 8)):
        print(f"  [注意] {grid_w}x{grid_h} は受信側の自動検出候補に無い。受信側で格子を明示指定すること")
    return 0


if __name__ == "__main__":
    sys.exit(main())

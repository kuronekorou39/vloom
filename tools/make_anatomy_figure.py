"""docs/images/vcode_anatomy.png (vcode フレームの構造図) を再生成する。

実物のフレームを tools/vcode_encode.py で作り、その上に領域の注釈と凡例を描く。
依存: Pillow (uv run --group dev)。日本語フォントは Windows のメイリオを使う
(他 OS ではそれらしい CJK フォントのパスに差し替えること)。

    uv run --group dev python tools/make_anatomy_figure.py
"""

from __future__ import annotations

import os
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

sys.path.insert(0, str(Path(__file__).parent))
import vcode_encode  # noqa: E402

SC = 3  # 描画するセルあたりのピクセル数
GRID = (13, 18)  # 現行既定の格子で描く (実際に使われている形を見せる)
OUT = Path(__file__).parent.parent / "docs" / "images" / "vcode_anatomy.png"

FONT_REG = "C:/Windows/Fonts/meiryo.ttc"
FONT_BOLD = "C:/Windows/Fonts/meiryob.ttc"


def make_frame() -> Image.Image:
    """1 フレームぶんを埋め切るランダムペイロードで実物のフレームを作る。"""
    gw, gh = GRID
    payload = os.urandom(gw * gh * vcode_encode.packet_size(1))
    frames, _oti, _packets = vcode_encode.build(payload, gw, gh, 1)
    w, h = vcode_encode.frame_size(gw, gh)
    png = vcode_encode.png_bytes(frames[0], w, h, SC)
    with tempfile.NamedTemporaryFile(suffix=".png", delete=False) as f:
        f.write(png)
        path = f.name
    img = Image.open(path).convert("RGB")
    img.load()
    os.unlink(path)
    return img


def main() -> None:
    # コードは縦長なので、凡例は下ではなく右に置く
    frame = make_frame()
    W, H = frame.size
    pad_t = 96
    ox = 48
    legend_x = ox + W + 110
    cv = Image.new("RGB", (legend_x + 660, H + pad_t + 48), "#ffffff")
    cv.paste(frame, (ox, pad_t))
    d = ImageDraw.Draw(cv, "RGBA")

    def font(sz: int, bold: bool = False) -> ImageFont.FreeTypeFont:
        return ImageFont.truetype(FONT_BOLD if bold else FONT_REG, sz)

    c1, c2, c3, c4, c5 = "#e5484d", "#3b82f6", "#16a34a", "#f59e0b", "#8b5cf6"

    def rect(r0: int, c0: int, r1: int, cc1: int) -> tuple[int, int, int, int]:
        return (ox + c0 * SC, pad_t + r0 * SC, ox + cc1 * SC, pad_t + r1 * SC)

    def box(r0, c0, r1, cc1, color, width=5, dash=False):
        x0, y0, x1, y1 = rect(r0, c0, r1, cc1)
        if not dash:
            d.rectangle([x0, y0, x1, y1], outline=color, width=width)
            return
        x = x0
        while x < x1:
            d.line([x, y0, min(x + 10, x1), y0], fill=color, width=width)
            d.line([x, y1, min(x + 10, x1), y1], fill=color, width=width)
            x += 18
        y = y0
        while y < y1:
            d.line([x0, y, x0, min(y + 10, y1)], fill=color, width=width)
            d.line([x1, y, x1, min(y + 10, y1)], fill=color, width=width)
            y += 18

    def badge(num, x, y, color):
        r = 24
        d.ellipse([x - r, y - r, x + r, y + r], fill=color, outline="#ffffff", width=4)
        f = font(30, True)
        t = str(num)
        bb = d.textbbox((0, 0), t, font=f)
        d.text((x - (bb[2] - bb[0]) / 2 - bb[0], y - (bb[3] - bb[1]) / 2 - bb[1]), t, font=f, fill="#ffffff")

    gw, gh = GRID
    wc = gw * vcode_encode.BLOCK  # セル幅 140
    hc = gh * vcode_encode.BLOCK + 2 * vcode_encode.STRIP_H  # セル高 176
    cn = vcode_encode.CORNER
    cols0 = cn + vcode_encode.SEP  # 帯の左端 (28)
    cols1 = wc - cols0  # 帯の右端 (112)

    # ① 四隅マーカー
    for r0, c0 in [(0, 0), (0, wc - cn), (hc - cn, 0), (hc - cn, wc - cn)]:
        box(r0, c0, r0 + cn, c0 + cn, c1, 5)
    badge(1, ox + cn // 2 * SC, pad_t + cn // 2 * SC, c1)
    # ② ヘッダ (行 1..CORNER-1)
    box(1, cols0, cn - 1, cols1, c2, 5)
    badge(2, ox + wc // 2 * SC, pad_t + cn // 2 * SC, c2)
    # ③ タイミング行 (行 0)
    x0, y0, x1, y1 = rect(0, cols0, 1, cols1)
    d.rectangle([x0, y0 - 2, x1, y1 + 2], outline=c3, width=3)
    badge(3, ox + (wc + 6) * SC + 30, pad_t + 3, c3)
    d.line([ox + cols1 * SC, pad_t + 3, ox + (wc + 6) * SC + 8, pad_t + 3], fill=c3, width=3)
    # ④ データ領域 (破線) と 1 ブロックの強調
    top = vcode_encode.STRIP_H
    box(top, 0, hc - vcode_encode.STRIP_H, wc, c4, 3, dash=True)
    br0, bc0 = top + 2 * vcode_encode.BLOCK, 3 * vcode_encode.BLOCK
    box(br0, bc0, br0 + vcode_encode.BLOCK, bc0 + vcode_encode.BLOCK, c4, 6)
    badge(4, ox + (bc0 + 10) * SC, pad_t + (br0 + 10) * SC, c4)
    # ⑤ 較正帯 (末尾 BAND_ROWS 行)
    box(hc - vcode_encode.BAND_ROWS, cols0, hc, cols1, c5, 4)
    yb = pad_t + (hc - vcode_encode.BAND_ROWS // 2) * SC
    badge(5, ox + (wc + 6) * SC + 30, yb, c5)
    d.line([ox + cols1 * SC, yb, ox + (wc + 6) * SC + 8, yb], fill=c5, width=3)

    d.text((ox, 24), f"vcode フレームの構造 (格子 {gw}×{gh} · 1bit の例)", font=font(40, True), fill="#111111")

    legend = [
        (c1, "① コーナーマーカー (24セル角)。4 隅それぞれ模様が違い、向きも一緒に分かる"),
        (c2, "② ヘッダ ×5コピー — 版・格子・フレーム番号・符号化情報 (1ビット = 2×2セル)"),
        (c3, "③ タイミング行 — 既知の白黒パターン。位置合わせの微調整の基準"),
        (c4, "④ データブロック (20×20セル) = 1パケット + CRC-32。ブロック単位で回収する"),
        (c5, "⑤ 較正帯 — 既知パターン。下端側の位置と輝度の基準"),
        (None, "マーカーの内側の白い帯 (4セル) は余白。マーカー検出の対比に使う"),
    ]
    f_leg = font(24)
    tx = legend_x + 46
    maxw = cv.width - tx - 24
    y = pad_t + 24
    for color, text in legend:
        if color:
            d.rectangle([legend_x, y + 5, legend_x + 30, y + 35], fill=color)
        # 収まらない行は折り返す
        rest = text
        first = True
        while rest:
            cut = len(rest)
            while d.textlength(rest[:cut], font=f_leg) > maxw:
                cut -= 1
            d.text((tx, y), rest[:cut], font=f_leg, fill="#222222")
            rest = rest[cut:]
            y += 34
            first = False
        y += 24

    OUT.parent.mkdir(parents=True, exist_ok=True)
    cv.save(OUT)
    print(f"saved {OUT} {cv.size}")


if __name__ == "__main__":
    main()

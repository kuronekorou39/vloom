"""PWA の受信を、擬似カメラ (Y4M) に vcode の映像を流し込んで端から端まで試す。

実機もカメラも要らない回帰テスト。「Pages の受信がまったく読めない」という壊れ方を、
配信物に対してそのまま検出できる (実際、受信が古い探索のままだったのを取り逃がしていた)。

    uv run --group dev python tools/test_pwa_receive.py                 # 既定 13x18 / Pages
    uv run --group dev python tools/test_pwa_receive.py --grid 7x6
    uv run --group dev python tools/test_pwa_receive.py --url https://localhost:8443/index.html

要 Playwright (`uv run --group dev python -m playwright install chrome` で Chrome を用意)。
復元まで到達すれば終了コード 0、しなければ 1。
"""

from __future__ import annotations

import argparse
import sys
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image
from playwright.sync_api import sync_playwright

sys.path.insert(0, str(Path(__file__).parent))
import vcode_encode  # noqa: E402

PAGES = "https://kuronekorou39.github.io/vloom/index.html"


def build_y4m(path: Path, grid: str, payload_len: int, cell_px: int, hold: int) -> tuple[int, int, int]:
    """vcode のフレーム列を、擬似カメラが読める Y4M (I420) にする。"""
    gw, gh = (int(v) for v in grid.split("x"))
    body = ("vloom pwa receive test " * 200)[:payload_len].encode()
    # 送信側と同じ二重ラップ: 内側がファイル名/MIME、外側が復元結果の CRC 検証
    payload = vcode_encode.wrap_payload(
        vcode_encode.wrap_file("e2e.txt", "text/plain;charset=utf-8", body)
    )
    frames, _oti, _packets = vcode_encode.build(payload, gw, gh, 1)
    cw, ch = vcode_encode.frame_size(gw, gh)
    # 擬似カメラは getUserMedia の理想値 (1920x1080) に合わせて縮めるので、
    # 映像自体を 1080 に収まる倍率で作る (はみ出すと切られて四隅が消え、検出できない)
    fit = min(1920 / (cw * 1.25), 1080 / (ch * 1.25))
    if cell_px > fit:
        cell_px = max(1, int(fit))
        print(f"セル倍率を {cell_px}px に下げた (1080p に収めるため)")
    code_w, code_h = cw * cell_px, ch * cell_px
    # コードの外側に余白を取る (四隅マーカーの検出は周囲の白との対比を見る)
    w = ((code_w * 5 // 4) + 1) // 2 * 2
    h = ((code_h * 5 // 4) + 1) // 2 * 2
    ox, oy = (w - code_w) // 2, (h - code_h) // 2

    with path.open("wb") as f:
        f.write(f"YUV4MPEG2 W{w} H{h} F30:1 Ip A1:1 C420\n".encode())
        uv = np.full((h // 2, w // 2), 128, np.uint8).tobytes()
        for cells in frames:
            arr = np.frombuffer(bytes(cells), np.uint8).reshape(ch, cw)  # セル値がそのままグレー値
            img = np.array(Image.fromarray(arr).resize((code_w, code_h), Image.NEAREST))
            canvas = np.full((h, w), 255, np.uint8)
            canvas[oy:oy + code_h, ox:ox + code_w] = img
            for _ in range(hold):  # 擬似カメラは 30fps なので 1 フレームを複数回書く
                f.write(b"FRAME\n")
                f.write(canvas.tobytes())
                f.write(uv)
                f.write(uv)
    return w, h, len(frames)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--url", default=PAGES, help=f"試す PWA の URL (既定: {PAGES})")
    ap.add_argument("--grid", default="13x18")
    ap.add_argument("--payload", type=int, default=3000, help="送るテキストのバイト数")
    ap.add_argument("--cell-px", type=int, default=3, help="擬似カメラ映像でのセルあたり画素")
    ap.add_argument("--hold", type=int, default=4, help="1 フレームを何回書くか (30fps 基準)")
    ap.add_argument("--timeout", type=int, default=60, help="復元を待つ秒数")
    ap.add_argument("--shot", help="終了時のスクリーンショット出力先")
    args = ap.parse_args()

    tmp = Path(tempfile.gettempdir()) / "vloom_fake_cam.y4m"
    w, h, n = build_y4m(tmp, args.grid, args.payload, args.cell_px, args.hold)
    print(f"擬似カメラ {w}x{h} · {n} フレーム · 格子 {args.grid} · {args.payload}B")

    with sync_playwright() as p:
        browser = p.chromium.launch(channel="chrome", headless=True, args=[
            "--use-fake-device-for-media-stream",
            "--use-fake-ui-for-media-stream",
            f"--use-file-for-fake-video-capture={tmp}",
        ])
        ctx = browser.new_context(permissions=["camera"], ignore_https_errors=True)
        page = ctx.new_page()
        errors: list[str] = []
        page.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)
        page.on("pageerror", lambda e: errors.append(f"pageerror: {e}"))
        page.goto(args.url)
        page.wait_for_timeout(1500)
        page.evaluate("() => document.getElementById('tabRecv').click()")
        page.evaluate(f"() => {{ document.getElementById('rxGrid').value = '{args.grid}'; }}")
        page.evaluate("() => document.getElementById('rxStart').click()")
        info = ""
        for _ in range(args.timeout):
            page.wait_for_timeout(1000)
            info = page.evaluate("() => document.getElementById('rxInfo').textContent")
            if "復元成功" in info:
                break
        ok = "復元成功" in info
        print("結果:", info.strip())
        print("診断:", page.evaluate("() => document.getElementById('rxDiag').textContent").strip())
        err = page.evaluate("() => document.getElementById('rxError').textContent").strip()
        if err:
            print("エラー表示:", err)
        if errors:
            print("コンソールエラー:", errors[:3])
        if args.shot:
            page.screenshot(path=args.shot, full_page=True)
        browser.close()

    print("OK" if ok and not errors else "NG")
    return 0 if ok and not errors else 1


if __name__ == "__main__":
    raise SystemExit(main())

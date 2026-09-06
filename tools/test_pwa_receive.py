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


def build_y4m(path: Path, grid: str, payload_len: int, hold: int, cam: tuple[int, int],
              fill: float, name: str = "e2e.txt") -> tuple[int, int, int, float]:
    """vcode のフレーム列を、擬似カメラが読める Y4M (I420) にする。

    映像は実機のカメラと同じ寸法 (既定 1920x1080) で作り、コードはその高さの fill 倍に
    収める。ここを実寸で作らないと「px/セル が足りなくて読めない」という現実の壁を
    再現できず、テストだけ通ってしまう (コードが画面いっぱいの小さな映像になるため)。
    """
    gw, gh = (int(v) for v in grid.split("x"))
    body = (("vloom pwa receive test " * (payload_len // 23 + 2))[:payload_len]).encode()
    # 送信側と同じ二重ラップ: 内側がファイル名/MIME、外側が復元結果の CRC 検証
    payload = vcode_encode.wrap_payload(
        vcode_encode.wrap_file(name, "text/plain;charset=utf-8", body)
    )
    frames, _oti, _packets = vcode_encode.build(payload, gw, gh, 1)
    cw, ch = vcode_encode.frame_size(gw, gh)
    w, h = cam
    # 縦横どちらでも収まる倍率。整数倍に丸めず、実機と同じく端数のある拡大にする
    scale = min(w * fill / cw, h * fill / ch)
    code_w, code_h = max(1, round(cw * scale)), max(1, round(ch * scale))
    ox, oy = (w - code_w) // 2, (h - code_h) // 2

    with path.open("wb") as f:
        f.write(f"YUV4MPEG2 W{w} H{h} F30:1 Ip A1:1 C420\n".encode())
        uv = np.full((h // 2, w // 2), 128, np.uint8).tobytes()
        for cells in frames:
            arr = np.frombuffer(bytes(cells), np.uint8).reshape(ch, cw)  # セル値がそのままグレー値
            img = np.array(Image.fromarray(arr).resize((code_w, code_h), Image.BILINEAR))
            canvas = np.full((h, w), 255, np.uint8)
            canvas[oy:oy + code_h, ox:ox + code_w] = img
            for _ in range(hold):  # 擬似カメラは 30fps なので 1 フレームを複数回書く
                f.write(b"FRAME\n")
                f.write(canvas.tobytes())
                f.write(uv)
                f.write(uv)
    return w, h, len(frames), code_h / ch


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--url", default=PAGES, help=f"試す PWA の URL (既定: {PAGES})")
    ap.add_argument("--grid", default="13x18")
    ap.add_argument("--payload", type=int, default=3000, help="送るテキストのバイト数")
    ap.add_argument("--cam", default="1920x1080",
                    help="擬似カメラの解像度。実機のカメラと同じ寸法にすること")
    ap.add_argument("--fill", type=float, default=0.88,
                    help="コードが映像の何割を占めるか (実機で枠に収めた状態が 0.85〜0.9)")
    ap.add_argument("--hold", type=int, default=2, help="1 フレームを何回書くか (30fps 基準)")
    ap.add_argument("--timeout", type=int, default=60, help="復元を待つ秒数")
    ap.add_argument("--shot", help="終了時のスクリーンショット出力先")
    ap.add_argument("--xss", action="store_true",
                    help="ファイル名に HTML を仕込み、実行されない (エスケープされる) ことを確かめる")
    args = ap.parse_args()

    # 受信名は送信側 (= 他人の画面) が決める。HTML を仕込んでも実行されないことを確かめる
    name = '<img src=x onerror="window.__xss=1">.txt' if args.xss else "e2e.txt"
    tmp = Path(tempfile.gettempdir()) / "vloom_fake_cam.y4m"
    cam = tuple(int(v) for v in args.cam.split("x"))
    w, h, n, cell_px = build_y4m(tmp, args.grid, args.payload, args.hold, cam, args.fill, name)
    print(f"擬似カメラ {w}x{h} · {n} フレーム · 格子 {args.grid} · {args.payload}B "
          f"· {cell_px:.2f} px/セル")

    with sync_playwright() as p:
        browser = p.chromium.launch(channel="chrome", headless=True, args=[
            "--use-fake-device-for-media-stream",
            "--use-fake-ui-for-media-stream",
            f"--use-file-for-fake-video-capture={tmp}",
        ])
        ctx = browser.new_context(permissions=["camera"], ignore_https_errors=True)
        page = ctx.new_page()
        errors: list[str] = []
        # 自己署名証明書のローカルサーバでは Service Worker の登録だけが必ず失敗する
        # ("... when fetching the script.")。ページの不具合ではないので数えない。
        # それ以外のコンソールエラーは今までどおり失敗にする。
        def on_console(m):
            if m.type == "error" and "when fetching the script" not in m.text:
                errors.append(m.text)

        page.on("console", on_console)
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
        if args.xss:
            fired = page.evaluate("() => !!window.__xss")
            escaped = page.evaluate(
                "() => document.getElementById('rxInfo').textContent.includes('<img src=x')")
            print(f"XSS 検査: 実行された={fired} / 文字として表示={escaped}")
            ok = ok and not fired and escaped
        print("結果:", info.strip())
        # 結果表 (実効スループット・走査 fps 等) をそのまま出す。条件を振ったときに
        # 「読めた/読めない」だけでなく、どれだけ速く読めたかを比べられるようにする。
        stats = page.evaluate(
            "() => Array.from(document.querySelectorAll('#rxResult table.stats tr'))"
            ".map(r => r.cells[0].textContent + ': ' + r.cells[1].textContent)")
        for row in stats:
            print("  ", row)
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

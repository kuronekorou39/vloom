"""計測用テスト画像を生成する。

条件 (格子・階調・fps・解像度) を振って比べるには、毎回同じデータを送る必要がある。
バイト列ではなく画像を使うのは、受信側で開いた瞬間に壊れているか判るため。
画像内にサイズを焼き込むので、どの条件のデータが届いたのかも一目で分かる。

目標バイト数ちょうどにするため、ラベルを焼いた画像を JPEG 品質固定で書き出しつつ、
解像度を二分探索する。元絵はリポジトリに入れていないので、差し替えるときは
SOURCES のパスを手元の画像に置き換えて実行する。

    python tools/make_testdata.py

出力: app/assets/testdata/ と web/pwa/testdata/ (Flutter と PWA の双方で使う)
"""

from __future__ import annotations

import io
import pathlib
import shutil

from PIL import Image, ImageDraw, ImageFont

# (元画像, 目標バイト数, 焼き込むラベル)。サイズごとに別の絵にすると絵柄でも見分けられる。
SOURCES = [
    ("Gemini_Generated_Image_muwveemuwveemuwv1.png", 100 * 1024, "100 KB"),
    ("Gemini_Generated_Image_muwveemuwveemuwv2.png", 500 * 1024, "500 KB"),
    ("Gemini_Generated_Image_muwveemuwveemuwv3.png", 1024 * 1024, "1 MB"),
    ("Gemini_Generated_Image_muwveemuwveemuwv4.png", 2 * 1024 * 1024, "2 MB"),
]

FLUTTER_OUT = pathlib.Path("app/assets/testdata")
PWA_OUT = pathlib.Path("web/pwa/testdata")
FONT_PATH = "C:/Windows/Fonts/arialbd.ttf"
QUALITY = 90


def render(base: Image.Image, width: int, label: str) -> Image.Image:
    """幅 width にリサイズし、下部の帯にサイズラベルを焼き込む"""
    height = round(base.height * width / base.width)
    im = base.resize((width, height), Image.LANCZOS)
    draw = ImageDraw.Draw(im)
    font_size = max(16, int(width * 0.13))
    font = ImageFont.truetype(FONT_PATH, font_size)
    pad = font_size // 3
    bar = font_size + pad * 2
    text_w = draw.textlength(label, font=font)
    # 帯を敷いてから文字を置く (絵柄に埋もれないように)
    draw.rectangle([0, height - bar, width, height], fill=(20, 22, 28))
    draw.text(((width - text_w) / 2, height - bar + pad - font_size * 0.12),
              label, font=font, fill=(255, 255, 255))
    return im


def encode(im: Image.Image) -> bytes:
    buf = io.BytesIO()
    im.save(buf, "JPEG", quality=QUALITY, optimize=True, subsampling=0)
    return buf.getvalue()


def main() -> None:
    FLUTTER_OUT.mkdir(parents=True, exist_ok=True)
    PWA_OUT.mkdir(parents=True, exist_ok=True)
    for src, target, label in SOURCES:
        if not pathlib.Path(src).exists():
            print(f"  skip (元画像なし): {src}")
            continue
        base = Image.open(src).convert("RGB")
        lo, hi, best = 200, 7000, None
        for _ in range(20):
            mid = (lo + hi) // 2
            data = encode(render(base, mid, label))
            if best is None or abs(len(data) - target) < abs(best[0] - target):
                best = (len(data), data)
            if len(data) < target:
                lo = mid + 1
            else:
                hi = mid - 1
        size, data = best
        name = f"test-{label.replace(' ', '')}.jpg"
        (FLUTTER_OUT / name).write_bytes(data)
        shutil.copy(FLUTTER_OUT / name, PWA_OUT / name)
        print(f"  {name}: {size:,} B ({size / target * 100:.1f}% of target)")
    print("\napp/lib/test_payload.dart の kTestImages に実バイト数を反映すること")


if __name__ == "__main__":
    main()

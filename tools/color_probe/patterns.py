"""色の読み分けを調べるための、答えの分かっている模様。

vcode の形式とは別物で、「画面に出した色つきのセルを、カメラがどれだけ正しく読めるか」
だけを測るためのもの。show.py が画面に出し、analyze.py が撮った画像と突き合わせる。
どちらも同じ種から同じ模様を作るので、答えをファイルで受け渡す必要はない。

画面 1920x1080 の中央に 1152x900 のデータ領域を置き、その左右に黒い目印 (四隅) を置く。
データ領域の大きさは、横長の 30x20 を全画面で出したときとほぼ同じ (受信の構図を変えずに撮れる)。
"""
from __future__ import annotations

from dataclasses import dataclass

import numpy as np

SCREEN_W, SCREEN_H = 1920, 1080
DATA_W, DATA_H = 1152, 900
DATA_X, DATA_Y = (SCREEN_W - DATA_W) // 2, (SCREEN_H - DATA_H) // 2

# 四隅の目印 (黒い正方形)。データ領域のすぐ外側、白地の上に置く
FID = 64
FID_GAP = 20


def fiducial_centers() -> np.ndarray:
    """目印 4 つの中心 (画面の画素座標)。左上・右上・右下・左下の順。"""
    xl = DATA_X - FID_GAP - FID / 2
    xr = DATA_X + DATA_W + FID_GAP + FID / 2
    yt = DATA_Y + FID / 2
    yb = DATA_Y + DATA_H - FID / 2
    return np.array([[xl, yt], [xr, yt], [xr, yb], [xl, yb]], dtype=np.float64)


@dataclass(frozen=True)
class Pattern:
    name: str
    cell: int  # 1 セルの一辺 (画面の画素)
    colors: int  # 色の数 (2 = 白黒、8 = RGB の各チャネルを 0/255)

    @property
    def grid(self) -> tuple[int, int]:
        return DATA_W // self.cell, DATA_H // self.cell

    def symbols(self) -> np.ndarray:
        """セルごとの記号 (行 × 列)。白黒は 0/7 (黒/白)、8 色は 0〜7 (ビット 0=R, 1=G, 2=B)。"""
        gw, gh = self.grid
        rng = np.random.default_rng(hash_seed(self.name))
        if self.colors == 2:
            return rng.integers(0, 2, size=(gh, gw), dtype=np.uint8) * 7
        return rng.integers(0, 8, size=(gh, gw), dtype=np.uint8)

    def image(self) -> np.ndarray:
        """画面に出す画像 (高さ × 幅 × 3、RGB)。"""
        img = np.full((SCREEN_H, SCREEN_W, 3), 255, dtype=np.uint8)
        sym = self.symbols()
        rgb = symbol_rgb(sym)
        big = np.repeat(np.repeat(rgb, self.cell, axis=0), self.cell, axis=1)
        img[DATA_Y:DATA_Y + big.shape[0], DATA_X:DATA_X + big.shape[1]] = big
        for cx, cy in fiducial_centers():
            x0, y0 = int(cx - FID / 2), int(cy - FID / 2)
            img[y0:y0 + FID, x0:x0 + FID] = 0
        return img


# --- 画面までの経路で色が間引かれていないかを見る縞 ---
#
# 画面への信号が YCbCr 4:2:2 / 4:2:0 だと、色は横 (4:2:0 なら縦も) 2 画素でひとまとめになる。
# 赤と青の 2 画素幅の縞を、その 2 画素の区切りに合わせたものと 1 画素ずらしたものとで出すと、
# 間引かれている経路では、ずらしたほうだけ色が混ざって紫一色になる (明るさの縞は残る)。
# 4 つの区画: [横に並ぶ縞・区切りに合う, 横に並ぶ縞・1 画素ずらす,
#              縦に並ぶ縞・区切りに合う, 縦に並ぶ縞・1 画素ずらす]
STRIPE_PATCHES = ["x_aligned", "x_shifted", "y_aligned", "y_shifted"]
STRIPE_INSET = 60


def stripe_patch_rects() -> list[tuple[int, int, int, int]]:
    """縞の区画 4 つ (x, y, 幅, 高さ。画面の画素)。データ領域を 2x2 に割り、内側に寄せる。"""
    hw, hh = DATA_W // 2, DATA_H // 2
    return [(DATA_X + qx * hw + STRIPE_INSET, DATA_Y + qy * hh + STRIPE_INSET,
             hw - 2 * STRIPE_INSET, hh - 2 * STRIPE_INSET)
            for qy in range(2) for qx in range(2)]


def stripes_image() -> np.ndarray:
    img = np.full((SCREEN_H, SCREEN_W, 3), 255, dtype=np.uint8)
    yy, xx = np.mgrid[0:SCREEN_H, 0:SCREEN_W]
    red, blue = np.array([255, 0, 0], np.uint8), np.array([0, 0, 255], np.uint8)
    phase = [(xx // 2) % 2, ((xx + 1) // 2) % 2, (yy // 2) % 2, ((yy + 1) // 2) % 2]
    for (x, y, w, h), ph in zip(stripe_patch_rects(), phase):
        sub = ph[y:y + h, x:x + w][..., None]
        img[y:y + h, x:x + w] = np.where(sub == 0, red, blue)
    for cx, cy in fiducial_centers():
        x0, y0 = int(cx - FID / 2), int(cy - FID / 2)
        img[y0:y0 + FID, x0:x0 + FID] = 0
    return img

def symbol_rgb(sym: np.ndarray) -> np.ndarray:
    """記号 (0〜7) → RGB (各チャネル 0 か 255)。"""
    return np.stack([(sym >> k & 1) * 255 for k in range(3)], axis=-1).astype(np.uint8)


def hash_seed(name: str) -> int:
    """名前から決まる種 (Python の hash は起動ごとに変わるので使わない)。"""
    return int.from_bytes(name.encode(), "little") % (2**32)


PATTERNS = {
    p.name: p
    for p in [
        Pattern("bw_c1", 1, 2),
        Pattern("bw_c2", 2, 2),
        Pattern("bw_c3", 3, 2),
        Pattern("rgb8_c2", 2, 8),
        Pattern("rgb8_c3", 3, 8),
        Pattern("rgb8_c4", 4, 8),
    ]
}

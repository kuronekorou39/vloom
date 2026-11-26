"""撮った画像 (Y・U・V) と模様の答えを突き合わせ、セルの読み誤りを数える。

    uv run --group dev python tools/color_probe/analyze.py rgb8_c2 path/to/yuv_1_2560x1920.bin ...

手順:
  1. 四隅の黒い目印を探し、画面 → カメラ画像の射影を決める
  2. データ領域を小さな升目に分け、升目ごとに明るさの相関が最大になるずれを探す
     (レンズの歪みは射影だけでは合わない。vcode のスキャナも同じことをしている)
  3. セルの中心で Y・U・V を読み、2 通りの方法で記号を当てる
     - centroid: 記号ごとの平均色にいちばん近いものを選ぶ (素朴な読み方)
     - equalizer: 周り 8 セルも見る線形の判別 (隣のにじみを打ち消す。うまく作った読み方の目安)
     どちらも升目の半分で学習し、残りの半分で誤りを数える

「答えを使って位置を合わせ、同じ画像で学習する」ので、実際の受信より甘い数字が出る。
白黒の模様も同じ手順にかけて、色との差を見るための道具。
"""
from __future__ import annotations

import argparse
import pathlib
import re

import numpy as np

from patterns import DATA_X, DATA_Y, PATTERNS, Pattern, fiducial_centers, symbol_rgb

# vcode の 1 ブロックのセル数。全部合って初めて CRC が通る
BLOCK_CELLS = 400

# 升目の一辺 (画面の画素)。この範囲ではレンズの歪みを平行移動とみなす
TILE_SCREEN_PX = 48

# 升目ごとのずれの探索範囲 (カメラ画像の画素) と刻み
SEARCH_RANGE = 6
FINE_STEP = 0.25


def load_yuv(path: pathlib.Path) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    m = re.search(r"_(\d+)x(\d+)\.bin$", path.name)
    w, h = int(m.group(1)), int(m.group(2))
    b = np.fromfile(path, dtype=np.uint8).astype(np.float32)
    y = b[: w * h].reshape(h, w)
    q = (w // 2) * (h // 2)
    u = b[w * h: w * h + q].reshape(h // 2, w // 2)
    v = b[w * h + q: w * h + 2 * q].reshape(h // 2, w // 2)
    return y, u, v


def bilinear(img: np.ndarray, x: np.ndarray, y: np.ndarray) -> np.ndarray:
    h, w = img.shape
    x = np.clip(x, 0, w - 1.001)
    y = np.clip(y, 0, h - 1.001)
    x0, y0 = np.floor(x).astype(np.int64), np.floor(y).astype(np.int64)
    fx, fy = x - x0, y - y0
    return (img[y0, x0] * (1 - fx) * (1 - fy) + img[y0, x0 + 1] * fx * (1 - fy)
            + img[y0 + 1, x0] * (1 - fx) * fy + img[y0 + 1, x0 + 1] * fx * fy)


def box_mean(img: np.ndarray, k: int) -> np.ndarray:
    """一辺 2k+1 の箱の平均 (端は詰める)。"""
    ii = np.pad(img, ((1, 0), (1, 0))).cumsum(0).cumsum(1)
    h, w = img.shape
    ys, xs = np.arange(h), np.arange(w)
    y0, y1 = np.clip(ys - k, 0, h), np.clip(ys + k + 1, 0, h)
    x0, x1 = np.clip(xs - k, 0, w), np.clip(xs + k + 1, 0, w)
    s = ii[y1][:, x1] - ii[y0][:, x1] - ii[y1][:, x0] + ii[y0][:, x0]
    return s / ((y1 - y0)[:, None] * (x1 - x0)[None, :])


# 目印の探し方の寸法は、幅 2560 の画像で合わせてある。ほかの解像度はこれに対する比で伸縮する
REFERENCE_WIDTH = 2560


def find_fiducials(y: np.ndarray) -> np.ndarray:
    """白地の上の黒い正方形 4 つの中心 (左上・右上・右下・左下)。"""
    scale = y.shape[1] / REFERENCE_WIDTH
    ds = max(1, round(4 * scale))
    small = y[::ds, ::ds]
    # 「周りが明るく、中が暗い」場所。データ領域は平均すると中間の明るさなので引っかからない
    inner, outer = box_mean(small, 8), box_mean(small, 22)
    score = (outer - inner) - inner * 0.5
    h, w = small.shape
    out = []
    for qy, qx in [(0, 0), (0, 1), (1, 1), (1, 0)]:
        sub = score[qy * h // 2:(qy + 1) * h // 2, qx * w // 2:(qx + 1) * w // 2]
        iy, ix = np.unravel_index(np.argmax(sub), sub.shape)
        cx, cy = (ix + qx * w // 2) * ds, (iy + qy * h // 2) * ds
        # 暗い画素の重心で詰める。窓は目印より少し広い程度にして、隣のデータ領域を入れない
        for _ in range(4):
            r = round(58 * scale)
            x0, y0 = int(cx) - r, int(cy) - r
            win = y[max(0, y0):y0 + 2 * r, max(0, x0):x0 + 2 * r]
            dark = win < (win.min() + win.max()) / 2
            yy, xx = np.nonzero(dark)
            cx, cy = max(0, x0) + xx.mean(), max(0, y0) + yy.mean()
        out.append((cx, cy))
    return np.array(out)


def homography(src: np.ndarray, dst: np.ndarray) -> np.ndarray:
    a = []
    for (x, y), (u, v) in zip(src, dst):
        a.append([x, y, 1, 0, 0, 0, -u * x, -u * y, -u])
        a.append([0, 0, 0, x, y, 1, -v * x, -v * y, -v])
    _, _, vt = np.linalg.svd(np.array(a))
    return vt[-1].reshape(3, 3)


def project(hm: np.ndarray, x: np.ndarray, y: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    d = hm[2, 0] * x + hm[2, 1] * y + hm[2, 2]
    return (hm[0, 0] * x + hm[0, 1] * y + hm[0, 2]) / d, (hm[1, 0] * x + hm[1, 1] * y + hm[1, 2]) / d


def corr(a: np.ndarray, b: np.ndarray) -> float:
    a, b = a - a.mean(), b - b.mean()
    d = np.sqrt((a * a).sum() * (b * b).sum())
    return float((a * b).sum() / d) if d > 0 else 0.0


def locate_cells(pat: Pattern, y: np.ndarray) -> tuple[np.ndarray, np.ndarray, np.ndarray, np.ndarray]:
    """セルの中心のカメラ座標 (行 × 列) と、升目ごとの相関・升目番号を返す。"""
    hm = homography(fiducial_centers(), find_fiducials(y))
    gw, gh = pat.grid
    cc, rr = np.meshgrid(np.arange(gw), np.arange(gh))
    px, py = project(hm, DATA_X + (cc + 0.5) * pat.cell, DATA_Y + (rr + 0.5) * pat.cell)
    rgb = symbol_rgb(pat.symbols()).astype(np.float32)
    luma = 0.299 * rgb[..., 0] + 0.587 * rgb[..., 1] + 0.114 * rgb[..., 2]

    t = max(4, TILE_SCREEN_PX // pat.cell)
    tiles_y, tiles_x = -(-gh // t), -(-gw // t)
    off = np.zeros((tiles_y, tiles_x, 2))
    quality = np.zeros((tiles_y, tiles_x))
    tile_id = (rr // t) * tiles_x + (cc // t)
    reach = round(SEARCH_RANGE * y.shape[1] / REFERENCE_WIDTH)
    coarse = np.arange(-reach, reach + 1, 1.0)
    fine = np.arange(-1, 1 + FINE_STEP, FINE_STEP)
    for ty in range(tiles_y):
        for tx in range(tiles_x):
            sl = (slice(ty * t, (ty + 1) * t), slice(tx * t, (tx + 1) * t))
            want, x0, y0 = luma[sl].ravel(), px[sl].ravel(), py[sl].ravel()
            # 隣の升目のずれから探し始める (歪みはなめらかに変わる)
            base = off[ty, tx - 1] if tx > 0 else off[ty - 1, tx] if ty > 0 else np.zeros(2)
            best = (-2.0, base[0], base[1])
            for steps in (coarse, fine):
                cx, cy = best[1], best[2]
                for dy in steps:
                    for dx in steps:
                        c = corr(want, bilinear(y, x0 + cx + dx, y0 + cy + dy))
                        if c > best[0]:
                            best = (c, cx + dx, cy + dy)
            quality[ty, tx] = best[0]
            off[ty, tx] = best[1:]
            px[sl] += best[1]
            py[sl] += best[2]
    return px, py, quality, tile_id


def cell_features(px: np.ndarray, py: np.ndarray, yuv, tile_id: np.ndarray, neighbors: bool) -> np.ndarray:
    """セルごとの特徴量。中心の Y・U・V (升目ごとに平均 0・分散 1 に揃える) と、必要なら周り 8 セル。"""
    y, u, v = yuv
    # 色差は縦横半分で、標本点は輝度 2 画素の真ん中にある
    planes = [bilinear(y, px, py), bilinear(u, (px - 0.5) / 2, (py - 0.5) / 2),
              bilinear(v, (px - 0.5) / 2, (py - 0.5) / 2)]
    n_tiles = tile_id.max() + 1
    normed = []
    for p in planes:
        # 明るさのむらや周辺減光を、答えを使わずに升目の統計だけで均す
        s = np.bincount(tile_id.ravel(), p.ravel(), n_tiles)
        s2 = np.bincount(tile_id.ravel(), (p * p).ravel(), n_tiles)
        n = np.bincount(tile_id.ravel(), minlength=n_tiles)
        mean = s / n
        std = np.sqrt(np.maximum(s2 / n - mean * mean, 1e-6))
        normed.append((p - mean[tile_id]) / std[tile_id])
    if not neighbors:
        return np.stack(normed, -1).reshape(-1, 3)
    feats = []
    for p in normed:
        pad = np.pad(p, 1, mode="edge")
        h, w = p.shape
        for dy in range(3):
            for dx in range(3):
                feats.append(pad[dy:dy + h, dx:dx + w])
    return np.stack(feats, -1).reshape(-1, 27)


def evaluate(pat: Pattern, path: pathlib.Path) -> dict:
    yuv = load_yuv(path)
    px, py, quality, tile_id = locate_cells(pat, yuv[0])
    sym = pat.symbols().ravel()
    bits = np.stack([(sym >> k & 1) for k in range(3)], -1).astype(np.float32) * 2 - 1
    # 升目を市松に分け、片方で学習してもう片方で数える
    tiles_x = int(tile_id[0].max()) + 1
    test = (((tile_id // tiles_x) + (tile_id % tiles_x)) % 2 == 1).ravel()
    train = ~test
    out = {"file": path.name, "cells": int(test.sum()), "align": float(np.median(quality)),
           "white": float(np.percentile(yuv[0], 99)), "black": float(np.percentile(yuv[0], 1))}

    # centroid: 記号ごとの平均色にいちばん近いもの
    f3 = cell_features(px, py, yuv, tile_id, neighbors=False)
    labels = np.unique(sym)
    cents = np.stack([f3[train & (sym == s)].mean(0) for s in labels])
    d = ((f3[test][:, None, :] - cents[None]) ** 2).sum(-1)
    out["ser_centroid"] = float((labels[d.argmin(1)] != sym[test]).mean())

    # equalizer: 周り 8 セルも見る線形の判別 (ビットごとに最小二乗)
    f27 = cell_features(px, py, yuv, tile_id, neighbors=True)
    x = np.hstack([f27, np.ones((len(f27), 1), np.float32)])
    used = [0] if pat.colors == 2 else [0, 1, 2]
    wrong = np.zeros((int(test.sum()), 3), bool)
    for k in used:
        wgt, *_ = np.linalg.lstsq(x[train], bits[train, k], rcond=None)
        wrong[:, k] = np.sign(x[test] @ wgt) != bits[test, k]
    out["ber"] = [float(wrong[:, k].mean()) for k in used]
    out["ser_equalizer"] = float(wrong.any(1).mean())
    if pat.colors == 8:
        # 3 つのうち 2 チャネルだけを使う 4 色の方式の目安 (残りのチャネルが乗ったままなので辛め)
        out["ser_pairs"] = {name: float(wrong[:, list(ks)].any(1).mean())
                            for name, ks in (("RG", (0, 1)), ("RB", (0, 2)), ("GB", (1, 2)))}
    return out


def block_yield(ser: float) -> float:
    return (1 - ser) ** BLOCK_CELLS


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("pattern", choices=sorted(PATTERNS))
    ap.add_argument("files", nargs="+", type=pathlib.Path)
    args = ap.parse_args()
    pat = PATTERNS[args.pattern]
    bits = 1 if pat.colors == 2 else 3
    for f in args.files:
        r = evaluate(pat, f)
        best = min(r["ser_centroid"], r["ser_equalizer"])
        line = (f"{pat.name} {r['file']}: 白 {r['white']:.0f} 黒 {r['black']:.0f} 位置合わせ {r['align']:.2f} "
                f"| 誤り centroid {r['ser_centroid']:.4f} equalizer {r['ser_equalizer']:.4f} "
                f"(ビット別 {' '.join(f'{b:.4f}' for b in r['ber'])}) "
                f"| ブロックが通る割合 {block_yield(best):.3f} → 1 セルあたり {bits * block_yield(best):.2f} ビット")
        if "ser_pairs" in r:
            pairs = " ".join(f"{k} {v:.4f}→{2 * block_yield(v):.2f}bit" for k, v in r["ser_pairs"].items())
            line += f" | 2 チャネルだけ: {pairs}"
        print(line, flush=True)


if __name__ == "__main__":
    main()

//! 実カメラ画像 (グレースケール) から vcode フレームをスキャンする。
//!
//! v0 の前提: UI がガイド枠を表示し、ユーザーがコードを枠内に収める。
//! つまり 4 隅のおおよその位置 (ガイド枠の角) は既知で、スキャナの仕事は
//!   1. 各隅の近傍窓でコーナーマーカーの外角を精密化
//!   2. 4 点からホモグラフィ (セル座標 → 画像座標) を推定
//!   3. セル中心をバイリニアサンプリングし Otsu で二値化
//!   4. 共通デコード経路 (ヘッダ CRC / ブロック CRC / 部分回収) に流す
//! 検出の完全自動化 (ガイドなし) とフレーム間トラッキングは次段。

use crate::{bits_to_bytes, DecodedFrame, FrameError, Layout, CORNER, STRIP_H};

/// 画像座標 (x, y) の 4 点。tl→tr→br→bl の順。
#[derive(Clone, Copy, Debug)]
pub struct Quad {
    pub tl: (f32, f32),
    pub tr: (f32, f32),
    pub br: (f32, f32),
    pub bl: (f32, f32),
}

/// グレースケール画像
pub struct GrayImage<'a> {
    pub w: usize,
    pub h: usize,
    pub data: &'a [u8],
}

impl<'a> GrayImage<'a> {
    pub fn get(&self, x: usize, y: usize) -> u8 {
        self.data[y * self.w + x]
    }

    /// バイリニア補間サンプリング。範囲外は白 (255) 扱い。
    pub fn bilinear(&self, x: f32, y: f32) -> f32 {
        if x < 0.0 || y < 0.0 || x >= (self.w - 1) as f32 || y >= (self.h - 1) as f32 {
            return 255.0;
        }
        let (x0, y0) = (x as usize, y as usize);
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let p00 = self.get(x0, y0) as f32;
        let p10 = self.get(x0 + 1, y0) as f32;
        let p01 = self.get(x0, y0 + 1) as f32;
        let p11 = self.get(x0 + 1, y0 + 1) as f32;
        p00 * (1.0 - fx) * (1.0 - fy) + p10 * fx * (1.0 - fy) + p01 * (1.0 - fx) * fy + p11 * fx * fy
    }
}

/// 3x3 ホモグラフィ行列 (row-major、h33 = 1 に正規化)
#[derive(Clone, Copy, Debug)]
pub struct Homography(pub [f32; 9]);

impl Homography {
    /// (x, y) を射影変換する
    pub fn map(&self, x: f32, y: f32) -> (f32, f32) {
        let m = &self.0;
        let d = m[6] * x + m[7] * y + m[8];
        ((m[0] * x + m[1] * y + m[2]) / d, (m[3] * x + m[4] * y + m[5]) / d)
    }

    /// 4 点対応 (src → dst) から DLT でホモグラフィを求める。
    /// 退化配置 (3 点が同一直線上など) では None。
    pub fn from_quad(src: &[(f32, f32); 4], dst: &[(f32, f32); 4]) -> Option<Self> {
        // 8x9 の同次連立方程式を組み、ガウスの消去法で h11..h32 を解く (h33=1)
        let mut a = [[0.0f64; 9]; 8];
        for i in 0..4 {
            let (x, y) = (src[i].0 as f64, src[i].1 as f64);
            let (u, v) = (dst[i].0 as f64, dst[i].1 as f64);
            a[i * 2] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
            a[i * 2 + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
        }
        // 前進消去 (部分ピボット)
        for col in 0..8 {
            let pivot = (col..8).max_by(|&i, &j| {
                a[i][col].abs().partial_cmp(&a[j][col].abs()).unwrap()
            })?;
            if a[pivot][col].abs() < 1e-9 {
                return None;
            }
            a.swap(col, pivot);
            for row in col + 1..8 {
                let f = a[row][col] / a[col][col];
                for k in col..9 {
                    a[row][k] -= f * a[col][k];
                }
            }
        }
        // 後退代入
        let mut hvec = [0.0f64; 8];
        for row in (0..8).rev() {
            let mut sum = a[row][8];
            for k in row + 1..8 {
                sum -= a[row][k] * hvec[k];
            }
            hvec[row] = sum / a[row][row];
        }
        let mut m = [0.0f32; 9];
        for i in 0..8 {
            m[i] = hvec[i] as f32;
        }
        m[8] = 1.0;
        Some(Self(m))
    }

    /// 逆行列 (随伴行列 / 行列式)。特異なら None。
    pub fn inverse(&self) -> Option<Self> {
        let m = &self.0;
        let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
            + m[2] * (m[3] * m[7] - m[4] * m[6]);
        if det.abs() < 1e-12 {
            return None;
        }
        let adj = [
            m[4] * m[8] - m[5] * m[7],
            m[2] * m[7] - m[1] * m[8],
            m[1] * m[5] - m[2] * m[4],
            m[5] * m[6] - m[3] * m[8],
            m[0] * m[8] - m[2] * m[6],
            m[2] * m[3] - m[0] * m[5],
            m[3] * m[7] - m[4] * m[6],
            m[1] * m[6] - m[0] * m[7],
            m[0] * m[4] - m[1] * m[3],
        ];
        let mut out = [0.0f32; 9];
        for i in 0..9 {
            out[i] = adj[i] / det;
        }
        Some(Self(out))
    }
}

/// 窓内の Otsu 閾値 (ヒストグラム 256 bin)
fn otsu(values: impl Iterator<Item = u8>) -> u8 {
    let mut hist = [0u32; 256];
    let mut n = 0u32;
    for v in values {
        hist[v as usize] += 1;
        n += 1;
    }
    if n == 0 {
        return 128;
    }
    let total_sum: u64 = hist.iter().enumerate().map(|(i, &c)| i as u64 * c as u64).sum();
    // Otsu の t は「クラス 0 (黒) に含まれる最大値」。二分布間に空白帯があると
    // クラス間分散は帯全体で平坦になるため、argmax 区間 [first, last] の中央を採り、
    // 呼び出し側の「v < thr が黒」に合わせて +1 した排他的閾値を返す。
    let (mut first_t, mut last_t, mut best_var) = (128usize, 128usize, -1.0f64);
    let (mut w0, mut sum0) = (0u64, 0u64);
    for t in 0..256 {
        w0 += hist[t] as u64;
        if w0 == 0 {
            continue;
        }
        let w1 = n as u64 - w0;
        if w1 == 0 {
            break;
        }
        sum0 += t as u64 * hist[t] as u64;
        let m0 = sum0 as f64 / w0 as f64;
        let m1 = (total_sum - sum0) as f64 / w1 as f64;
        let var = w0 as f64 * w1 as f64 * (m0 - m1) * (m0 - m1);
        if var > best_var {
            best_var = var;
            first_t = t;
            last_t = t;
        } else if var == best_var {
            last_t = t;
        }
    }
    (((first_t + last_t) / 2) + 1).min(255) as u8
}

// (旧 refine_corner 方式は「窓内で最も隅方向に突き出た黒画素」を拾うため、
//  ブラウザ UI や周辺テキストなどのクラッタを誤認して廃止。
//  現在はガイド枠を初期値に、既知セル一致スコアの粗→細探索で直接合わせる。)

/// スキャン結果 (デコード結果 + 推定ホモグラフィ)
pub struct ScanResult {
    pub frame: DecodedFrame,
    pub homography: Homography,
    /// 精密化後の 4 隅 (画像座標、tl→tr→br→bl)。次フレームのトラッキング初期値に使う。
    pub corners: [(f32, f32); 4],
}

/// コーナーマーカーのセル一覧 (構造が低周波で、粗い位置合わせのスコアに向く)
fn corner_cells(layout: Layout) -> Vec<(usize, usize, bool)> {
    let (w, h) = (layout.width(), layout.height());
    let mut cells = Vec::new();
    for (which, or, oc) in crate::corner_origins(w, h) {
        for r in 0..CORNER {
            for c in 0..CORNER {
                cells.push((or + r, oc + c, crate::corner_black(which, r, c)));
            }
        }
    }
    cells
}

/// コーナー + 上端タイミング行 + 下ストリップの市松。
/// 高周波パターンが上下両側にあることで、水平方向のスケール誤差を拘束する。
fn known_cells(layout: Layout) -> Vec<(usize, usize, bool)> {
    let (w, h) = (layout.width(), layout.height());
    let mut cells = corner_cells(layout);
    for c in CORNER..w - CORNER {
        cells.push((0, c, crate::calib_black(0, c)));
    }
    for r in h - STRIP_H..h {
        for c in CORNER..w - CORNER {
            cells.push((r, c, crate::calib_black(r, c)));
        }
    }
    cells
}

/// ガイド枠 (数十 px ずれていてよい) を初期値として、4 隅を粗→細の座標降下で動かし、
/// 既知セルの一致数を最大化するホモグラフィを求める。
/// 粗いステップ (8/4px) ではコーナーマーカーのみ、細かいステップでは市松も加えて評価する。
/// 周辺クラッタ (ブラウザ UI 等) の影響を受けない: スコアは常にコード内部の既知セルで測る。
fn refine_homography(
    img: &GrayImage,
    corners: &mut [(f32, f32); 4],
    layout: Layout,
    thr: u8,
    coarse_half: i32,
    coarse_step: usize,
) -> Option<Homography> {
    let (wc, hc) = (layout.width() as f32, layout.height() as f32);
    let src = [(0.0, 0.0), (wc, 0.0), (wc, hc), (0.0, hc)];

    // コーナーごとのマーカーセル (quad 順 tl, tr, br, bl に並べ替え)
    let per_corner: Vec<Vec<(usize, usize, bool)>> = {
        let origins = crate::corner_origins(layout.width(), layout.height()); // TL,TR,BL,BR
        [0usize, 1, 3, 2] // quad k → origins index
            .iter()
            .map(|&i| {
                let (which, or, oc) = origins[i];
                let mut cells = Vec::with_capacity(CORNER * CORNER);
                for r in 0..CORNER {
                    for c in 0..CORNER {
                        cells.push((or + r, oc + c, crate::corner_black(which, r, c)));
                    }
                }
                cells
            })
            .collect()
    };

    let score = |quad: &[(f32, f32); 4], cells: &[(usize, usize, bool)]| -> Option<(Homography, usize)> {
        let hm = Homography::from_quad(&src, quad)?;
        let n = cells
            .iter()
            .filter(|&&(r, c, black)| {
                let (x, y) = hm.map(c as f32 + 0.5, r as f32 + 0.5);
                (img.bilinear(x, y) < thr as f32) == black
            })
            .count();
        Some((hm, n))
    };

    // 粗探索: 各コーナーを独立に全数探索 (±coarse_half px, coarse_step px 刻み)。
    // 評価はそのコーナーのマーカーセルのみ。全数なのでマーカーの自己相似による
    // 局所最適に捕まらない。
    //
    // ラウンド数は 1。以前は 2 ラウンド回してコーナー間の相互作用を収束させていたが、
    // 実機ではこの探索が 1 フレーム 300ms に達し、カメラが 32fps で進むため
    // 「探索している間に 10 フレーム進んで位置がずれ、また探索し直す」悪循環に陥る。
    // 残差は後段の座標降下が吸収するので、速さを優先する。
    // 通常受信は ±48/3 (実機で可視ガイド枠に手持ちで合わせると数十 px の構図ずれが常に出るため)。
    // acquire (位置合わせ) は ±96/6 に広げ、多位置 sweep と併せて画面外れ・傾きを取得する。
    // 刻みを大きくして評価数は据え置き、半セル未満の残差は後段の座標降下が吸収する。
    for _ in 0..1 {
        for k in 0..4 {
            let base = corners[k];
            let mut best_n = match score(corners, &per_corner[k]) {
                Some((_, n)) => n,
                None => 0,
            };
            for dy in (-coarse_half..=coarse_half).step_by(coarse_step) {
                for dx in (-coarse_half..=coarse_half).step_by(coarse_step) {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let mut cand = *corners;
                    cand[k] = (base.0 + dx as f32, base.1 + dy as f32);
                    if let Some((_, n)) = score(&cand, &per_corner[k]) {
                        if n > best_n {
                            best_n = n;
                            corners[k] = cand[k];
                        }
                    }
                }
            }
        }
    }

    // 微調整: 全既知セル (コーナー + 擬似ランダム較正) で座標降下
    descend(img, corners, layout, thr, &[2.0, 1.0, 0.5])
}

/// 4 隅を指定ステップ列の座標降下で微調整する (全既知セルの一致数を最大化)。
/// フル探索の微調整段と、トラッキング時の追従の両方で使う。
fn descend(
    img: &GrayImage,
    corners: &mut [(f32, f32); 4],
    layout: Layout,
    thr: u8,
    steps: &[f32],
) -> Option<Homography> {
    let (wc, hc) = (layout.width() as f32, layout.height() as f32);
    let src = [(0.0, 0.0), (wc, 0.0), (wc, hc), (0.0, hc)];
    let fine = known_cells(layout);

    let score = |quad: &[(f32, f32); 4]| -> Option<(Homography, usize)> {
        let hm = Homography::from_quad(&src, quad)?;
        let n = fine
            .iter()
            .filter(|&&(r, c, black)| {
                let (x, y) = hm.map(c as f32 + 0.5, r as f32 + 0.5);
                (img.bilinear(x, y) < thr as f32) == black
            })
            .count();
        Some((hm, n))
    };

    let mut best_h = None;
    for &step in steps {
        for _ in 0..2 {
            for k in 0..4 {
                let base = corners[k];
                let mut best = score(corners)?;
                for dy in -2i32..=2 {
                    for dx in -2i32..=2 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let mut cand = *corners;
                        cand[k] = (base.0 + dx as f32 * step, base.1 + dy as f32 * step);
                        if let Some((hm, n)) = score(&cand) {
                            if n > best.1 {
                                best = (hm, n);
                                corners[k] = cand[k];
                            }
                        }
                    }
                }
                best_h = Some(best.0);
            }
        }
    }
    best_h
}

/// 構造セル (上下ストリップ = コーナー/ヘッダ/較正) のサンプル値から Otsu 閾値を求める。
/// データ領域は輝度多値 (2bit) の場合に中間グレーを含むため、二値化閾値の推定には使わない。
fn threshold_for(img: &GrayImage, hm: &Homography, layout: Layout) -> u8 {
    let (w, h) = (layout.width(), layout.height());
    let rows = (0..STRIP_H).chain(h - STRIP_H..h);
    let values: Vec<u8> = rows
        .flat_map(|r| (0..w).map(move |c| (r, c)))
        .map(|(r, c)| {
            let (x, y) = hm.map(c as f32 + 0.5, r as f32 + 0.5);
            img.bilinear(x, y).round().clamp(0.0, 255.0) as u8
        })
        .collect();
    otsu(values.iter().copied())
}

/// ガイド枠 (おおよその 4 隅) を頼りに、グレースケール画像から vcode フレームをスキャンする。
///
/// layout はガイド段階では未知のヘッダ内容に先立ってセル格子を張るためのヒント。
/// ヘッダの実レイアウトと一致しなければ LayoutMismatch を返す。
pub fn scan_frame(
    img: &GrayImage,
    guide: &Quad,
    layout: Layout,
) -> Result<ScanResult, FrameError> {
    // 通常受信: 中央ガイド枠付近を ±48px で探索
    scan_frame_ranged(img, guide, layout, 48, 3)
}

/// 位置合わせ (acquire) 用の広域版。コーナー粗探索を ±96px に広げ、呼び出し側の
/// 多位置 sweep と併せて、画面中央から外れた/傾いたコードでも初回取得できるようにする。
/// 通常受信 (scan_frame) より重いので、固定後の一回きりの取得にのみ使う。
pub fn scan_frame_wide(
    img: &GrayImage,
    guide: &Quad,
    layout: Layout,
) -> Result<ScanResult, FrameError> {
    scan_frame_ranged(img, guide, layout, 96, 6)
}

fn scan_frame_ranged(
    img: &GrayImage,
    guide: &Quad,
    layout: Layout,
    coarse_half: i32,
    coarse_step: usize,
) -> Result<ScanResult, FrameError> {
    let (wc, hc) = (layout.width() as f32, layout.height() as f32);

    // ガイド枠をそのまま初期 4 隅とする (粗→細探索が数十 px のずれを吸収する)
    let mut corners = [guide.tl, guide.tr, guide.br, guide.bl];
    let hmat0 = Homography::from_quad(
        &[(0.0, 0.0), (wc, 0.0), (wc, hc), (0.0, hc)],
        &corners,
    )
    .ok_or(FrameError::CornerMismatch)?;
    let thr0 = threshold_for(img, &hmat0, layout);

    // 既知パターン (コーナー + 擬似ランダム較正) への一致を最大化するよう 4 隅を微調整
    let hmat = refine_homography(img, &mut corners, layout, thr0, coarse_half, coarse_step)
        .ok_or(FrameError::CornerMismatch)?;

    decode_at(img, hmat, layout)
}

/// 前フレームで成功した 4 隅を初期値に、粗探索なしの座標降下だけで追従スキャンする。
/// 手持ちのフレーム間変位 (数 px) を吸収する。大きく外れた場合はエラーを返すので、
/// 呼び出し側は scan_frame (フル探索) にフォールバックすること。
pub fn scan_frame_tracked(
    img: &GrayImage,
    prev_corners: &[(f32, f32); 4],
    layout: Layout,
) -> Result<ScanResult, FrameError> {
    let (wc, hc) = (layout.width() as f32, layout.height() as f32);
    let mut corners = *prev_corners;
    let hmat0 = Homography::from_quad(
        &[(0.0, 0.0), (wc, 0.0), (wc, hc), (0.0, hc)],
        &corners,
    )
    .ok_or(FrameError::CornerMismatch)?;
    let thr0 = threshold_for(img, &hmat0, layout);
    // 60fps 処理予算 (16ms) に収めるため探索ステップは 2 段に抑える。
    // 高フレームレートではフレーム間変位が数 px なのでこれで十分追従できる。
    let hmat = descend(img, &mut corners, layout, thr0, &[2.0, 0.5])
        .ok_or(FrameError::CornerMismatch)?;
    decode_at(img, hmat, layout)
}

/// 確定したホモグラフィでフレームをデコードする (コーナー照合 + ヘッダ + ブロック部分回収)
fn decode_at(
    img: &GrayImage,
    hmat: Homography,
    layout: Layout,
) -> Result<ScanResult, FrameError> {
    let (w, h) = (layout.width(), layout.height());
    let thr = threshold_for(img, &hmat, layout) as f32;

    // セル (row+dy, col+dx) の生グレー値 / 二値 (dx, dy はサブセルオフセット)
    let sample_raw = |r: usize, c: usize, dx: f32, dy: f32| -> f32 {
        let (x, y) = hmat.map(c as f32 + 0.5 + dx, r as f32 + 0.5 + dy);
        img.bilinear(x, y)
    };
    let sample = |r: usize, c: usize, dx: f32, dy: f32| -> bool { sample_raw(r, c, dx, dy) < thr };

    // 四隅マーカーの照合 (オフセットなし)
    let mut matched = 0usize;
    let corner_total = 4 * CORNER * CORNER;
    for (which, or, oc) in crate::corner_origins(w, h) {
        for r in 0..CORNER {
            for c in 0..CORNER {
                if sample(or + r, oc + c, 0.0, 0.0) == crate::corner_black(which, r, c) {
                    matched += 1;
                }
            }
        }
    }
    if (matched as f32) < crate::CORNER_MATCH_MIN * corner_total as f32 {
        return Err(FrameError::CornerMismatch);
    }

    // 残留する半セル級の系統ずれを、CRC を正解判定器としたサブセルオフセット
    // リトライで領域ごとに吸収する (ヘッダ/各ブロックで独立に最良オフセットを探す)。
    // 中心から近い順の 5x5 格子 (±0.5 セル)。
    const STEPS: [f32; 5] = [0.0, 0.25, -0.25, 0.5, -0.5];
    let offs: Vec<(f32, f32)> = STEPS
        .iter()
        .flat_map(|&dy| STEPS.iter().map(move |&dx| (dx, dy)))
        .collect();

    // ヘッダ: 各オフセット x 各コピーで最初に CRC が通ったものを採用
    let hdr_cells: Vec<(usize, usize)> = crate::header_cells(w).collect();
    let copy_bits = crate::HEADER_LEN * 8;
    let header = offs
        .iter()
        .find_map(|&(dx, dy)| {
            let bits: Vec<bool> = hdr_cells.iter().map(|&(r, c)| sample(r, c, dx, dy)).collect();
            (0..bits.len() / copy_bits).find_map(|k| {
                let bytes = bits_to_bytes(&bits[k * copy_bits..(k + 1) * copy_bits]);
                crate::FrameHeader::deserialize(&bytes)
            })
        })
        .ok_or(FrameError::HeaderNotFound)?;
    let bpc = header.bits_per_cell;
    if header.layout != layout || !(bpc == 1 || bpc == 2) {
        return Err(FrameError::LayoutMismatch);
    }

    // 輝度 4 値 (bpc=2) 用の局所較正: 上端タイミング行と下ストリップの既知白黒セルから
    // 列ごとの黒/白レベルを推定し、データセルは上下の推定値を行位置で線形補間して正規化する。
    // 照明勾配・ガンマ・露出の空間変化を吸収する (Phase 0 の較正の教訓)。
    let calib = if bpc == 2 {
        let estimate = |rows: &[usize]| -> (Vec<f32>, Vec<f32>) {
            let mut blk: Vec<Vec<f32>> = vec![Vec::new(); w];
            let mut wht: Vec<Vec<f32>> = vec![Vec::new(); w];
            for &r in rows {
                for c in CORNER..w - CORNER {
                    let v = sample_raw(r, c, 0.0, 0.0);
                    if crate::calib_black(r, c) {
                        blk[c].push(v);
                    } else {
                        wht[c].push(v);
                    }
                }
            }
            let smooth = |acc: &[Vec<f32>]| -> Vec<f32> {
                (0..w)
                    .map(|c| {
                        let cc = c.clamp(CORNER, w - CORNER - 1);
                        for win in [6usize, 16, w] {
                            let lo = cc.saturating_sub(win).max(CORNER);
                            let hi = (cc + win + 1).min(w - CORNER);
                            let (mut sum, mut n) = (0.0f32, 0u32);
                            for k in lo..hi {
                                for &v in &acc[k] {
                                    sum += v;
                                    n += 1;
                                }
                            }
                            if n > 0 {
                                return sum / n as f32;
                            }
                        }
                        0.0
                    })
                    .collect()
            };
            (smooth(&blk), smooth(&wht))
        };
        let (black_top, white_top) = estimate(&[0]);
        let bot_rows: Vec<usize> = (h - STRIP_H..h).collect();
        let (black_bot, white_bot) = estimate(&bot_rows);
        Some((black_top, white_top, black_bot, white_bot))
    } else {
        None
    };

    // 正規化した輝度からレベル (0..4) を判定
    let quantize = |r: usize, c: usize, dx: f32, dy: f32| -> u8 {
        let (black_top, white_top, black_bot, white_bot) = calib.as_ref().unwrap();
        let v = sample_raw(r, c, dx, dy);
        let t = r as f32 / (h - 1) as f32;
        let black = black_top[c] * (1.0 - t) + black_bot[c] * t;
        let white = white_top[c] * (1.0 - t) + white_bot[c] * t;
        let norm = (v - black) / (white - black).max(1.0);
        match norm {
            x if x < 1.0 / 6.0 => 0,
            x if x < 0.5 => 1,
            x if x < 5.0 / 6.0 => 2,
            _ => 3,
        }
    };

    // ブロック: 同様にオフセットリトライ付きで CRC が通ったものだけ回収
    let blocks = (0..layout.block_count())
        .map(|bi| {
            let (or, oc) = layout.block_origin(bi);
            offs.iter().find_map(|&(dx, dy)| {
                let mut bits = Vec::with_capacity(layout.block * layout.block * bpc as usize);
                for i in 0..layout.block * layout.block {
                    let (r, c) = (or + i / layout.block, oc + i % layout.block);
                    if bpc == 1 {
                        bits.push(sample(r, c, dx, dy));
                    } else {
                        let level = quantize(r, c, dx, dy);
                        bits.push(level & 2 != 0);
                        bits.push(level & 1 != 0);
                    }
                }
                let bytes = bits_to_bytes(&bits);
                let (payload, crc) = bytes.split_at(layout.block_payload_len(bpc));
                if crate::crc32(payload) == u32::from_be_bytes([crc[0], crc[1], crc[2], crc[3]]) {
                    Some(payload.to_vec())
                } else {
                    None
                }
            })
        })
        .collect();

    let (wc, hc) = (w as f32, h as f32);
    Ok(ScanResult {
        frame: DecodedFrame { header, blocks },
        corners: [
            hmat.map(0.0, 0.0),
            hmat.map(wc, 0.0),
            hmat.map(wc, hc),
            hmat.map(0.0, hc),
        ],
        homography: hmat,
    })
}

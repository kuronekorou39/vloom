//! コーナーマーカーの直接検出。
//!
//! 位置合わせはこれまで「テクスチャから大まかな位置を推定 → ガイド枠から粗探索」で
//! 始めていたが、推定が外れると (実機で幅 910 のコードを 1024×960 と見積もった)
//! 粗探索の許容 ±48px に入らず、掴めない。前フレームの四隅に頼る追従も、
//! 手持ちで大きく動くと切れる。
//!
//! マーカーは一辺 24 セルの黒い四角で、コードの中で最も大きく最も単純な構造なので、
//! 縮小画像から直接探せる。外周が黒・周囲が白 (セパレータ/余白) という共通の形で
//! 候補を拾い、内部の模様で 4 種を見分ける。4 つ見つかれば四隅の座標がそのまま
//! 出るので、前フレームにも縮尺の仮定にも依存せず、毎フレーム掴み直せる。
//! 模様で種別が決まるため、コードが回転していても正しい順で四隅が出る。
//!
//! 計算は 1/4 縮小画像の積分画像で箱の平均を O(1) に取り、大きさ 8 段階 ×
//! 位置 2〜3px 刻みを総当たりする (300×400 で数十万回の箱評価、数 ms)。

use crate::scan::{GrayImage, Quad};
use crate::CORNER;

/// 縮小率。1200×1600 → 300×400。マーカー (実機で 60〜200px) は 15〜50px になる
const DOWN: usize = 4;

/// 縮小画像上で試すマーカーの一辺 (px)。実機の 2.5〜8 px/セル に相当する範囲
const SIZES: [usize; 11] = [12, 14, 16, 18, 21, 24, 27, 31, 36, 42, 48];

/// 各種別で残す候補数。偽陽性 (タスクバーのアイコン等) があっても、4 つの組み合わせで
/// 幾何的に整合するものを選べるように複数残す
const KEEP: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy, Debug)]
struct Cand {
    kind: Kind,
    /// 縮小画像上の左上と一辺
    x: usize,
    y: usize,
    s: usize,
    /// 模様の不一致 (小さいほど良い)
    err: f32,
    /// 外周の明るさ / 周囲の明るさ。マーカーより大きい箱は外周に白が混ざって上がる
    ring: f32,
}

/// 縮小画像の積分画像
struct Integral {
    w: usize,
    h: usize,
    /// (w+1)×(h+1)、行優先
    acc: Vec<u32>,
}

impl Integral {
    fn build(img: &GrayImage) -> Integral {
        let (w, h) = (img.w / DOWN, img.h / DOWN);
        let mut acc = vec![0u32; (w + 1) * (h + 1)];
        for y in 0..h {
            let mut row = 0u32;
            for x in 0..w {
                // DOWN×DOWN の箱平均で縮小する
                let mut sum = 0u32;
                for dy in 0..DOWN {
                    let base = (y * DOWN + dy) * img.w + x * DOWN;
                    for dx in 0..DOWN {
                        sum += img.data[base + dx] as u32;
                    }
                }
                row += sum / (DOWN * DOWN) as u32;
                acc[(y + 1) * (w + 1) + (x + 1)] = acc[y * (w + 1) + (x + 1)] + row;
            }
        }
        Integral { w, h, acc }
    }

    /// [x0,x1)×[y0,y1) の平均輝度。範囲外は None
    fn mean(&self, x0: isize, y0: isize, x1: isize, y1: isize) -> Option<f32> {
        if x0 < 0 || y0 < 0 || x1 as usize > self.w || y1 as usize > self.h || x1 <= x0 || y1 <= y0 {
            return None;
        }
        let (x0, y0, x1, y1) = (x0 as usize, y0 as usize, x1 as usize, y1 as usize);
        let w1 = self.w + 1;
        let s = self.acc[y1 * w1 + x1] + self.acc[y0 * w1 + x0]
            - self.acc[y0 * w1 + x1]
            - self.acc[y1 * w1 + x0];
        Some(s as f32 / ((x1 - x0) * (y1 - y0)) as f32)
    }
}

/// 種別ごとの候補 (縮小画像座標: x, y, 一辺, 不一致度)。調査用。
pub fn debug_candidates(img: &GrayImage) -> [Vec<(usize, usize, usize, f32)>; 4] {
    let ig = Integral::build(img);
    let c = collect(&ig);
    let conv = |v: &Vec<Cand>| v.iter().map(|c| (c.x * DOWN, c.y * DOWN, c.s * DOWN, c.err + c.ring)).collect();
    [conv(&c[0]), conv(&c[1]), conv(&c[2]), conv(&c[3])]
}

/// 1 箇所の評価の中身 (調査用): 元画像座標の (x, y, 一辺) を渡す。
/// 戻り: (ring, quiet, 中央, 環, 上, 下, 左, 右) の正規化前の平均と、種別ごとの不一致
pub fn debug_eval(img: &GrayImage, x: usize, y: usize, s: usize) -> String {
    let ig = Integral::build(img);
    let (x, y, s) = (x / DOWN, y / DOWN, s / DOWN);
    let t = (s / 6).max(1);
    let q = (t / 2).max(1);
    let c3 = s / 3;
    let (xi, yi, si, ti, qi) = (x as isize, y as isize, s as isize, t as isize, q as isize);
    let g: isize = 1;
    let m = |a, b, c, d| ig.mean(a, b, c, d).map(|v| v.round()).unwrap_or(-1.0);
    let outer = m(xi + g, yi + g, xi + si - g, yi + si - g);
    let inner = m(xi + ti, yi + ti, xi + si - ti, yi + si - ti);
    let around = m(xi - g - qi, yi - g - qi, xi + si + g + qi, yi + si + g + qi);
    let shell = m(xi - g, yi - g, xi + si + g, yi + si + g);
    let center = m(xi + ((s - c3) / 2) as isize, yi + ((s - c3) / 2) as isize,
                   xi + ((s - c3) / 2) as isize + c3 as isize, yi + ((s - c3) / 2) as isize + c3 as isize);
    let cand = evaluate(&ig, x, y, s, t, q, c3);
    format!("box({x},{y},{s}) t={t} q={q}: outer {outer} inner {inner} around {around} shell {shell} center {center} -> {cand:?}")
}

fn collect(ig: &Integral) -> [Vec<Cand>; 4] {
    let mut cands: [Vec<Cand>; 4] = Default::default();

    for &s in &SIZES {
        if s + 2 >= ig.w.min(ig.h) {
            continue;
        }
        let t = (s / 6).max(1); // 外周の太さ (CORNER/6 セル)
        let q = (t / 2).max(1); // 周囲の白を見る幅 (セパレータ 4 セルの半分)
        let c3 = s / 3; // 中央の四角 (CORNER/3 セル)
        let step = if s <= 24 { 2 } else { 3 };
        let mut y = 0;
        while y + s <= ig.h {
            let mut x = 0;
            while x + s <= ig.w {
                if let Some(c) = evaluate(ig, x, y, s, t, q, c3) {
                    push_cand(&mut cands[c.kind as usize], c);
                }
                x += step;
            }
            y += step;
        }
    }
    cands
}

/// 4 つのマーカーを探し、コードの四隅 (元画像座標、tl→tr→br→bl) を返す。
pub fn locate_markers(img: &GrayImage) -> Option<Quad> {
    if img.w < DOWN * 16 || img.h < DOWN * 16 {
        return None;
    }
    let ig = Integral::build(img);
    let cands = collect(&ig);
    // 4 種それぞれ候補があるか
    if cands.iter().any(|v| v.is_empty()) {
        return None;
    }
    // 幾何的に整合する組み合わせを選ぶ。大きさが揃っていて、四辺の長さが対辺で
    // 揃っていて、角の向きが一貫している (裏返っていない) こと。
    let mut best: Option<(f32, [Cand; 4])> = None;
    for &tl in &cands[0] {
        for &tr in &cands[1] {
            for &bl in &cands[2] {
                for &br in &cands[3] {
                    let set = [tl, tr, br, bl];
                    if let Some(score) = geometry_score(&set) {
                        let total = score + [tl, tr, bl, br].iter().map(|c| c.err + c.ring).sum::<f32>();
                        if best.map_or(true, |(b, _)| total < b) {
                            best = Some((total, set));
                        }
                    }
                }
            }
        }
    }
    let (_, set) = best?;

    // 各マーカーの箱のうち、4 つの中心から最も遠い角がコードの隅。
    // 種別ごとに「左上の角」と決め打ちしないので、コードが回転していても正しい。
    let cx = set.iter().map(|c| c.x as f32 + c.s as f32 / 2.0).sum::<f32>() / 4.0;
    let cy = set.iter().map(|c| c.y as f32 + c.s as f32 / 2.0).sum::<f32>() / 4.0;
    let outer = |c: &Cand| -> (f32, f32) {
        let (x0, y0, x1, y1) = (c.x as f32, c.y as f32, (c.x + c.s) as f32, (c.y + c.s) as f32);
        let corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
        let far = corners
            .iter()
            .max_by(|a, b| {
                let da = (a.0 - cx).powi(2) + (a.1 - cy).powi(2);
                let db = (b.0 - cx).powi(2) + (b.1 - cy).powi(2);
                da.partial_cmp(&db).unwrap()
            })
            .unwrap();
        (far.0 * DOWN as f32, far.1 * DOWN as f32)
    };
    Some(Quad {
        tl: outer(&set[0]),
        tr: outer(&set[1]),
        br: outer(&set[2]),
        bl: outer(&set[3]),
    })
}

/// 位置 (x,y)・一辺 s の箱がマーカーらしいか評価し、種別と不一致度を返す。
fn evaluate(ig: &Integral, x: usize, y: usize, s: usize, t: usize, q: usize, c3: usize) -> Option<Cand> {
    let (xi, yi, si, ti, qi) = (x as isize, y as isize, s as isize, t as isize, q as isize);
    // 箱の縁 1px は見ない (guard)。走査は 2〜3px 刻みなので箱はマーカーに対して
    // 1px ほどずれうる。縁を数えると外周に白が、周囲に黒が混ざって落ちる。
    let g: isize = 1;
    if ti <= g {
        return None;
    }
    let outer = ig.mean(xi + g, yi + g, xi + si - g, yi + si - g)?;
    let inner = ig.mean(xi + ti, yi + ti, xi + si - ti, yi + si - ti)?;
    let around = ig.mean(xi - g - qi, yi - g - qi, xi + si + g + qi, yi + si + g + qi)?;
    let shell = ig.mean(xi - g, yi - g, xi + si + g, yi + si + g)?;
    // 外周 (縁を除いた箱から内側を除いた部分) と周囲 (guard の外の枠) の平均
    let a_outer = ((s as isize - 2 * g).pow(2)) as f32;
    let a_inner = ((s - 2 * t) * (s - 2 * t)) as f32;
    let a_shell = ((s as isize + 2 * g).pow(2)) as f32;
    let a_around = ((s as isize + 2 * g + 2 * qi).pow(2)) as f32;
    let ring = (outer * a_outer - inner * a_inner) / (a_outer - a_inner);
    let quiet = (around * a_around - shell * a_shell) / (a_around - a_shell);
    // 外周が黒く周囲が白い、というマーカー共通の形。コントラストが薄いものと、
    // 外周が本当には黒くないもの (余白とマーカーにまたがる大きな箱は外周が灰色に
    // なる) を弾く。明るさの比で見るので照明の暗い隅でも成り立つ。
    if quiet - ring < 60.0 || ring > quiet * 0.45 {
        return None;
    }
    // 明るさを黒=0・白=1 に正規化する (照明の勾配に依存しないように)
    let norm = |v: f32| ((v - ring) / (quiet - ring)).clamp(0.0, 1.0);
    // 内部の領域: 中央の四角、その周りの環、上半分、下半分
    let c0 = xi + ((s - c3) / 2) as isize;
    let r0 = yi + ((s - c3) / 2) as isize;
    let center = ig.mean(c0, r0, c0 + c3 as isize, r0 + c3 as isize)?;
    let a_center = (c3 * c3) as f32;
    let annulus = (inner * a_inner - center * a_center) / (a_inner - a_center);
    let (midx, midy) = (xi + si / 2, yi + si / 2);
    let top = ig.mean(xi + ti, yi + ti, xi + si - ti, midy)?;
    let bottom = ig.mean(xi + ti, midy, xi + si - ti, yi + si - ti)?;
    let left = ig.mean(xi + ti, yi + ti, midx, yi + si - ti)?;
    let right = ig.mean(midx, yi + ti, xi + si - ti, yi + si - ti)?;
    let (nc, na) = (norm(center), norm(annulus));
    // BR は「半分が黒」。コードが回転していると黒い半分が上下左右のどこにも来る
    // ので、4 通りのうち最も合うものを採る (種別は模様で決まる、という原則を保つ)
    let half = [
        (norm(top), norm(bottom)),
        (norm(bottom), norm(top)),
        (norm(left), norm(right)),
        (norm(right), norm(left)),
    ]
    .iter()
    .map(|&(white, black)| (1.0 - white) + black)
    .fold(f32::INFINITY, f32::min);
    // 各種別の期待値との差。TL: 全部黒 / TR: 内部が白 / BL: 中央だけ黒 / BR: 半分が黒
    let errs = [
        (Kind::TopLeft, nc + na),
        (Kind::TopRight, (1.0 - nc) + (1.0 - na)),
        (Kind::BottomLeft, nc + (1.0 - na)),
        (Kind::BottomRight, half),
    ];
    let (kind, err) = errs
        .iter()
        .copied()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .unwrap();
    // 2 領域の合計で 0.7 まで許す。画面の端は照明の勾配で白が沈み、BL の環が
    // 0.6 程度にしか上がらないことがある (実機)。誤検出は 4 つの組み合わせの
    // 幾何で落とせるので、ここは緩めにしておく
    if err > 0.7 {
        return None;
    }
    Some(Cand { kind, x, y, s, err, ring: ring / quiet })
}

/// 種別ごとの候補に加える。近い位置の候補は良いほうだけ残す (非最大抑制)。
fn push_cand(list: &mut Vec<Cand>, c: Cand) {
    for e in list.iter_mut() {
        let dx = (e.x as f32 + e.s as f32 / 2.0) - (c.x as f32 + c.s as f32 / 2.0);
        let dy = (e.y as f32 + e.s as f32 / 2.0) - (c.y as f32 + c.s as f32 / 2.0);
        if dx * dx + dy * dy < (e.s.max(c.s) as f32 / 2.0).powi(2) {
            // 塗りつぶしのマーカーは内側の小さい箱も条件を満たす (不一致 0) ので、
            // 同点なら大きい箱 = マーカー全体を採る。ただしマーカーより大きい箱は
            // 外周に白が混ざる (ring が上がる) ので、それも点数に入れて弾く。
            let (sc, se) = (c.err + c.ring, e.err + e.ring);
            let better = if (sc - se).abs() < 0.1 { c.s > e.s } else { sc < se };
            if better {
                *e = c;
            }
            return;
        }
    }
    list.push(c);
    // 切り詰めの基準は非最大抑制と同じ「不一致 + 外周の明るさ」。不一致だけで並べると、
    // 縁にまたがる箱 (不一致 0 だが外周が灰色) が本物を押し出してしまう
    list.sort_by(|a, b| (a.err + a.ring).partial_cmp(&(b.err + b.ring)).unwrap());
    list.truncate(KEEP);
}

/// 4 候補 (tl, tr, br, bl) の幾何的な整合性。悪いほど大きい値、不整合なら None。
fn geometry_score(set: &[Cand; 4]) -> Option<f32> {
    let smax = set.iter().map(|c| c.s).max()? as f32;
    let smin = set.iter().map(|c| c.s).min()? as f32;
    if smax / smin > 1.5 {
        return None; // 4 隅で大きさが違いすぎる (遠近では 1.5 倍も違わない)
    }
    let p: Vec<(f32, f32)> = set
        .iter()
        .map(|c| (c.x as f32 + c.s as f32 / 2.0, c.y as f32 + c.s as f32 / 2.0))
        .collect();
    let len = |a: (f32, f32), b: (f32, f32)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
    let (top, bottom) = (len(p[0], p[1]), len(p[3], p[2]));
    let (left, right) = (len(p[0], p[3]), len(p[1], p[2]));
    if top < smax * 2.0 || left < smax * 2.0 {
        return None; // マーカー同士が近すぎる (同じ塊を別種に見ている)
    }
    let ratio = |a: f32, b: f32| (a.max(b) / a.min(b)) - 1.0;
    if ratio(top, bottom) > 0.35 || ratio(left, right) > 0.35 {
        return None;
    }
    // 角の向き: tl→tr と tl→bl の外積が 4 隅で同符号 (凸で裏返っていない)
    let cross = |o: (f32, f32), a: (f32, f32), b: (f32, f32)| {
        (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
    };
    let c0 = cross(p[0], p[1], p[3]);
    let c1 = cross(p[1], p[2], p[0]);
    let c2 = cross(p[2], p[3], p[1]);
    let c3 = cross(p[3], p[0], p[2]);
    if !(c0 > 0.0 && c1 > 0.0 && c2 > 0.0 && c3 > 0.0) && !(c0 < 0.0 && c1 < 0.0 && c2 < 0.0 && c3 < 0.0) {
        return None;
    }
    Some(ratio(top, bottom) + ratio(left, right) + (smax / smin - 1.0))
}

/// マーカーの一辺 (セル)。テストと呼び出し側が縮尺の見当を付けるために公開する
pub const MARKER_CELLS: usize = CORNER;

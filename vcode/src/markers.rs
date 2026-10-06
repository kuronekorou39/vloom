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
//! 計算は 2 段。まず 1/4 縮小画像の積分画像で箱の平均を O(1) に取り、大きさ 11 段階 ×
//! 位置 2〜3px 刻みを総当たりして候補を拾う (300×400 で数十万回の箱評価、数 ms)。
//! 次に候補のまわりだけ元の解像度で、位置と大きさを 1px 刻みで合わせ直す。
//!
//! 2 段にしているのは、縮小画像だけでは縮尺によって通らない帯ができるため。白抜きの
//! マーカー (右上) は外周の黒が 4 セルしかなく、縮小画像では 3 画素前後になる。箱の
//! 大きさと位置が粗い刻みに乗らない縮尺 (実機: 約 3.25 px/セル) では、外周に白が、
//! 周囲に隣のデータが混ざって判定を外し、その距離では何枚撮っても掴めなかった。
//! 1 段目は条件を緩めて取りこぼさないことだけを狙い、合否は 2 段目で決める。

use crate::scan::{GrayImage, Quad};
use crate::{Layout, CORNER};

/// 縮小率。1200×1600 → 300×400。マーカー (実機で 60〜200px) は 15〜50px になる
const DOWN: usize = 4;

/// 縮小画像上で試すマーカーの一辺 (px)。実機の 2.5〜8 px/セル に相当する範囲
const SIZES: [usize; 11] = [12, 14, 16, 18, 21, 24, 27, 31, 36, 42, 48];

/// 各種別で残す候補数。偽陽性 (タスクバーのアイコン等) があっても、4 つの組み合わせで
/// 幾何的に整合するものを選べるように複数残す
const KEEP: usize = 5;

/// 1 段目 (縮小画像) で種別ごとに残す候補数。条件を緩めたぶん偽の候補が増えるので、
/// 本物が押し出されないよう 2 段目より多く残す
const COARSE_KEEP: usize = 12;

/// 2 段目で候補のまわりを探す範囲 (元画像の px)。1 段目の刻み (位置 3px × 縮小 4 =
/// 12px、大きさは隣の候補との差の半分) を覆う
const FINE_POS: isize = 8;
const FINE_SIZE: isize = 8;

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

    /// 元の解像度のまま、(x0, y0) から w×h を切り出した積分画像 (画像の外は切り詰める)。
    /// 戻りは (積分画像, 実際の左上 x, y)。
    fn build_crop(img: &GrayImage, x0: isize, y0: isize, w: usize, h: usize) -> (Integral, usize, usize) {
        let (cx, cy) = (x0.max(0) as usize, y0.max(0) as usize);
        let (cx1, cy1) = (
            ((x0 + w as isize).max(0) as usize).min(img.w),
            ((y0 + h as isize).max(0) as usize).min(img.h),
        );
        let (w, h) = (cx1.saturating_sub(cx), cy1.saturating_sub(cy));
        let mut acc = vec![0u32; (w + 1) * (h + 1)];
        for y in 0..h {
            let mut row = 0u32;
            let base = (cy + y) * img.w + cx;
            for x in 0..w {
                row += img.data[base + x] as u32;
                acc[(y + 1) * (w + 1) + (x + 1)] = acc[y * (w + 1) + (x + 1)] + row;
            }
        }
        (Integral { w, h, acc }, cx, cy)
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

/// 種別ごとの候補 (元画像座標: x, y, 一辺, 不一致度)。2 段目を通ったもの。調査用。
pub fn debug_candidates(img: &GrayImage) -> [Vec<(usize, usize, usize, f32)>; 4] {
    let c = refined(img);
    let conv = |v: &Vec<Fine>| {
        v.iter()
            .map(|c| (c.x.round() as usize, c.y.round() as usize, c.s.round() as usize, c.err + c.ring))
            .collect()
    };
    [conv(&c[0]), conv(&c[1]), conv(&c[2]), conv(&c[3])]
}

/// 1 箇所の評価の中身 (調査用): 元画像座標の (x, y, 一辺) を渡す。
/// 1 段目 (縮小画像・緩い条件) と 2 段目 (元の解像度・本来の条件) の判定を並べて返す。
pub fn debug_eval(img: &GrayImage, x: usize, y: usize, s: usize) -> String {
    let ig = Integral::build(img);
    let (dx, dy, ds) = (x / DOWN, y / DOWN, s / DOWN);
    let dt = (ds / 6).max(1);
    let coarse = evaluate(&ig, dx, dy, ds, dt, (dt / 2).max(1), ds / 3, 1, false);
    let m = (s / 2) as isize;
    let side = s + 2 * m as usize;
    let (fig, ox, oy) = Integral::build_crop(img, x as isize - m, y as isize - m, side, side);
    let fine = if x >= ox && y >= oy { evaluate_fine(&fig, x - ox, y - oy, s) } else { None };
    format!("box({x},{y},{s}): 1 段目 {coarse:?} / 2 段目 {fine:?}")
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
                if let Some(c) = evaluate(ig, x, y, s, t, q, c3, 1, false) {
                    push_cand(&mut cands[c.kind as usize], c, COARSE_KEEP);
                }
                x += step;
            }
            y += step;
        }
    }
    cands
}

/// 2 段目を通った候補 (元画像座標)。位置と大きさは 1px より細かく出る
#[derive(Clone, Copy, Debug)]
struct Fine {
    kind: Kind,
    x: f32,
    y: f32,
    s: f32,
    err: f32,
    ring: f32,
}

/// 元の解像度での評価。外周の太さ・周囲の幅・縁の遊びを一辺から決める
/// (縁の遊びは 1 セルぶん。ボケた縁と、箱がマーカーから 1 セル以内ずれるぶんを吸収する)。
fn evaluate_fine(ig: &Integral, x: usize, y: usize, s: usize) -> Option<Cand> {
    let t = ((s + 3) / 6).max(2);
    let g = ((s + 12) / 24).max(1);
    let q = (t / 2).max(1);
    evaluate(ig, x, y, s, t, q, s / 3, g as isize, true)
}

/// 1 段目の候補 1 つを、元の解像度で位置と大きさを合わせ直す。本来の条件を満たす箱が
/// 無ければ None (1 段目が緩めた条件で拾った偽の候補はここで落ちる)。
fn refine(img: &GrayImage, c: &Cand) -> Option<Fine> {
    let (x, y, s) = ((c.x * DOWN) as isize, (c.y * DOWN) as isize, (c.s * DOWN) as isize);
    // 周囲の白まで含めて切り出す (大きさ + 位置の探索幅 + 周囲を見る幅)
    let m = FINE_POS + FINE_SIZE + s / 4 + 2;
    let side = (s + 2 * m) as usize;
    let (ig, ox, oy) = Integral::build_crop(img, x - m, y - m, side, side);
    let eval = |fx: isize, fy: isize, fs: isize| -> Option<(Cand, f32)> {
        let (lx, ly) = (fx - ox as isize, fy - oy as isize);
        if lx < 0 || ly < 0 || fs < 24 {
            return None;
        }
        let e = evaluate_fine(&ig, lx as usize, ly as usize, fs as usize)?;
        Some((Cand { x: fx as usize, y: fy as usize, ..e }, e.err + e.ring))
    };
    // 2px 刻みで全体を当たってから、最良のまわりだけ 1px 刻みで詰める
    // (1px 刻みの総当たりは 17^3 通りで、候補の数だけ掛かって 1 枚 7ms になった)
    let mut coarse_best: Option<(Cand, f32)> = None;
    for fs in (s - FINE_SIZE..=s + FINE_SIZE).step_by(2) {
        for fy in (y - FINE_POS..=y + FINE_POS).step_by(2) {
            for fx in (x - FINE_POS..=x + FINE_POS).step_by(2) {
                if let Some(h) = eval(fx, fy, fs) {
                    if coarse_best.map_or(true, |b| h.1 < b.1) {
                        coarse_best = Some(h);
                    }
                }
            }
        }
    }
    let (c0, _) = coarse_best?;
    let (bx, by, bs) = (c0.x as isize, c0.y as isize, c0.s as isize);
    let mut hits: Vec<(Cand, f32)> = Vec::new();
    for fs in bs - 3..=bs + 3 {
        for fy in by - 3..=by + 3 {
            for fx in bx - 3..=bx + 3 {
                hits.extend(eval(fx, fy, fs));
            }
        }
    }
    let best = hits.iter().map(|h| h.1).fold(f32::INFINITY, f32::min);
    // 縁の遊びがあるので、マーカーに対して ±1 セルずれた箱はほぼ同じ点数になる。
    // 最良だけを採ると刻みの端に寄るので、同点の箱を平均して真ん中を取る。
    // 種別は最良の箱のものに揃える
    let kind = hits.iter().find(|h| h.1 == best)?.0.kind;
    let near: Vec<&(Cand, f32)> =
        hits.iter().filter(|h| h.0.kind == kind && h.1 <= best + 0.03).collect();
    let n = near.len() as f32;
    let avg = |f: fn(&Cand) -> f32| near.iter().map(|h| f(&h.0)).sum::<f32>() / n;
    Some(Fine {
        kind,
        x: avg(|c| c.x as f32),
        y: avg(|c| c.y as f32),
        s: avg(|c| c.s as f32),
        err: avg(|c| c.err),
        ring: avg(|c| c.ring),
    })
}

/// 1 段目で拾った候補を 2 段目にかけ、種別ごとに良い順で KEEP 個まで残す。
fn refined(img: &GrayImage) -> [Vec<Fine>; 4] {
    let ig = Integral::build(img);
    let coarse = collect(&ig);
    let mut out: [Vec<Fine>; 4] = Default::default();
    for c in coarse.iter().flatten() {
        let Some(f) = refine(img, c) else { continue };
        let list = &mut out[f.kind as usize];
        // 同じマーカーに 1 段目の候補が複数乗ることがある。近いものは良いほうだけ残す
        let (fcx, fcy) = (f.x + f.s / 2.0, f.y + f.s / 2.0);
        if let Some(e) = list.iter_mut().find(|e| {
            let (dx, dy) = (e.x + e.s / 2.0 - fcx, e.y + e.s / 2.0 - fcy);
            dx * dx + dy * dy < (e.s.max(f.s) / 2.0).powi(2)
        }) {
            if f.err + f.ring < e.err + e.ring {
                *e = f;
            }
        } else {
            list.push(f);
        }
    }
    for list in out.iter_mut() {
        list.sort_by(|a, b| (a.err + a.ring).partial_cmp(&(b.err + b.ring)).unwrap());
        list.truncate(KEEP);
    }
    out
}

/// 4 つのマーカーを探し、コードの四隅 (元画像座標、tl→tr→br→bl) を返す。
pub fn locate_markers(img: &GrayImage) -> Option<Quad> {
    locate(img).map(|(q, _)| q)
}

/// 見積もりの候補として返す格子の数
const LAYOUT_GUESSES: usize = 4;

/// 四隅に加えて、写りから格子 (ブロックの列数 × 段数) の候補を、確からしい順に返す。
///
/// 手がかりは 2 つ。マーカーの一辺は格子によらず CORNER セルなので、その写りの大きさが
/// 1 セルの画素数になり、四辺の長さをそれで割ればセル数が出る (ただしマーカーの大きさは
/// 数 % ぶれるので、これだけだと 1 ずれる)。もう 1 つはコードの縦横比で、四隅から
/// 精度よく測れるが、それだけでは格子が 1 つに決まらない。両方のずれを足して順位を付ける。
///
/// 格子を知らされていなくても掴めるので、送信側が格子を切り替えても付いていける。
/// 先頭が外れることもあるので、呼び出し側はヘッダの格子と照合し、合わなければ次を試すこと。
pub fn locate_markers_layout(img: &GrayImage) -> Option<(Quad, Vec<Layout>)> {
    let (q, side) = locate(img)?;
    let dist = |a: (f32, f32), b: (f32, f32)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
    let (px_w, px_h) = (
        (dist(q.tl, q.tr) + dist(q.bl, q.br)) / 2.0,
        (dist(q.tl, q.bl) + dist(q.tr, q.br)) / 2.0,
    );
    let cell = side / CORNER as f32;
    let (cells_w, cells_h) = (px_w / cell, px_h / cell);
    let block = Layout::BLOCK as f32;
    let strips = 2.0 * crate::STRIP_H as f32;
    let (gw0, gh0) = ((cells_w / block).round() as i32, ((cells_h - strips) / block).round() as i32);
    let mut cands: Vec<(f32, Layout)> = Vec::new();
    for gw in (gw0 - 2).max(2)..=(gw0 + 2).min(20) {
        for gh in (gh0 - 2).max(2)..=(gh0 + 2).min(20) {
            let l = Layout::from_grid(gw as usize, gh as usize);
            let (w, h) = (l.width() as f32, l.height() as f32);
            // 大きさのずれ (マーカーの大きさが 3% ぶれる想定) と、縦横比のずれ (1% の想定)
            let size = ((w / cells_w).ln().powi(2) + (h / cells_h).ln().powi(2)) / (0.03f32).powi(2);
            let aspect = ((w / h) / (px_w / px_h)).ln().powi(2) / (0.01f32).powi(2);
            cands.push((size + aspect, l));
        }
    }
    cands.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    Some((q, cands.into_iter().take(LAYOUT_GUESSES).map(|c| c.1).collect()))
}

/// 四隅と、4 つのマーカーの一辺の平均 (元画像の px)。
fn locate(img: &GrayImage) -> Option<(Quad, f32)> {
    if img.w < DOWN * 16 || img.h < DOWN * 16 {
        return None;
    }
    let cands = refined(img);
    // 4 種それぞれ候補があるか
    if cands.iter().any(|v| v.is_empty()) {
        return None;
    }
    // 幾何的に整合する組み合わせを選ぶ。大きさが揃っていて、四辺の長さが対辺で
    // 揃っていて、角の向きが一貫している (裏返っていない) こと。
    let mut best: Option<(f32, [Fine; 4])> = None;
    for &tl in &cands[0] {
        for &tr in &cands[1] {
            for &bl in &cands[2] {
                for &br in &cands[3] {
                    let set = [tl, tr, br, bl];
                    if let Some(score) = geometry_score(&set) {
                        let total = score + set.iter().map(|c| c.err + c.ring).sum::<f32>();
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
    let cx = set.iter().map(|c| c.x + c.s / 2.0).sum::<f32>() / 4.0;
    let cy = set.iter().map(|c| c.y + c.s / 2.0).sum::<f32>() / 4.0;
    let outer = |c: &Fine| -> (f32, f32) {
        let (x0, y0, x1, y1) = (c.x, c.y, c.x + c.s, c.y + c.s);
        let corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
        *corners
            .iter()
            .max_by(|a, b| {
                let da = (a.0 - cx).powi(2) + (a.1 - cy).powi(2);
                let db = (b.0 - cx).powi(2) + (b.1 - cy).powi(2);
                da.partial_cmp(&db).unwrap()
            })
            .unwrap()
    };
    let side = set.iter().map(|c| c.s).sum::<f32>() / 4.0;
    Some((
        Quad { tl: outer(&set[0]), tr: outer(&set[1]), br: outer(&set[2]), bl: outer(&set[3]) },
        side,
    ))
}

/// 位置 (x,y)・一辺 s の箱がマーカーらしいか評価し、種別と不一致度を返す。
///
/// g は箱の縁で見ない幅 (guard)。箱はマーカーに対して刻みのぶんずれうるので、縁を
/// 数えると外周に白が、周囲に黒が混ざって落ちる。strict=false は 1 段目用の緩い条件。
#[allow(clippy::too_many_arguments)]
fn evaluate(
    ig: &Integral,
    x: usize,
    y: usize,
    s: usize,
    t: usize,
    q: usize,
    c3: usize,
    g: isize,
    strict: bool,
) -> Option<Cand> {
    let (xi, yi, si, ti, qi) = (x as isize, y as isize, s as isize, t as isize, q as isize);
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
    // 1 段目は縮小で外周が灰色に寄るので緩める (合否は 2 段目が元の解像度で決める)
    let (min_contrast, max_ring, max_err) =
        if strict { (40.0, 0.65, 0.7) } else { (30.0, 0.75, 0.95) };
    if quiet - ring < min_contrast || ring > quiet * max_ring {
        return None;
    }
    // 周囲は 4 辺とも白いこと。平均だけで見ると、暗い帯 (背景のウィンドウの縁や
    // サイドバー) を横切る箱が「左右は白・上下は黒」で通ってしまい、塗りつぶしの
    // マーカー (左上) の候補を埋め尽くして本物を押し出す (実機: 引いた構図で多発)。
    let (ax0, ay0, ax1, ay1) = (xi - g - qi, yi - g - qi, xi + si + g + qi, yi + si + g + qi);
    let sides = [
        ig.mean(ax0, ay0, ax1, yi - g)?,
        ig.mean(ax0, yi + si + g, ax1, ay1)?,
        ig.mean(ax0, yi - g, xi - g, yi + si + g)?,
        ig.mean(xi + si + g, yi - g, ax1, yi + si + g)?,
    ];
    let darkest = sides.iter().copied().fold(f32::INFINITY, f32::min);
    if darkest - ring < min_contrast || ring > darkest * max_ring {
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
    if err > max_err {
        return None;
    }
    Some(Cand { kind, x, y, s, err, ring: ring / quiet })
}

/// 種別ごとの候補に加える。近い位置の候補は良いほうだけ残す (非最大抑制)。
fn push_cand(list: &mut Vec<Cand>, c: Cand, keep: usize) {
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
    list.truncate(keep);
}

/// 4 候補 (tl, tr, br, bl) の幾何的な整合性。悪いほど大きい値、不整合なら None。
fn geometry_score(set: &[Fine; 4]) -> Option<f32> {
    let smax = set.iter().map(|c| c.s).fold(f32::MIN, f32::max);
    let smin = set.iter().map(|c| c.s).fold(f32::MAX, f32::min);
    if smax / smin > 1.5 {
        return None; // 4 隅で大きさが違いすぎる (遠近では 1.5 倍も違わない)
    }
    let p: Vec<(f32, f32)> = set
        .iter()
        .map(|c| (c.x + c.s / 2.0, c.y + c.s / 2.0))
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

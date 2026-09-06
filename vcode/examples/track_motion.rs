//! 手持ち受信を合成画像で再現し、追従スキャンの取りこぼしを測る実験台。
//!
//! 実機の「三脚 138 KB/s → 手持ち 60 KB/s」がどこで失われているかを、カメラも
//! 実機も無しで切り分けるために作った。送信画面を手持ちカメラで撮る状況を
//!   ・低周波のドリフト + 高周波の震え (並進・回転・スケール)
//!   ・露光時間ぶんの動きボケ
//! で合成し、受信側の追従経路 (scan_frame_tracked → 失敗ならマーカー直接検出) を
//! そのまま回して、検出率と回収ブロック数を出す。
//!
//!     cargo run --release -p vloom-vcode --example track_motion

use std::time::Instant;

use vloom_vcode::markers::locate_markers;
use vloom_vcode::scan::{scan_frame, scan_frame_tracked_with, GrayImage, Homography};
use vloom_vcode::{encode_frame, Bitmap, FrameHeader, Layout, VERSION};

/// 受信カメラの画素数 (Pixel 9a の max = 1600x1200 を縦持ちに回した向き)
const CAM_W: usize = 1200;
const CAM_H: usize = 1600;
/// 露光時間 / フレーム間隔。動きボケの量を決める
const EXPOSURE_FRAC: f32 = 0.25;
/// 露光内のサンプル数 (動きボケの積分)
const SUBS: usize = 3;
const FPS: f32 = 30.0;
/// 送信画面の fps。受信カメラと同じでも位相は揃わないので、必ずどこかの行で切り替わる
const TX_FPS: f32 = 30.0;
/// 送信の切り替わりが 1 枚のどのあたりに来るか (0..1)
const TX_PHASE: f32 = 0.45;
/// センサーの読み出しにかかる時間 / フレーム間隔。1.0 なら上端と下端で丸 1 枚ぶんずれる。
/// 上から下へ順次読むので、切り替わりをまたぐ行では前後のフレームが混ざる
const READOUT_FRAC: f32 = 0.9;

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
    fn unit(&mut self) -> f32 {
        (self.next() % 10000) as f32 / 10000.0
    }
}

/// 手振れの姿勢。並進・回転・スケールを、帯域制限した正弦の和で作る
/// (乱数の積み上げだと際限なく漂うので、実際の手振れに近い周期成分で組む)。
struct Shake {
    amp_tr: f32,
    amp_rot: f32,
    amp_scale: f32,
    ph: [f32; 12],
}

impl Shake {
    fn new(amp_tr: f32, amp_rot: f32, amp_scale: f32, seed: u64) -> Self {
        let mut rng = Lcg(seed);
        let mut ph = [0.0f32; 12];
        for p in ph.iter_mut() {
            *p = rng.unit() * std::f32::consts::TAU;
        }
        Shake { amp_tr, amp_rot, amp_scale, ph }
    }

    /// 3 成分 (ゆっくりしたドリフト / 中域 / 震え) の和。振幅は 1 に正規化してある
    fn mix(&self, t: f32, i: usize) -> f32 {
        let tau = std::f32::consts::TAU;
        ((tau * 0.9 * t + self.ph[i]).sin()
            + 0.6 * (tau * 2.7 * t + self.ph[i + 1]).sin()
            + 0.3 * (tau * 8.5 * t + self.ph[i + 2]).sin())
            / 1.9
    }

    /// (並進 x, 並進 y, 回転 rad, スケール)
    fn pose(&self, t: f32) -> (f32, f32, f32, f32) {
        (
            self.amp_tr * self.mix(t, 0),
            self.amp_tr * self.mix(t, 3),
            self.amp_rot * self.mix(t, 6) * std::f32::consts::PI / 180.0,
            1.0 + self.amp_scale * self.mix(t, 9),
        )
    }
}

/// 時刻 t のカメラ姿勢から、コードの 4 隅 (画像座標) を出す
fn corners_at(shake: &Shake, t: f32, code_w: f32, code_h: f32) -> [(f32, f32); 4] {
    let (dx, dy, rot, sc) = shake.pose(t);
    let (cx, cy) = (CAM_W as f32 / 2.0 + dx, CAM_H as f32 / 2.0 + dy);
    let (hw, hh) = (code_w * sc / 2.0, code_h * sc / 2.0);
    // 面が正対していない (手持ちなので必ず少し傾く) ぶんの台形を入れる
    let tilt = 0.012 * sc;
    let base = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)];
    let (s, c) = (rot.sin(), rot.cos());
    let mut out = [(0.0f32, 0.0f32); 4];
    for (k, &(x, y)) in base.iter().enumerate() {
        let persp = 1.0 + tilt * (y / hh);
        let x = x * persp;
        out[k] = (cx + x * c - y * s, cy + x * s + y * c);
    }
    out
}

/// レンズの樽型歪曲。四隅からの射影変換だけでは吸収できない「周辺が内側にずれる」
/// 成分で、実測ではコードが画角の 9 割を占めると周辺が ±1.5 セル (3.4px/セルで約 5px)
/// ずれる。ここが手持ちでブロックが落ちる主因なので、同じ量を合成側にも入れる。
const LENS_K1: f32 = 0.005;
/// 画面中心 / 周辺のボケ半径 (px)。AF が中心に合っていても周辺は像面湾曲で緩む
const BLUR_CENTER: f32 = 0.35;
const BLUR_EDGE: f32 = 1.15;

/// 画像座標 → 歪曲を取り除いた理想座標 (中心からの距離の 3 次で内側へ)
fn undistort(x: f32, y: f32) -> (f32, f32) {
    let (cx, cy) = (CAM_W as f32 / 2.0, CAM_H as f32 / 2.0);
    let r_norm = ((cx * cx + cy * cy) as f32).sqrt();
    let (dx, dy) = (x - cx, y - cy);
    let r = (dx * dx + dy * dy).sqrt() / r_norm;
    let k = 1.0 + LENS_K1 * r * r;
    (cx + dx * k, cy + dy * k)
}

/// コードのビットマップを 4 隅へ射影し、白紙との差分を acc に書く。
/// 呼び出し側が露光ぶん (SUBS 枚) 平均して動きボケにする。
/// frame_at(row) は「その行が読み出される時刻に画面が出しているフレーム」を返す
/// (ローリングシャッター: センサーは上から下へ順に読むので、行によって別のフレームが写る)。
fn splat(acc: &mut [f32], frames: &[Bitmap], row_frame: &dyn Fn(usize) -> usize, dst: &[(f32, f32); 4]) {
    let (fw, fh) = (frames[0].w as f32, frames[0].h as f32);
    let src_quad = [(0.0, 0.0), (fw, 0.0), (fw, fh), (0.0, fh)];
    let h_inv = Homography::from_quad(&src_quad, dst).unwrap().inverse().unwrap();
    // 歪曲で外へ広がるぶん、描き込む範囲は 4 隅の外接矩形に余裕を持たせる
    let m = 24.0f32;
    let x0 = (dst.iter().map(|p| p.0).fold(f32::MAX, f32::min) - m).floor().max(0.0) as usize;
    let x1 = ((dst.iter().map(|p| p.0).fold(f32::MIN, f32::max) + m).ceil() as usize).min(CAM_W);
    let y0 = (dst.iter().map(|p| p.1).fold(f32::MAX, f32::min) - m).floor().max(0.0) as usize;
    let y1 = ((dst.iter().map(|p| p.1).fold(f32::MIN, f32::max) + m).ceil() as usize).min(CAM_H);
    for y in y0..y1 {
        let bm = &frames[row_frame(y) % frames.len()];
        let src = GrayImage { w: bm.w, h: bm.h, data: &bm.data };
        for x in x0..x1 {
            let (ux, uy) = undistort(x as f32 + 0.5, y as f32 + 0.5);
            let (sx, sy) = h_inv.map(ux, uy);
            if sx < 0.0 || sy < 0.0 || sx >= fw || sy >= fh {
                continue;
            }
            // 表示面の白は紙より少し暗い程度 (白飛びさせない)
            acc[y * CAM_W + x] = src.bilinear(sx, sy) * 0.88 + 12.0 - 250.0;
        }
    }
}

/// 中心から離れるほど強くなるボケ。分離可能な 3 タップで近似する
/// (半径 r のガウスは、両隣に r^2/2 を配る 3 タップとほぼ同じ)。
fn defocus(buf: &mut [f32]) {
    let (cx, cy) = (CAM_W as f32 / 2.0, CAM_H as f32 / 2.0);
    let r_norm = (cx * cx + cy * cy).sqrt();
    let weight = |x: usize, y: usize| {
        let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
        let r = (dx * dx + dy * dy).sqrt() / r_norm;
        let sigma = BLUR_CENTER + (BLUR_EDGE - BLUR_CENTER) * r * r;
        (sigma * sigma / 2.0).min(0.45)
    };
    let src = buf.to_vec();
    for y in 0..CAM_H {
        for x in 1..CAM_W - 1 {
            let a = weight(x, y);
            let i = y * CAM_W + x;
            buf[i] = src[i - 1] * a + src[i] * (1.0 - 2.0 * a) + src[i + 1] * a;
        }
    }
    let src = buf.to_vec();
    for y in 1..CAM_H - 1 {
        for x in 0..CAM_W {
            let a = weight(x, y);
            let i = y * CAM_W + x;
            buf[i] = src[i - CAM_W] * a + src[i] * (1.0 - 2.0 * a) + src[i + CAM_W] * a;
        }
    }
}

fn render(frames: &[Bitmap], shake: &Shake, i: usize, code_w: f32, code_h: f32) -> Vec<u8> {
    let t = i as f32 / FPS;
    let mut sum = vec![0.0f32; CAM_W * CAM_H];
    let mut acc = vec![0.0f32; CAM_W * CAM_H];
    for s in 0..SUBS {
        let sub = EXPOSURE_FRAC * (s as f32 + 0.5) / SUBS as f32 / FPS;
        let ts = t + sub;
        let dst = corners_at(shake, ts, code_w, code_h);
        acc.iter_mut().for_each(|v| *v = 0.0);
        // 行ごとの読み出し時刻から、その行に写る送信フレームを決める
        let row_frame = |row: usize| -> usize {
            let t_row = ts + READOUT_FRAC * (row as f32 / CAM_H as f32) / FPS;
            (t_row * TX_FPS + TX_PHASE).floor().max(0.0) as usize
        };
        splat(&mut acc, frames, &row_frame, &dst);
        for (d, a) in sum.iter_mut().zip(acc.iter()) {
            *d += *a;
        }
    }
    let inv = 1.0 / SUBS as f32;
    sum.iter_mut().for_each(|d| *d *= inv);
    defocus(&mut sum);
    let mut rng = Lcg(0xBEEF + i as u64);
    sum.iter()
        .map(|v| {
            let n = (rng.next() % 9) as f32 - 4.0; // ゲインノイズ ±4
            (250.0 + v + n).clamp(0.0, 255.0) as u8
        })
        .collect()
}

fn build_frames(layout: Layout, n: usize) -> Vec<Bitmap> {
    (0..n)
        .map(|f| {
            let header = FrameHeader {
                version: VERSION,
                bits_per_cell: 1,
                layout,
                frame_seq: f as u16,
                oti: [9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 1, 2],
            };
            let blocks: Vec<Vec<u8>> = (0..layout.block_count())
                .map(|bi| {
                    (0..layout.block_payload_len(1))
                        .map(|i| (i as u8).wrapping_mul(31).wrapping_add((bi + f * 7) as u8))
                        .collect()
                })
                .collect();
            encode_frame(&header, &blocks, 1)
        })
        .collect()
}

/// 受信側の追従経路をそのまま回す。prior = 前フレームのサブセルオフセット場を引き継ぐ
fn run(
    layout: Layout,
    shake: &Shake,
    n_frames: usize,
    use_prior: bool,
    cell_px: f32,
) -> (usize, usize, f64) {
    let frames = build_frames(layout, 8);
    let (code_w, code_h) = (layout.width() as f32 * cell_px, layout.height() as f32 * cell_px);
    let mut last: Option<[(f32, f32); 4]> = None;
    let mut offsets: Option<Vec<Option<(f32, f32)>>> = None;
    let mut detected = 0usize;
    let mut blocks = 0usize;
    let mut scan_ms = 0.0f64;

    for i in 0..n_frames {
        let gray = render(&frames, shake, i, code_w, code_h);
        let img = GrayImage { w: CAM_W, h: CAM_H, data: &gray };
        let t0 = Instant::now();
        let mut ok = false;
        if let Some(c) = last {
            let prior = if use_prior { offsets.as_deref() } else { None };
            if let Ok(r) = scan_frame_tracked_with(&img, &c, layout, prior) {
                last = Some(r.corners);
                blocks += r.frame.blocks.iter().filter(|b| b.is_some()).count();
                offsets = Some(r.block_offsets);
                detected += 1;
                ok = true;
            }
        }
        if !ok {
            if let Some(q) = locate_markers(&img) {
                if let Ok(r) = scan_frame(&img, &q, layout) {
                    last = Some(r.corners);
                    blocks += r.frame.blocks.iter().filter(|b| b.is_some()).count();
                    offsets = Some(r.block_offsets);
                    detected += 1;
                    ok = true;
                }
            }
            if !ok {
                last = None;
                offsets = None;
            }
        }
        scan_ms += t0.elapsed().as_secs_f64() * 1000.0;
    }
    (detected, blocks, scan_ms / n_frames as f64)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n: usize = args
        .iter()
        .position(|a| a == "--frames")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(90);
    let layout = Layout::V6_XDENSE;
    // 実機の 3.1〜3.5 px/セル に合わせる (13x18 を縦持ち視野いっぱいに写した状態)
    let cell_px = (CAM_H as f32 * 0.88) / layout.height() as f32;
    println!(
        "13x18 / 1bit / {:.2} px/セル / {CAM_W}x{CAM_H} / 露光 {:.0}% / {n} フレーム\n",
        cell_px,
        EXPOSURE_FRAC * 100.0
    );
    println!(
        "{:<24} {:>9} {:>13} {:>11} {:>9}  {}",
        "手振れ (並進/回転)", "検出", "回収ブロック", "実効 KB/s", "scan ms", "オフセット探索"
    );
    for &(tr, rot, name) in &[
        (0.0f32, 0.0f32, "三脚 (0px / 0.00deg)"),
        (3.0, 0.15, "軽い (3px / 0.15deg)"),
        (7.0, 0.35, "手持ち (7px / 0.35deg)"),
        (14.0, 0.7, "強い (14px / 0.70deg)"),
    ] {
        for &use_prior in &[false, true] {
            let shake = Shake::new(tr, rot, 0.006, 12345);
            let (det, blk, ms) = run(layout, &shake, n, use_prior, cell_px);
            // 1 ブロック = 42 byte のパケット。取りこぼしなく積めた場合の実効速度
            let kbps = blk as f64 * 42.0 * FPS as f64 / n as f64 / 1024.0;
            println!(
                "{:<24} {:>3}/{:<5} {:>13} {:>11.1} {:>9.1}  {}",
                name,
                det,
                n,
                blk,
                kbps,
                ms,
                if use_prior { "オフセット引継ぎ" } else { "現状 (毎回 5x5)" }
            );
        }
    }
}

//! コーナーマーカー直接検出の合成テスト。
//!
//! 縮尺 (px/セル) を振り、余白付きのキャンバスに置いたコードから四隅が出ることと、
//! 180° 回したコードでも模様から正しい順で四隅が出ることを確かめる。

use vloom_vcode::markers::locate_markers;
use vloom_vcode::scan::{GrayImage, Quad};
use vloom_vcode::{encode_frame, Bitmap, FrameHeader, Layout, VERSION};

fn frame(layout: Layout, seed: u8, scale: usize) -> Bitmap {
    let header = FrameHeader {
        version: VERSION,
        bits_per_cell: 1,
        layout,
        frame_seq: 7,
        oti: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
    };
    let n = layout.block_payload_len(1);
    let blocks: Vec<Vec<u8>> = (0..layout.block_count())
        .map(|b| (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(b as u8 ^ seed)).collect())
        .collect();
    encode_frame(&header, &blocks, scale)
}

/// 白いキャンバスの (x0, y0) にコードを置く。rot180 なら上下左右を反転して置く。
/// 少しノイズも載せる (LCG)。
fn canvas(bm: &Bitmap, cw: usize, ch: usize, x0: usize, y0: usize, rot180: bool) -> Vec<u8> {
    let mut c = vec![235u8; cw * ch];
    let mut rng = 0x9E37u32;
    for y in 0..bm.h {
        for x in 0..bm.w {
            let (sx, sy) = if rot180 { (bm.w - 1 - x, bm.h - 1 - y) } else { (x, y) };
            let v = bm.data[sy * bm.w + sx];
            rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = ((rng >> 24) % 21) as i32 - 10;
            let v = if v < 128 { 22 } else { 235 };
            c[(y0 + y) * cw + x0 + x] = (v as i32 + noise).clamp(0, 255) as u8;
        }
    }
    c
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

fn check(q: &Quad, expect: &Quad, tol: f32, what: &str) {
    for (name, got, want) in [
        ("tl", q.tl, expect.tl),
        ("tr", q.tr, expect.tr),
        ("br", q.br, expect.br),
        ("bl", q.bl, expect.bl),
    ] {
        assert!(
            dist(got, want) <= tol,
            "{what}: {name} が {got:?}、期待 {want:?} (許容 {tol}px)"
        );
    }
}

#[test]
fn finds_corners_across_scales() {
    // 実機の 2.5〜8 px/セル に相当する範囲
    for &scale in &[3usize, 4, 5, 6, 8] {
        for layout in [Layout::V4_TALL, Layout::V0, Layout::from_grid(13, 18)] {
            let bm = frame(layout, 0x5A, scale);
            let m = 12 * scale; // 余白 (セパレータより広ければよい)
            let (cw, ch) = (bm.w + 2 * m, bm.h + 2 * m);
            let data = canvas(&bm, cw, ch, m, m, false);
            let img = GrayImage { w: cw, h: ch, data: &data };
            if std::env::var("MARKERS_DEBUG").is_ok() {
                eprintln!("{}x{} scale {scale} (画像 {cw}x{ch}, コード {}x{}): {:?}", layout.grid_w, layout.grid_h,
                    bm.w, bm.h, vloom_vcode::markers::debug_candidates(&img));
            }
            let q = locate_markers(&img).unwrap_or_else(|| {
                panic!("{}x{} scale {scale}: マーカーが見つからない", layout.grid_w, layout.grid_h)
            });
            let (w, h) = (bm.w as f32, bm.h as f32);
            let expect = Quad {
                tl: (m as f32, m as f32),
                tr: (m as f32 + w, m as f32),
                br: (m as f32 + w, m as f32 + h),
                bl: (m as f32, m as f32 + h),
            };
            // 縮小画像 (1/4) の刻みとサイズ段階のぶん、2 セル程度はずれてよい
            // (その先は scan_frame の ±48px の精密化が吸収する)
            check(&q, &expect, 2.5 * scale as f32 + 4.0, &format!("{}x{} scale {scale}", layout.grid_w, layout.grid_h));
        }
    }
}

#[test]
fn corners_follow_pattern_when_rotated_180() {
    // 上下逆さまに写っても、模様で種別が決まるので tl はコードの (0,0) を指す
    let scale = 5;
    let bm = frame(Layout::V4_TALL, 0x33, scale);
    let m = 60;
    let (cw, ch) = (bm.w + 2 * m, bm.h + 2 * m);
    let data = canvas(&bm, cw, ch, m, m, true);
    let img = GrayImage { w: cw, h: ch, data: &data };
    if std::env::var("MARKERS_DEBUG").is_ok() {
        eprintln!("180° (画像 {cw}x{ch}, コード {}x{}): {:?}", bm.w, bm.h, vloom_vcode::markers::debug_candidates(&img));
        // 真の TR (リング) の位置: 左下 (60, 1620) 一辺 120。近傍の箱を評価してみる
        for (x, y, s) in [(60, 1620, 120), (56, 1616, 124), (64, 1624, 108), (60, 1620, 124), (56, 1616, 128)] {
            eprintln!("  {}", vloom_vcode::markers::debug_eval(&img, x, y, s));
        }
    }
    let q = locate_markers(&img).expect("180° 回転で見つからない");
    let (w, h) = (bm.w as f32, bm.h as f32);
    let expect = Quad {
        tl: (m as f32 + w, m as f32 + h),
        tr: (m as f32, m as f32 + h),
        br: (m as f32, m as f32),
        bl: (m as f32 + w, m as f32),
    };
    check(&q, &expect, 2.5 * scale as f32 + 4.0, "180°");
}

#[test]
fn rejects_blank_image() {
    let data = vec![200u8; 640 * 480];
    let img = GrayImage { w: 640, h: 480, data: &data };
    assert!(locate_markers(&img).is_none());
}

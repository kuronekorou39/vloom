//! スキャン所要時間の簡易ベンチ (実機の scan=NNms と突き合わせるため)。
//!
//! 実機では 1 フレームのスキャンがカメラのフレーム間隔 (30fps なら 33ms) に
//! 収まらないと、取りこぼしてスループットが落ちる。回収できないブロックが
//! サブセルオフセットを何通り試すかが所要時間を支配するので、
//! 「全ブロック回収できる場合」と「半分しか回収できない場合」の両方を測る。
//!
//!     cargo run --release -p vloom-vcode --bench scan_speed

use std::time::Instant;

use vloom_vcode::scan::{scan_frame, GrayImage, Homography, Quad};
use vloom_vcode::{encode_frame, FrameHeader, Layout, VERSION};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
}

/// 実機相当の擬似カメラ画像を作る。noise を上げるとブロックの CRC が通らなくなり、
/// 「回収できないブロックが何通りも試して失敗する」経路を再現できる。
fn synth(layout: Layout, bpc: u8, cell_px: usize, noise: u32) -> (Vec<u8>, usize, usize, Quad) {
    let header = FrameHeader {
        version: VERSION,
        bits_per_cell: bpc,
        layout,
        frame_seq: 7,
        oti: [9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 1, 2],
    };
    let blocks: Vec<Vec<u8>> = (0..layout.block_count())
        .map(|bi| {
            (0..layout.block_payload_len(bpc))
                .map(|i| (i as u8).wrapping_mul(31).wrapping_add(bi as u8))
                .collect()
        })
        .collect();
    let frame = encode_frame(&header, &blocks, cell_px);
    let (cw, ch) = (frame.w + 260, frame.h + 220);
    let (fw, fh) = (frame.w as f32, frame.h as f32);
    let dst = [
        (120.0f32, 90.0),
        (120.0 + fw, 112.0),
        (104.0 + fw, 90.0 + fh),
        (136.0, 74.0 + fh),
    ];
    let src_quad = [(0.0, 0.0), (fw, 0.0), (fw, fh), (0.0, fh)];
    let h_inv = Homography::from_quad(&src_quad, &dst).unwrap().inverse().unwrap();
    let src = GrayImage { w: frame.w, h: frame.h, data: &frame.data };
    let mut rng = Lcg(0xC0FFEE);
    let mut out = vec![250u8; cw * ch];
    for y in 0..ch {
        for x in 0..cw {
            let (sx, sy) = h_inv.map(x as f32, y as f32);
            let v = if sx < -1.0 || sy < -1.0 || sx > fw || sy > fh {
                250.0
            } else {
                src.bilinear(sx, sy)
            };
            // ノイズは画像の右側だけに乗せる。実機と同じく「一部のブロックだけ
            // CRC が通らない」状態を作り、失敗ブロックが何通り試すかを測るため。
            let heavy = x * 100 / cw >= 45;
            let amp = if heavy { noise } else { noise / 8 };
            let n = if amp == 0 { 0.0 } else { (rng.next() % (amp * 2 + 1)) as f32 - amp as f32 };
            out[y * cw + x] = (v * 0.85 + n).clamp(0.0, 255.0) as u8;
        }
    }
    let guide = Quad {
        tl: (dst[0].0 - 14.0, dst[0].1 + 11.0),
        tr: (dst[1].0 + 13.0, dst[1].1 - 9.0),
        br: (dst[2].0 + 11.0, dst[2].1 + 12.0),
        bl: (dst[3].0 - 10.0, dst[3].1 - 13.0),
    };
    (out, cw, ch, guide)
}

fn bench(name: &str, layout: Layout, bpc: u8, cell_px: usize, noise: u32) {
    let (canvas, cw, ch, guide) = synth(layout, bpc, cell_px, noise);
    let img = GrayImage { w: cw, h: ch, data: &canvas };
    // 1 回目で温めてから計測する
    let warm = scan_frame(&img, &guide, layout);
    let ok = warm
        .as_ref()
        .map(|r| r.frame.blocks.iter().filter(|b| b.is_some()).count())
        .unwrap_or(0);
    let runs = 20;
    let t0 = Instant::now();
    for _ in 0..runs {
        let _ = scan_frame(&img, &guide, layout);
    }
    let ms = t0.elapsed().as_secs_f64() * 1000.0 / runs as f64;
    println!(
        "{name:38} {ms:7.1} ms/frame   回収 {ok:3}/{:3}",
        layout.block_count()
    );
}

fn main() {
    println!("(30fps に追従するには 33ms 以内、60fps なら 16ms 以内)\n");
    bench("7x6 2bpc 6px/セル ノイズ小", Layout::V1_DENSE, 2, 6, 4);
    bench("7x6 2bpc 6px/セル 右側つぶれ", Layout::V1_DENSE, 2, 6, 60);
    bench("9x8 2bpc 6px/セル ノイズ小", Layout::V2_ULTRA, 2, 6, 4);
    bench("9x8 2bpc 6px/セル 右側つぶれ", Layout::V2_ULTRA, 2, 6, 60);
}

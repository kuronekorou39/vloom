//! scan の合成テスト失敗の調査用。コーナー推定誤差とブロック別ビット誤りを表示する。
//! 実行: cargo run -p vloom-vcode --example debug_scan

use vloom_vcode::scan::{scan_frame, GrayImage, Homography, Quad};
use vloom_vcode::{encode_frame, FrameHeader, Layout, VERSION};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
}

fn main() {
    let layout = Layout::V0;
    let header = FrameHeader {
        version: VERSION,
        bits_per_cell: 1,
        layout,
        frame_seq: 42,
        oti: [9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 1, 2],
    };
    let blocks: Vec<Vec<u8>> = (0..layout.block_count())
        .map(|bi| {
            (0..layout.block_payload_len(1))
                .map(|i| (i as u8).wrapping_mul(17).wrapping_add(bi as u8 ^ 0xC3))
                .collect()
        })
        .collect();
    let frame_px = encode_frame(&header, &blocks, 8);

    let dst = [
        (180.0f32, 130.0f32),
        (1010.0, 155.0),
        (985.0, 950.0),
        (205.0, 920.0),
    ];
    let src_quad = [
        (0.0, 0.0),
        (frame_px.w as f32, 0.0),
        (frame_px.w as f32, frame_px.h as f32),
        (0.0, frame_px.h as f32),
    ];
    let h_fwd = Homography::from_quad(&src_quad, &dst).unwrap();
    let h_inv = h_fwd.inverse().unwrap();
    let src = GrayImage { w: frame_px.w, h: frame_px.h, data: &frame_px.data };

    let (cw, ch) = (1280usize, 1080usize);
    let mut rng = Lcg(0xFACE);
    let mut canvas = vec![250u8; cw * ch];
    for y in 0..ch {
        for x in 0..cw {
            let (sx, sy) = h_inv.map(x as f32, y as f32);
            let v = if sx < -1.0 || sy < -1.0 || sx > frame_px.w as f32 || sy > frame_px.h as f32 {
                250.0
            } else {
                src.bilinear(sx, sy)
            };
            let gain = 0.75 + 0.20 * (x as f32 / cw as f32);
            let noise = (rng.next() % 17) as f32 - 8.0;
            canvas[y * cw + x] = (v * gain + noise).clamp(0.0, 255.0) as u8;
        }
    }
    // クラッタ (タブバー相当の帯 + テキスト状点列) を追加
    for y in 60..95 {
        for x in 100..1100 {
            canvas[y * 1280 + x] = 40;
        }
    }
    for x in (250..900).step_by(7) {
        for y in 985..997 {
            if (x / 7) % 3 != 0 {
                canvas[y * 1280 + x] = 20;
                canvas[y * 1280 + x + 3] = 25;
            }
        }
    }
    let img = GrayImage { w: cw, h: ch, data: &canvas };

    let guide = Quad {
        tl: (dst[0].0 - 22.0, dst[0].1 + 18.0),
        tr: (dst[1].0 + 25.0, dst[1].1 - 15.0),
        br: (dst[2].0 + 19.0, dst[2].1 + 24.0),
        bl: (dst[3].0 - 17.0, dst[3].1 - 25.0),
    };

    match scan_frame(&img, &guide, layout) {
        Err(e) => println!("scan_frame エラー: {e:?}"),
        Ok(result) => {
            println!("真の 4 隅:   {:?}", dst);
            let hm = &result.homography;
            let (wc, hc) = (layout.width() as f32, layout.height() as f32);
            for (name, (cx, cy)) in [
                ("tl", (0.0, 0.0)),
                ("tr", (wc, 0.0)),
                ("br", (wc, hc)),
                ("bl", (0.0, hc)),
            ] {
                let p = hm.map(cx, cy);
                println!("推定 {name}: ({:.1}, {:.1})", p.0, p.1);
            }

            // グラウンドトゥルースとの照合: 真のホモグラフィでセル値を求め、
            // 推定ホモグラフィのサンプル結果と比較する
            let ok = result.frame.blocks.iter().filter(|b| b.is_some()).count();
            println!("ブロック回収: {ok}/{}", layout.block_count());
            let status: String = result
                .frame
                .blocks
                .iter()
                .map(|b| if b.is_some() { 'O' } else { 'x' })
                .collect();
            println!("ブロックマップ (5列x4行):");
            for row in status.as_bytes().chunks(5) {
                println!("  {}", String::from_utf8_lossy(row));
            }

            // scan_frame 内部と同じ手順 (Otsu) を再現して閾値と値分布を確認
            let (w, h) = (layout.width(), layout.height());
            let mut values = vec![0u8; w * h];
            for r in 0..h {
                for c in 0..w {
                    let (x, y) = hm.map(c as f32 + 0.5, r as f32 + 0.5);
                    values[r * w + c] = img.bilinear(x, y).round().clamp(0.0, 255.0) as u8;
                }
            }
            let mut hist = [0u32; 256];
            for &v in &values {
                hist[v as usize] += 1;
            }
            let lo: u32 = hist[..100].iter().sum();
            let mid: u32 = hist[100..180].iter().sum();
            let hi: u32 = hist[180..].iter().sum();
            println!("値分布: <100:{lo}  100-180:{mid}  >=180:{hi}");
            // 中間帯の詳細
            for (i, &c) in hist.iter().enumerate() {
                if (60..200).contains(&i) && c > 0 {
                    print!("{i}:{c} ");
                }
            }
            println!();

            // 手動でブロック CRC 判定を再現 (scan_frame 内部と同一のはずの手順)
            let thr_manual = {
                // 単純な谷選び: 分離ギャップの中央 (デバッグ用)
                140u8
            };
            let mut manual_ok = 0;
            for bi in 0..layout.block_count() {
                let by = bi / layout.grid_w;
                let bx = bi % layout.grid_w;
                let mut bits = Vec::with_capacity(layout.block * layout.block);
                for i in 0..layout.block * layout.block {
                    let r = vloom_vcode::STRIP_H + by * layout.block + i / layout.block;
                    let c = bx * layout.block + i % layout.block;
                    bits.push(values[r * w + c] < thr_manual);
                }
                let bytes: Vec<u8> = bits
                    .chunks(8)
                    .map(|ch| ch.iter().fold(0u8, |acc, &b| (acc << 1) | b as u8))
                    .collect();
                let (payload, crc) = bytes.split_at(layout.block_payload_len(1));
                if vloom_vcode::crc32(payload)
                    == u32::from_be_bytes([crc[0], crc[1], crc[2], crc[3]])
                {
                    manual_ok += 1;
                } else if bi < 3 {
                    println!(
                        "block{bi} 手動CRC不一致: payload先頭 {:02x?} 期待 {:02x?}",
                        &payload[..8],
                        &blocks[bi][..8]
                    );
                }
            }
            println!("手動 CRC 判定: {manual_ok}/{}", layout.block_count());

            // セル単位のビット誤り率をブロックごとに算出
            let scale = 8.0f32;
            for bi in 0..layout.block_count() {
                let by = bi / layout.grid_w;
                let bx = bi % layout.grid_w;
                let mut errs = 0;
                for r in 0..layout.block {
                    for c in 0..layout.block {
                        let cell_r = vloom_vcode::STRIP_H + by * layout.block + r;
                        let cell_c = bx * layout.block + c;
                        // 真値: レンダリング画像のセル中心
                        let truth = frame_px.data[((cell_r as f32 * scale + 4.0) as usize)
                            * frame_px.w
                            + (cell_c as f32 * scale + 4.0) as usize]
                            < 128;
                        // 推定側: 推定 H でサンプリング
                        let (x, y) = hm.map(cell_c as f32 + 0.5, cell_r as f32 + 0.5);
                        let est = img.bilinear(x, y) < 128.0;
                        if truth != est {
                            errs += 1;
                        }
                    }
                }
                print!("block{bi:02} err={errs:3} ");
                if bi % 5 == 4 {
                    println!();
                }
            }
        }
    }
}

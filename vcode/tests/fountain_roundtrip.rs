//! vcode と fountain (RaptorQ) の結合テスト。
//! 送信: payload → RaptorQ パケット → vcode フレーム列
//! 受信: フレームデコード (部分回収) → RaptorQ デコーダ → payload 復元
//! を、無損失と「ブロック単位の破損あり」の両方で検証する。

use vloom_fountain::{Decoder, Encoder};
use vloom_vcode::{decode_frame, encode_frame, FrameHeader, Layout, VERSION};

const PAYLOAD_LEN: usize = 20_000;

fn test_payload() -> Vec<u8> {
    (0..PAYLOAD_LEN).map(|i| (i as u8).wrapping_mul(37).wrapping_add((i >> 8) as u8)).collect()
}

/// 決定的な擬似乱数 (テスト再現性のため std のみで済ませる)
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
}

/// payload を vcode フレーム列にエンコードする (送信側の処理そのもの)
fn build_frames(encoder: &Encoder, layout: Layout) -> Vec<vloom_vcode::Bitmap> {
    let per_frame = layout.block_count();
    let n_frames = encoder.packet_count().div_ceil(per_frame);
    (0..n_frames)
        .map(|f| {
            let header = FrameHeader {
                version: VERSION,
                bits_per_cell: 1,
                layout,
                frame_seq: f as u16,
                oti: encoder.oti_bytes(),
            };
            let blocks: Vec<Vec<u8>> = (f * per_frame..encoder.packet_count())
                .take(per_frame)
                .map(|i| encoder.packet(i))
                .collect();
            encode_frame(&header, &blocks, 1)
        })
        .collect()
}

#[test]
fn e2e_no_loss() {
    let payload = test_payload();
    let layout = Layout::V0;
    let encoder = Encoder::new(&payload, layout.packet_size(1) as u16, 0);
    let frames = build_frames(&encoder, layout);

    // OTI はフレームヘッダから取る (プロトコル外の共有を不要にする設計の確認)
    let first = decode_frame(&frames[0], 1).unwrap();
    let mut decoder = Decoder::from_oti_bytes(&first.header.oti);

    for bm in &frames {
        let decoded = decode_frame(bm, 1).unwrap();
        for block in decoded.blocks.into_iter().flatten() {
            if let Some(out) = decoder.add_packet(&block) {
                assert_eq!(out, payload);
                return;
            }
        }
    }
    panic!("無損失で復元できなかった");
}

#[test]
fn e2e_2bpc_with_padding() {
    // 輝度 4 値: payload 98B に対し raptorq はシンボルを丸める (94→88, シリアライズ 92B)。
    // 送信はゼロパディング、受信は OTI のシンボルサイズで切り出す規約を検証する。
    let payload = test_payload();
    let layout = Layout::V0;
    let bpc = 2u8;
    let encoder = Encoder::new(&payload, layout.packet_size(bpc) as u16, 0);
    let payload_len = layout.block_payload_len(bpc);

    let per_frame = layout.block_count();
    let pc = encoder.packet_count();
    let n_frames = pc.div_ceil(per_frame);
    let mut oti = [0u8; 12];
    oti.copy_from_slice(&encoder.oti_bytes());
    let pkt_len = 4 + vloom_fountain::oti_symbol_size(&oti) as usize;
    assert!(pkt_len <= payload_len, "パケットがペイロードに入らない");

    let mut decoder = Decoder::from_oti_bytes(&oti);
    for f in 0..n_frames {
        let header = FrameHeader {
            version: VERSION,
            bits_per_cell: bpc,
            layout,
            frame_seq: f as u16,
            oti,
        };
        let blocks: Vec<Vec<u8>> = (0..per_frame)
            .map(|j| {
                let mut p = encoder.packet((f * per_frame + j) % pc);
                p.resize(payload_len, 0);
                p
            })
            .collect();
        let bm = encode_frame(&header, &blocks, 1);
        let decoded = decode_frame(&bm, 1).unwrap();
        assert_eq!(decoded.header.bits_per_cell, 2);
        for block in decoded.blocks.into_iter().flatten() {
            // 受信側の切り出し規約
            let packet = &block[..pkt_len];
            if let Some(out) = decoder.add_packet(packet) {
                assert_eq!(out, payload);
                return;
            }
        }
    }
    panic!("2bpc + パディングで復元できなかった");
}

#[test]
fn e2e_with_block_corruption() {
    let payload = test_payload();
    let layout = Layout::V0;
    // ブロック破損 30% を想定し、リペアを 60% 追加
    let source_packets = PAYLOAD_LEN.div_ceil(layout.packet_size(1));
    let encoder = Encoder::new(&payload, layout.packet_size(1) as u16, (source_packets * 6 / 10) as u32);
    let frames = build_frames(&encoder, layout);

    let mut rng = Lcg(0xBEEF);
    let mut corrupted_blocks = 0usize;
    let mut recovered_blocks = 0usize;
    let mut decoder: Option<Decoder> = None;

    for bm in &frames {
        // 受信画像の劣化を模擬: 各ブロック領域を 30% の確率で黒塗りにする
        let mut damaged = vloom_vcode::Bitmap {
            w: bm.w,
            h: bm.h,
            data: bm.data.clone(),
        };
        let block = layout.block;
        for by in 0..layout.grid_h {
            for bx in 0..layout.grid_w {
                if rng.next() % 100 < 30 {
                    corrupted_blocks += 1;
                    for y in 0..block {
                        for x in 0..block {
                            damaged.set(bx * block + x, vloom_vcode::STRIP_H + by * block + y, 0);
                        }
                    }
                }
            }
        }

        // ヘッダはデータ破損の影響を受けず毎フレーム読めるはず
        let decoded = decode_frame(&damaged, 1).expect("ヘッダ/コーナーは無傷のはず");
        let dec = decoder.get_or_insert_with(|| Decoder::from_oti_bytes(&decoded.header.oti));

        for block in decoded.blocks.into_iter().flatten() {
            recovered_blocks += 1;
            if let Some(out) = dec.add_packet(&block) {
                assert_eq!(out, payload, "破損チャネル経由で payload が一致しない");
                assert!(corrupted_blocks > 0, "破損が発生していないとテストの意味がない");
                println!(
                    "破損ブロック {} 個 / 回収ブロック {} 個で復元成功",
                    corrupted_blocks, recovered_blocks
                );
                return;
            }
        }
    }
    panic!(
        "30% ブロック破損で復元できなかった (破損 {} / 回収 {})",
        corrupted_blocks, recovered_blocks
    );
}

//! 「さがす」モード: その環境で一番速い設定を探すための試験フレーム。
//!
//! 送信側は格子と fps の組を数秒ずつ切り替えながら試験フレームを流し続け、受信側は
//! 設定ごとに「重複なしで読めたブロック数」を数えて実効スループットを出す。ファイルは
//! 運ばない (Fountain code を通さない)。同じ光路・同じスキャナ・同じブロック形式を
//! 使うので、数字は本番の転送にほぼ対応する。
//!
//! 形式は通常のフレームと同じで、中身だけが違う:
//!   - ヘッダの OTI 欄 (12 バイト) に、試験フレームの印と設定の情報を入れる
//!   - 各ブロックのペイロードの先頭に (フレーム番号, ブロック番号, 設定) を入れ、
//!     残りを擬似乱数で埋める。ブロックが自分の素性を持つので、切り替わりをまたいで
//!     前後の枚や前後の設定が 1 枚に混ざって写っても、正しく数えられる
//!
//! 通常の受信に試験フレームが写っても害はない。OTI 欄の先頭 5 バイトは RaptorQ の
//! 転送長として読まれ、印のせいで約 370GB になるので、受信側の上限
//! (`fountain::MAX_PAYLOAD_BYTES`) で弾かれる。

use crate::{encode_frame, Bitmap, FrameHeader, Layout, VERSION};

/// OTI 欄の先頭に置く印
pub const PROBE_MAGIC: [u8; 4] = *b"VPRB";

/// ブロックの先頭に置く素性の長さ (フレーム番号 4 + ブロック番号 2 + 設定 1 + fps 1)
const BLOCK_ID_LEN: usize = 8;

/// 試験フレームのヘッダに載る設定の情報。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeTag {
    /// 今流している設定の番号 (0 始まり)
    pub setting: u8,
    /// 一巡に含まれる設定の数
    pub settings: u8,
    /// この設定の送信 fps
    pub fps: u8,
    /// 1 つの設定を流し続ける長さ (0.1 秒単位)
    pub dwell_ds: u8,
}

impl ProbeTag {
    /// OTI 欄に入れる 12 バイト。
    ///
    /// 6〜7 バイト目は RaptorQ の OTI でシンボル長にあたる。受信側の共通経路が
    /// 「4 + シンボル長」でブロックを切り出すので、通常の転送と同じ値を入れておく
    /// (0 にするとブロックの中身が 4 バイトに切り詰められてしまう)。
    pub fn to_oti(&self, layout: Layout) -> [u8; 12] {
        let mut o = [0u8; 12];
        o[..4].copy_from_slice(&PROBE_MAGIC);
        o[4] = self.setting;
        o[6..8].copy_from_slice(&(layout.packet_size(1) as u16).to_be_bytes());
        o[8] = self.settings;
        o[9] = self.fps;
        o[10] = self.dwell_ds;
        o
    }

    /// OTI 欄が試験フレームのものなら設定の情報を返す。
    pub fn from_oti(oti: &[u8]) -> Option<ProbeTag> {
        if oti.len() < 12 || oti[..4] != PROBE_MAGIC {
            return None;
        }
        Some(ProbeTag { setting: oti[4], settings: oti[8], fps: oti[9], dwell_ds: oti[10] })
    }
}

/// 試験フレームのブロック 1 つの素性。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeBlock {
    /// 送信開始からのフレーム番号 (設定をまたいで増え続ける)
    pub frame: u32,
    /// フレーム内のブロック番号 (行優先)
    pub block: u16,
    pub setting: u8,
    pub fps: u8,
}

impl ProbeBlock {
    /// 回収したブロックのペイロードから素性を読む。
    /// CRC が通ったブロックだけを渡すこと (中身の検査はしない)。
    pub fn parse(payload: &[u8]) -> Option<ProbeBlock> {
        if payload.len() < BLOCK_ID_LEN {
            return None;
        }
        Some(ProbeBlock {
            frame: u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]),
            block: u16::from_le_bytes([payload[4], payload[5]]),
            setting: payload[6],
            fps: payload[7],
        })
    }
}

/// 試験フレームのブロック 1 つのペイロード。素性 (フレーム番号・ブロック番号・設定) の
/// 後ろを擬似乱数で埋める。中身は素性だけで決まるので、受信した側でも作り直せる
/// (調査で「正解」として使う)。
pub fn probe_block_payload(layout: Layout, tag: ProbeTag, frame: u32, bi: usize) -> Vec<u8> {
    let len = layout.block_payload_len(1);
    let mut p = Vec::with_capacity(len);
    p.extend_from_slice(&frame.to_le_bytes());
    p.extend_from_slice(&(bi as u16).to_le_bytes());
    p.push(tag.setting);
    p.push(tag.fps);
    // xorshift32。種はフレームとブロックで決まるので、どの送信実装でも同じ絵になる
    let mut x = (frame.wrapping_mul(0x9E37_79B1) ^ (bi as u32).wrapping_mul(0x85EB_CA77)) | 1;
    while p.len() < len {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        p.push((x >> 8) as u8);
    }
    p
}

/// 試験フレームを 1 枚作る。frame は送信開始からの通し番号。
///
/// ペイロードの残りは擬似乱数で埋める。本番のデータ (RaptorQ のパケット) と同じく
/// 白黒がほぼ半々になり、ブロックごとの閾値や混ざり方が本番と同じ条件になる。
pub fn probe_frame(layout: Layout, tag: ProbeTag, frame: u32) -> Bitmap {
    let header = FrameHeader {
        version: VERSION,
        bits_per_cell: 1,
        layout,
        frame_seq: (frame % 0x10000) as u16,
        oti: tag.to_oti(layout),
    };
    let blocks: Vec<Vec<u8>> = (0..layout.block_count())
        .map(|bi| probe_block_payload(layout, tag, frame, bi))
        .collect();
    encode_frame(&header, &blocks, 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode_frame;

    #[test]
    fn tag_roundtrip_and_symbol_size() {
        let tag = ProbeTag { setting: 3, settings: 6, fps: 20, dwell_ds: 40 };
        let oti = tag.to_oti(Layout::V6_XDENSE);
        assert_eq!(ProbeTag::from_oti(&oti), Some(tag));
        // 受信側の共通経路がブロックを切り出す長さ (4 + シンボル長) がペイロード全体になる
        let symbol = u16::from_be_bytes([oti[6], oti[7]]) as usize;
        assert_eq!(4 + symbol, Layout::V6_XDENSE.block_payload_len(1));
        // 通常の転送の OTI は試験フレームと見なさない
        assert_eq!(ProbeTag::from_oti(&[0, 0, 16, 0, 0, 0, 0, 42, 1, 0, 1, 8]), None);
    }

    #[test]
    fn frame_decodes_and_blocks_identify_themselves() {
        let layout = Layout::from_grid(15, 20);
        let tag = ProbeTag { setting: 2, settings: 4, fps: 30, dwell_ds: 30 };
        let frame = decode_frame(&probe_frame(layout, tag, 70_000), 1).expect("復号できる");
        assert_eq!(frame.header.layout, layout);
        assert_eq!(ProbeTag::from_oti(&frame.header.oti), Some(tag));
        assert_eq!(frame.blocks.len(), layout.block_count());
        for (bi, b) in frame.blocks.iter().enumerate() {
            let id = ProbeBlock::parse(b.as_ref().expect("全ブロック回収")).unwrap();
            assert_eq!(id, ProbeBlock { frame: 70_000, block: bi as u16, setting: 2, fps: 30 });
        }
    }

    #[test]
    fn payload_is_balanced() {
        // 白黒が偏ると、ブロックごとの Otsu 閾値が本番と違う条件で働いてしまう
        let bm = probe_frame(Layout::V6_XDENSE, ProbeTag { setting: 0, settings: 1, fps: 20, dwell_ds: 40 }, 5);
        let layout = Layout::V6_XDENSE;
        let (or, oc) = layout.block_origin(100);
        let black = (0..400)
            .filter(|i| bm.get(oc + i % 20, or + i / 20) < 128)
            .count();
        assert!((150..=250).contains(&black), "黒セル {black}/400");
    }
}

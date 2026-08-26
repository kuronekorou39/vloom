//! Fountain code (RaptorQ) を薄くラップして、コードのストリーミング用の符号化/復号 API を提供する。
//!
//! 設計方針 (MVP):
//!   - 事前に K+ N パケット (= K source + N repair) を生成して固定配列で持つ
//!   - 送信側は配列を循環表示
//!   - 受信側は受け取ったパケット (順不同) を decoder に投入し、復元できたら終了
//!   - 受信に必要なのは OTI (12 byte) だけ。OTI は最初の数フレームで別途送る or プロトコル外で共有
//!
//! 公開 API:
//!   - `Encoder` / `encoder_new(payload, packet_size, extra_repair) -> Encoder`
//!   - `encoder.oti() -> &OTI` (12 byte でシリアライズ可能)
//!   - `encoder.packet(i) -> Vec<u8>` (i を循環させて呼ぶ)
//!   - `encoder.packet_count() -> usize`
//!   - `Decoder` / `decoder_new_from_oti(oti_bytes) -> Decoder`
//!   - `decoder.add_packet(bytes) -> Option<Vec<u8>>`

use raptorq::{Decoder as RqDecoder, Encoder as RqEncoder, EncodingPacket, ObjectTransmissionInformation};

pub const DEFAULT_PACKET_SIZE: u16 = 500;

/// OTI からシンボルサイズを取り出す。
/// raptorq は指定 packet_size を 8 の倍数などに丸めるため、実際のシリアライズ済み
/// パケット長 (= 4 + symbol_size) はこの値から求めること。
pub fn oti_symbol_size(oti_bytes: &[u8; 12]) -> u16 {
    ObjectTransmissionInformation::deserialize(oti_bytes).symbol_size()
}

pub struct Encoder {
    oti: ObjectTransmissionInformation,
    /// 事前生成した K source + N repair = packets.len() のパケット列。
    packets: Vec<EncodingPacket>,
}

impl Encoder {
    /// payload を packet_size で符号化し、追加で extra_repair 個のリペアパケットを生成する。
    /// extra_repair = 0 ならパケットは source 分のみ (= ceil(payload.len() / packet_size))。
    /// 損失に強くしたければ extra_repair を増やす (例: source の 50% 程度)。
    pub fn new(payload: &[u8], packet_size: u16, extra_repair: u32) -> Self {
        let encoder = RqEncoder::with_defaults(payload, packet_size);
        let oti = encoder.get_config();
        let packets = encoder.get_encoded_packets(extra_repair);
        Self { oti, packets }
    }

    pub fn oti(&self) -> &ObjectTransmissionInformation {
        &self.oti
    }

    pub fn oti_bytes(&self) -> [u8; 12] {
        self.oti.serialize()
    }

    pub fn packet_count(&self) -> usize {
        self.packets.len()
    }

    /// i 番目のパケット (シリアライズ済み) を返す。i は packet_count() で割って循環してよい。
    pub fn packet(&self, i: usize) -> Vec<u8> {
        self.packets[i % self.packets.len()].serialize()
    }
}

pub struct Decoder {
    inner: RqDecoder,
    oti: ObjectTransmissionInformation,
    packets_received: u32,
}

impl Decoder {
    pub fn from_oti_bytes(oti_bytes: &[u8; 12]) -> Self {
        let oti = ObjectTransmissionInformation::deserialize(oti_bytes);
        let inner = RqDecoder::new(oti.clone());
        Self { inner, oti, packets_received: 0 }
    }

    pub fn payload_size(&self) -> u64 {
        self.oti.transfer_length()
    }

    pub fn packets_received(&self) -> u32 {
        self.packets_received
    }

    /// シリアライズ済みパケットを 1 つ追加。完全に復元できたら Some(payload) を返す。
    pub fn add_packet(&mut self, packet_bytes: &[u8]) -> Option<Vec<u8>> {
        let packet = EncodingPacket::deserialize(packet_bytes);
        self.inner.add_new_packet(packet);
        self.packets_received += 1;
        self.inner.get_result()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_no_loss() {
        let payload: Vec<u8> = (0..2000).map(|i| (i as u8).wrapping_mul(31)).collect();
        let encoder = Encoder::new(&payload, 200, 20);
        let oti_bytes = encoder.oti_bytes();
        let mut decoder = Decoder::from_oti_bytes(&oti_bytes);
        for i in 0..encoder.packet_count() {
            if let Some(out) = decoder.add_packet(&encoder.packet(i)) {
                assert_eq!(out, payload);
                return;
            }
        }
        panic!("did not recover");
    }

    #[test]
    fn recovers_with_50pct_loss() {
        let payload: Vec<u8> = (0..5000).map(|i| (i as u8).wrapping_mul(13)).collect();
        let encoder = Encoder::new(&payload, 250, 30);
        let oti_bytes = encoder.oti_bytes();
        let mut decoder = Decoder::from_oti_bytes(&oti_bytes);
        // 偶数番号のパケットだけ採用 (50% 損失)
        for i in (0..encoder.packet_count()).step_by(2) {
            if let Some(out) = decoder.add_packet(&encoder.packet(i)) {
                assert_eq!(out, payload);
                return;
            }
        }
        panic!("did not recover at 50% loss (Fountain code 想定通り動いていない)");
    }

    #[test]
    fn large_payload_200kb() {
        let payload: Vec<u8> = (0..200_000).map(|i| (i as u8).wrapping_mul(7)).collect();
        let encoder = Encoder::new(&payload, 500, 100);
        // packet_count はおおむね 400 + 100 = 500 のはず
        assert!(encoder.packet_count() >= 400 && encoder.packet_count() <= 600);
        let oti_bytes = encoder.oti_bytes();
        let mut decoder = Decoder::from_oti_bytes(&oti_bytes);
        for i in 0..encoder.packet_count() {
            if let Some(out) = decoder.add_packet(&encoder.packet(i)) {
                assert_eq!(out.len(), payload.len());
                assert_eq!(out, payload);
                return;
            }
        }
        panic!("did not recover 200KB payload");
    }
}

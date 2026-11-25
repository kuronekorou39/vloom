//! Fountain code (RaptorQ) を薄くラップして、コードのストリーミング用の符号化/復号 API を提供する。
//!
//! 設計方針:
//!   - 1 巡目の K source + N repair は事前に生成して持つ
//!   - 送信側はフレームの通し番号を増やし続け、1 巡目を過ぎたら新しいリペアをその場で作る
//!     (同じパケットを繰り返さない。fountain 符号の「新しい符号を無限に作れる」性質を使う)
//!   - 受信側は受け取ったパケット (順不同) を decoder に投入し、復元できたら終了
//!   - 受信に必要なのは OTI (12 byte) だけ。OTI は最初の数フレームで別途送る or プロトコル外で共有
//!
//! 公開 API:
//!   - `Encoder` / `encoder_new(payload, packet_size, extra_repair) -> Encoder`
//!   - `encoder.oti() -> &OTI` (12 byte でシリアライズ可能)
//!   - `encoder.packet(i) -> Vec<u8>` (i は増やし続けてよい。1 巡目以降は新しいリペア)
//!   - `encoder.packet_count() -> usize`
//!   - `Decoder` / `decoder_new_from_oti(oti_bytes) -> Decoder`
//!   - `decoder.add_packet(bytes) -> Option<Vec<u8>>`

use std::collections::HashSet;

use raptorq::{
    partition, Encoder as RqEncoder, EncodingPacket, ObjectTransmissionInformation, SourceBlockDecoder,
};

pub const DEFAULT_PACKET_SIZE: u16 = 500;

/// OTI からシンボルサイズを取り出す。
/// raptorq は指定 packet_size を 8 の倍数などに丸めるため、実際のシリアライズ済み
/// パケット長 (= 4 + symbol_size) はこの値から求めること。
pub fn oti_symbol_size(oti_bytes: &[u8; 12]) -> u16 {
    ObjectTransmissionInformation::deserialize(oti_bytes).symbol_size()
}

pub struct Encoder {
    oti: ObjectTransmissionInformation,
    /// 1 巡目に出す K source + N repair = packets.len() のパケット列 (事前生成)。
    packets: Vec<EncodingPacket>,
    /// 2 巡目以降のリペアを作るための符号器 (ブロックごとの中間シンボルを持つ)。
    rq: RqEncoder,
    /// ブロックごとに 1 巡目で使ったリペアの数 (2 巡目以降はこの続きの番号から作る)
    initial_repair: Vec<u32>,
    /// ブロックごとのソースシンボル数 (ESI の上限の計算用)
    block_k: Vec<u32>,
}

/// 1 ソースブロックのシンボル数の目安 (42B シンボルで約 290KB)。
///
/// raptorq の既定は 1 ブロックを上限近く (約 5 万シンボル = 2.3MB) まで大きくする。
/// 送信は全ブロックへ均等に配るので、大きいブロックはどれも受信の最後 1% でほぼ同時に
/// 揃い、復元の計算 (スマホで 1 ブロック数秒) が「ゲージ満タン後」にまとめて走っていた。
/// 小さいブロックは揃う時期が終盤にばらけ、計算を受信と並行して済ませられる。
/// 30.6MB の見積もり (docs/measurements/2026-11-25-laptop-sweep.md): 満タン後の待ちは
/// 約 9 秒 → 3 秒、受信は 2% 延びる (最後のブロックが揃うのを待つ分)。計算の合計も 3 割減る。
const TARGET_BLOCK_SYMBOLS: u64 = 7_000;

/// payload を何ブロックに分けるか決めた OTI。シンボル長と整列は raptorq の既定と同じ。
/// 受信側は OTI に書かれた分け方に従うので、古い受信側でもそのまま読める。
fn block_config(len: u64, packet_size: u16) -> ObjectTransmissionInformation {
    let def = ObjectTransmissionInformation::with_defaults(len, packet_size);
    let kt = len.div_ceil(def.symbol_size() as u64);
    // Z は 8bit。上限に当たったら既定より細かくはしない (1 ブロックの上限 56,403 には収まる)
    let z = kt.div_ceil(TARGET_BLOCK_SYMBOLS).clamp(def.source_blocks() as u64, 255) as u8;
    ObjectTransmissionInformation::new(len, def.symbol_size(), z, 1, def.symbol_alignment())
}

/// RaptorQ の ESI は 24bit。1 ブロックのシンボル番号はこの手前までしか使えない。
const MAX_ESI: u32 = (1 << 24) - 1;

impl Encoder {
    /// payload を packet_size で符号化し、追加で extra_repair 個のリペアパケットを生成する。
    /// extra_repair = 0 ならパケットは source 分のみ (= ceil(payload.len() / packet_size))。
    /// 損失に強くしたければ extra_repair を増やす (例: source の 50% 程度)。
    ///
    /// extra_repair は**オブジェクト全体**でのリペア数。RaptorQ は 2MB 前後を超えると
    /// 複数のソースブロックに割るので、各ブロックへはソースシンボル数に比例して配る。
    /// (raptorq の `get_encoded_packets(n)` の n はブロックあたりの数。全体の 50% を
    /// そのまま渡すと 12MB で 1 巡がデータの 4 倍、32MB で 8.5 倍に膨らんでいた。)
    ///
    /// パケットはブロックをまたいで交互に並べる。ブロック順に並べると、あるブロックの
    /// 取りこぼしは 1 巡 (= ファイル全体ぶん) 待たないと埋まらず、ブロックごとの
    /// 余剰リペアも「もう復元済みのブロック」に使われて無駄になる。
    pub fn new(payload: &[u8], packet_size: u16, extra_repair: u32) -> Self {
        let encoder = RqEncoder::new(payload, block_config(payload.len() as u64, packet_size));
        let oti = encoder.get_config();
        let blocks = encoder.get_block_encoders();
        let k_total: u64 = blocks.iter().map(|b| b.source_packets().len() as u64).sum();
        let mut initial_repair = Vec::with_capacity(blocks.len());
        let mut block_k = Vec::with_capacity(blocks.len());
        let per_block: Vec<Vec<EncodingPacket>> = blocks
            .iter()
            .map(|b| {
                let mut v = b.source_packets();
                let k = v.len() as u64;
                let repair = if blocks.len() <= 1 {
                    extra_repair
                } else {
                    (extra_repair as u64 * k).div_ceil(k_total.max(1)) as u32
                };
                v.extend(b.repair_packets(0, repair));
                initial_repair.push(repair);
                block_k.push(k as u32);
                v
            })
            .collect();
        Self { oti, packets: interleave(per_block), rq: encoder, initial_repair, block_k }
    }

    pub fn oti(&self) -> &ObjectTransmissionInformation {
        &self.oti
    }

    pub fn oti_bytes(&self) -> [u8; 12] {
        self.oti.serialize()
    }

    /// 1 巡目のパケット数 (送信側の「1 巡」の長さ)。packet() はこれを超えても呼べる。
    pub fn packet_count(&self) -> usize {
        self.packets.len()
    }

    /// i 番目のパケット (シリアライズ済み) を返す。i は 0 から増やし続けてよい。
    ///
    /// i < packet_count() は 1 巡目の事前生成ぶん。それ以降は**毎回新しいリペア**を作る
    /// (fountain 符号は新しい符号を無限に作れる)。以前は 1 巡目と同じパケットを繰り返して
    /// いたので、2 巡目以降は 1 巡目で受け取れたパケットが届いても役に立たなかった。
    /// 受け取れる割合が 5 割の環境で、30MB の所要時間が見積もりで約 16% 縮む。
    pub fn packet(&self, i: usize) -> Vec<u8> {
        if i < self.packets.len() {
            return self.packets[i].serialize();
        }
        // 2 巡目以降: ブロックを順に回り、各ブロックのリペア番号を 1 巡目の続きから進める
        let blocks = self.rq.get_block_encoders();
        let r = i - self.packets.len();
        let b = r % blocks.len();
        let k = self.block_k[b];
        // ESI の上限 (24bit) に達したら頭から作り直す (1 ブロックで 1,600 万個先。実用上は届かない)
        let room = (MAX_ESI - k) as usize;
        let id = ((self.initial_repair[b] as usize + r / blocks.len()) % room) as u32;
        blocks[b].repair_packets(id, 1).remove(0).serialize()
    }
}

/// ブロックごとのパケット列を、各ブロックの進み具合 (= 列内の位置の割合) が揃うように
/// 1 本へ混ぜる。ブロックの大きさはほぼ等しい (RaptorQ の分割は 1 シンボル差以内) が、
/// リペア数の丸めで長さが少しずれても偏らないよう、割合が最も遅れている列から取る。
fn interleave(lists: Vec<Vec<EncodingPacket>>) -> Vec<EncodingPacket> {
    if lists.len() <= 1 {
        return lists.into_iter().next().unwrap_or_default();
    }
    let total: usize = lists.iter().map(Vec::len).sum();
    let lens: Vec<usize> = lists.iter().map(Vec::len).collect();
    let mut iters: Vec<_> = lists.into_iter().map(Vec::into_iter).collect();
    let mut taken = vec![0usize; iters.len()];
    let mut out = Vec::with_capacity(total);
    while out.len() < total {
        // taken/len が最小の列 (同率なら番号の小さい列)。整数で比べる: a/la < b/lb ⇔ a*lb < b*la
        let mut best = usize::MAX;
        for i in 0..iters.len() {
            if taken[i] >= lens[i] {
                continue;
            }
            if best == usize::MAX || taken[i] * lens[best] < taken[best] * lens[i] {
                best = i;
            }
        }
        out.push(iters[best].next().expect("taken < len"));
        taken[best] += 1;
    }
    out
}

/// 必要数に達したブロックの復元に失敗したとき、次に試すまでにためる新しいシンボル数。
/// RaptorQ は必要数ちょうどで約 99%、+2 で 99.9999% 復元できるので、1〜2 回で済む。
const RETRY_AFTER_NEW_SYMBOLS: usize = 3;

/// ブロック 1 つぶんの受信状態。
struct BlockState {
    dec: SourceBlockDecoder,
    /// このブロックのソースシンボル数 (復元に要る最小数)
    k: usize,
    /// 受け取った ESI (重複を捨てるため)
    seen: HashSet<u32>,
    /// 必要数に達した後、次の試行まで手元にためているパケット
    pending: Vec<EncodingPacket>,
    /// seen がこの数に達したら復元を試す
    next_try: usize,
    out: Option<Vec<u8>>,
}

/// 受信側。raptorq の `Decoder` をそのまま使わず、ブロックごとの復元器を直接持つ。
///
/// raptorq の `Decoder` は、ブロックが必要数に達した後はパケットを 1 つ受け取るたびに
/// (重複でも) そのブロックの復元を最初から計算し直す。1 ブロック (約 5 万シンボル) の
/// 計算は PC で 0.1〜0.5 秒、スマホではその数倍かかり、30MB (15 ブロック) の受信の
/// 最後に画面とカメラが止まって完了しなくなった (2026-11-25)。
/// ここでは重複を捨て、必要数に達したときに 1 回だけまとめて試し、失敗したら
/// 新しいシンボルを数個ためてから再挑戦する。
pub struct Decoder {
    oti: ObjectTransmissionInformation,
    blocks: Vec<BlockState>,
    done_blocks: usize,
    packets_received: u32,
    result: Option<Vec<u8>>,
}

/// 受信側が受け入れる最大ペイロード長 (1 オブジェクトあたり)。
///
/// OTI の転送長は送信側 (= 他人の画面) が決める 40bit の値で、RFC 6330 の上限は約 1TB。
/// そのまま RaptorQ デコーダに渡すと、100GB 級では確保に失敗して panic する
/// (wasm ではインスタンスが壊れて以降のスキャンが全滅し、アプリでは FFI 越しの panic)。
/// なので「作れないものだけ弾く」線を引く。
///
/// 1 オブジェクトの大きさは RaptorQ の構造で決まっていて、
/// ソースブロック数 Z (8bit = 最大 256) × 1 ブロック最大 56,403 シンボル × シンボル長。
/// vcode のシンボルは 1bit セルで 42B なので 578MB、2bit で 92B なので 1,267MB が天井。
/// さらに符号化側の既定 (1 ブロック 2MB 前後) では 512MB 付近で Z が 256 を超えて
/// そもそも作れなくなる。したがって 512MB は「実際に送れるものは 1 つも弾かない」線。
///
/// これは 1 オブジェクトの上限であって、転送できる総量の上限ではない。ファイルを塊に
/// 割って塊ごとに符号化すれば (チャンク分割)、総量は保存先の空き容量まで伸ばせる。
pub const MAX_PAYLOAD_BYTES: u64 = 512 << 20;

impl Decoder {
    /// OTI からデコーダを作る。宣言された転送長が [MAX_PAYLOAD_BYTES] を超える (または 0)
    /// なら None。細工されたフレームで確保を暴走させないための入口。
    pub fn from_oti_bytes_checked(oti_bytes: &[u8; 12]) -> Option<Self> {
        let oti = ObjectTransmissionInformation::deserialize(oti_bytes);
        let len = oti.transfer_length();
        if len == 0 || len > MAX_PAYLOAD_BYTES {
            return None;
        }
        Some(Self::new(oti))
    }

    pub fn from_oti_bytes(oti_bytes: &[u8; 12]) -> Self {
        Self::new(ObjectTransmissionInformation::deserialize(oti_bytes))
    }

    /// raptorq の `Decoder::new` と同じ分割でブロックごとの復元器を作る。
    fn new(oti: ObjectTransmissionInformation) -> Self {
        let t = oti.symbol_size() as u64;
        let kt = oti.transfer_length().div_ceil(t.max(1));
        // 転送長は MAX_PAYLOAD_BYTES 以下なので、シンボル数は u32 に収まる
        let (kl, ks, zl, zs) = partition(kt.min(u32::MAX as u64) as u32, oti.source_blocks());
        let blocks = (0..zl + zs)
            .map(|i| {
                let k = if i < zl { kl } else { ks } as usize;
                BlockState {
                    dec: SourceBlockDecoder::new(i as u8, &oti, k as u64 * t),
                    k,
                    seen: HashSet::new(),
                    pending: Vec::new(),
                    next_try: k,
                    out: None,
                }
            })
            .collect();
        Self { oti, blocks, done_blocks: 0, packets_received: 0, result: None }
    }

    pub fn payload_size(&self) -> u64 {
        self.oti.transfer_length()
    }

    pub fn packets_received(&self) -> u32 {
        self.packets_received
    }

    /// シリアライズ済みパケットを 1 つ追加。完全に復元できたら Some(payload) を返す
    /// (それ以降も同じものを返す)。
    pub fn add_packet(&mut self, packet_bytes: &[u8]) -> Option<Vec<u8>> {
        self.packets_received += 1;
        if let Some(r) = &self.result {
            return Some(r.clone());
        }
        if packet_bytes.len() < 4 {
            return None;
        }
        let packet = EncodingPacket::deserialize(packet_bytes);
        let sbn = packet.payload_id().source_block_number() as usize;
        let esi = packet.payload_id().encoding_symbol_id();
        let Some(b) = self.blocks.get_mut(sbn) else { return None };
        if b.out.is_some() || !b.seen.insert(esi) {
            return None; // 復元済みのブロック・重複は計算しない
        }
        if b.seen.len() < b.k {
            // 必要数に届くまでは渡すだけ (raptorq は数を数えて何もせず返す)
            b.dec.decode(std::iter::once(packet));
            return None;
        }
        b.pending.push(packet);
        if b.seen.len() < b.next_try {
            return None;
        }
        match b.dec.decode(b.pending.drain(..)) {
            Some(out) => {
                b.out = Some(out);
                b.seen = HashSet::new(); // もう要らない (メモリを返す)
                self.done_blocks += 1;
            }
            None => {
                b.next_try = b.seen.len() + RETRY_AFTER_NEW_SYMBOLS;
                return None;
            }
        }
        if self.done_blocks < self.blocks.len() {
            return None;
        }
        let mut out = Vec::with_capacity(self.oti.transfer_length() as usize);
        for b in self.blocks.iter_mut() {
            out.extend(b.out.take().unwrap_or_default());
        }
        out.truncate(self.oti.transfer_length() as usize);
        self.result = Some(out.clone());
        Some(out)
    }

    /// 復元済みのブロック数と全ブロック数 (進捗表示用)
    pub fn blocks_done(&self) -> (usize, usize) {
        (self.done_blocks, self.blocks.len())
    }
}

#[cfg(test)]
mod cap_tests {
    use super::*;

    fn oti_bytes(transfer_len: u64) -> [u8; 12] {
        let mut b = [0u8; 12];
        b[..5].copy_from_slice(&transfer_len.to_be_bytes()[3..]);
        b[6..8].copy_from_slice(&42u16.to_be_bytes());
        b[8] = 1;
        b[9..11].copy_from_slice(&1u16.to_be_bytes());
        b[11] = 1;
        b
    }

    #[test]
    fn oversized_declared_length_is_rejected() {
        assert!(Decoder::from_oti_bytes_checked(&oti_bytes(1 << 20)).is_some());
        assert!(Decoder::from_oti_bytes_checked(&oti_bytes(MAX_PAYLOAD_BYTES)).is_some());
        assert!(Decoder::from_oti_bytes_checked(&oti_bytes(MAX_PAYLOAD_BYTES + 1)).is_none());
        assert!(Decoder::from_oti_bytes_checked(&oti_bytes(0)).is_none());
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

    /// 複数ソースブロックでも、extra_repair はオブジェクト全体の数として効き、
    /// パケットはブロックをまたいで交互に並ぶ。
    #[test]
    fn multi_block_repair_is_total_and_interleaved() {
        // シンボルを小さくしてブロック分割を起こす (vcode 1bit は 42B で 2MB 前後から分割)
        let payload: Vec<u8> = (0..600_000u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
        let k_total = payload.len().div_ceil(8);
        let encoder = Encoder::new(&payload, 8, (k_total / 2) as u32);
        let z = encoder.oti().source_blocks();
        assert!(z > 1, "テストの前提: 複数ブロックに割れること (Z={z})");
        let ratio = encoder.packet_count() as f64 / k_total as f64;
        assert!((1.49..1.52).contains(&ratio), "1 巡はデータの 1.5 倍のはず: {ratio:.3}");
        // 先頭 Z 個は全ブロックから 1 つずつ
        let mut head: Vec<u8> = (0..z as usize)
            .map(|i| EncodingPacket::deserialize(&encoder.packet(i)).payload_id().source_block_number())
            .collect();
        head.sort();
        assert_eq!(head, (0..z).collect::<Vec<_>>());
    }

    /// 複数ブロックで 30% を落としても 1 巡以内に復元できる (ブロック順だと特定ブロックが
    /// 足りず 2 巡目待ちになりやすかった)。
    #[test]
    fn multi_block_recovers_within_one_cycle_at_30pct_loss() {
        let payload: Vec<u8> = (0..600_000u32).map(|i| (i.wrapping_mul(2246822519) >> 24) as u8).collect();
        let k_total = payload.len().div_ceil(8);
        let encoder = Encoder::new(&payload, 8, (k_total / 2) as u32);
        assert!(encoder.oti().source_blocks() > 1);
        let mut decoder = Decoder::from_oti_bytes(&encoder.oti_bytes());
        let mut rng = 0x1234_5678u32;
        for i in 0..encoder.packet_count() {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            if rng % 10 < 3 {
                continue;
            }
            if let Some(out) = decoder.add_packet(&encoder.packet(i)) {
                assert_eq!(out, payload);
                return;
            }
        }
        panic!("1 巡で復元できなかった");
    }

    /// 2 巡目以降は 1 巡目と重ならない新しいリペアで、それだけでも復元できる (単一・複数ブロック)。
    #[test]
    fn later_passes_carry_fresh_repair() {
        for (len, sym) in [(20_000usize, 42u16), (600_000, 8)] {
            let payload: Vec<u8> = (0..len as u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
            let encoder = Encoder::new(&payload, sym, (len / sym as usize / 2) as u32);
            let n = encoder.packet_count();
            let first: std::collections::HashSet<Vec<u8>> =
                (0..n).map(|i| encoder.packet(i)[..4].to_vec()).collect();
            let mut decoder = Decoder::from_oti_bytes(&encoder.oti_bytes());
            let mut done = false;
            for i in n..n * 3 {
                let p = encoder.packet(i);
                assert!(!first.contains(&p[..4]), "2 巡目に 1 巡目と同じパケット ID が出た (i={i})");
                if let Some(out) = decoder.add_packet(&p) {
                    assert_eq!(out, payload);
                    done = true;
                    break;
                }
            }
            assert!(done, "2 巡目以降のパケットだけで復元できなかった (len={len})");
        }
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

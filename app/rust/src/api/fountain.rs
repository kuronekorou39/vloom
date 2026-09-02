//! Vloom Fountain コア (RaptorQ) の Flutter 向け FFI ラッパ。
//!
//! 既存の純 Rust コア `vloom_fountain` をそのまま再利用し、web 版 (core-wasm) と
//! 同じ API 形状を Dart のオパーク型として公開する。エンコーダ/デコーダは状態を持つため
//! frb の RustAutoOpaque でハンドルとして扱う (&mut self メソッドも可)。

use vloom_fountain as fountain;

/// 送信側: payload を packet_size バイトの Fountain パケット列に符号化するハンドル。
pub struct FountainEncoder {
    inner: fountain::Encoder,
}

impl FountainEncoder {
    /// payload を packet_size バイトで符号化。extra_repair はリペアパケットの追加数 (損失耐性)。
    #[flutter_rust_bridge::frb(sync)]
    pub fn new(payload: Vec<u8>, packet_size: u16, extra_repair: u32) -> FountainEncoder {
        FountainEncoder {
            inner: fountain::Encoder::new(&payload, packet_size, extra_repair),
        }
    }

    /// 受信側に渡す 12 バイトの OTI (decoder 構築に必須)。
    #[flutter_rust_bridge::frb(sync)]
    pub fn oti_bytes(&self) -> Vec<u8> {
        self.inner.oti_bytes().to_vec()
    }

    /// 生成済みパケットの総数。送信側はこの値で循環表示する (Dart 側 int)。
    #[flutter_rust_bridge::frb(sync)]
    pub fn packet_count(&self) -> u32 {
        self.inner.packet_count() as u32
    }

    /// i 番目のシリアライズ済みパケット (4 バイト ID + symbol_size バイト data)。
    #[flutter_rust_bridge::frb(sync)]
    pub fn packet(&self, i: u32) -> Vec<u8> {
        self.inner.packet(i as usize)
    }
}

/// 受信側: OTI で初期化し、パケットを投入して payload を復元するハンドル。
pub struct FountainDecoder {
    inner: fountain::Decoder,
    result: Option<Vec<u8>>,
}

impl FountainDecoder {
    /// 12 バイトの OTI から初期化する。
    #[flutter_rust_bridge::frb(sync)]
    pub fn new(oti_bytes: Vec<u8>) -> Result<FountainDecoder, String> {
        if oti_bytes.len() != 12 {
            return Err(format!("OTI must be 12 bytes, got {}", oti_bytes.len()));
        }
        let mut arr = [0u8; 12];
        arr.copy_from_slice(&oti_bytes);
        // 宣言サイズの上限を超えるものは、デコーダを作る前に弾く (細工フレーム対策)
        let inner = fountain::Decoder::from_oti_bytes_checked(&arr)
            .ok_or_else(|| "宣言されたサイズが不正です (壊れたフレームか、上限を超える大きさ)".to_string())?;
        Ok(FountainDecoder { inner, result: None })
    }

    /// パケットを 1 つ追加。復元できれば内部に payload を保存し true を返す。
    #[flutter_rust_bridge::frb(sync)]
    pub fn add_packet(&mut self, packet: Vec<u8>) -> bool {
        if self.result.is_some() {
            return true;
        }
        match self.inner.add_packet(&packet) {
            Some(r) => {
                self.result = Some(r);
                true
            }
            None => false,
        }
    }

    /// 復元済みなら payload を返す (未復元なら None)。
    #[flutter_rust_bridge::frb(sync)]
    pub fn payload(&self) -> Option<Vec<u8>> {
        self.result.clone()
    }

    /// 想定 payload サイズ (バイト)。
    #[flutter_rust_bridge::frb(sync)]
    pub fn payload_size(&self) -> u64 {
        self.inner.payload_size()
    }

    /// これまでに投入した (採用された) パケット数。
    #[flutter_rust_bridge::frb(sync)]
    pub fn packets_received(&self) -> u32 {
        self.inner.packets_received()
    }
}

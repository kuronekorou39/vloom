//! beyond-qr の WebAssembly バインディング (Phase 1: Fountain code)。
//!
//! `wasm-pack build --target web --release` で `pkg/` に JS モジュールを生成する。
//! JS 側からは `import init, { FountainEncoder, FountainDecoder } from "./pkg/beyond_qr_core_wasm.js"` で使う。

use ::beyond_qr_fountain as fountain;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn init() {}

// =============================================================
// Phase 1: Fountain code (RaptorQ) over QR streaming
// =============================================================

/// JS から見える Fountain エンコーダのハンドル。
#[wasm_bindgen]
pub struct FountainEncoder {
    inner: fountain::Encoder,
}

#[wasm_bindgen]
impl FountainEncoder {
    /// payload を packet_size byte で符号化。extra_repair はリペアパケットの追加数 (損失耐性向上)。
    #[wasm_bindgen(constructor)]
    pub fn new(payload: &[u8], packet_size: u16, extra_repair: u32) -> FountainEncoder {
        FountainEncoder {
            inner: fountain::Encoder::new(payload, packet_size, extra_repair),
        }
    }

    /// 受信側に渡す 12 byte の OTI を返す (decoder 構築に必須)。
    #[wasm_bindgen(js_name = otiBytes)]
    pub fn oti_bytes(&self) -> Vec<u8> {
        self.inner.oti_bytes().to_vec()
    }

    /// 生成済みパケットの総数。送信側はこの値で循環表示する。
    #[wasm_bindgen(js_name = packetCount)]
    pub fn packet_count(&self) -> usize {
        self.inner.packet_count()
    }

    /// i 番目のシリアライズ済みパケット (4 byte ID + symbol_size byte data)。
    pub fn packet(&self, i: usize) -> Vec<u8> {
        self.inner.packet(i)
    }
}

/// JS から見える Fountain デコーダのハンドル。
#[wasm_bindgen]
pub struct FountainDecoder {
    inner: fountain::Decoder,
    last_result: Option<Vec<u8>>,
}

#[wasm_bindgen]
impl FountainDecoder {
    /// 12 byte の OTI から初期化する。
    #[wasm_bindgen(constructor)]
    pub fn new(oti_bytes: &[u8]) -> Result<FountainDecoder, JsError> {
        if oti_bytes.len() != 12 {
            return Err(JsError::new(&format!("OTI must be 12 bytes, got {}", oti_bytes.len())));
        }
        let mut arr = [0u8; 12];
        arr.copy_from_slice(oti_bytes);
        Ok(FountainDecoder {
            inner: fountain::Decoder::from_oti_bytes(&arr),
            last_result: None,
        })
    }

    /// パケットを 1 つ追加。復元できれば内部に payload を保存し true を返す。
    #[wasm_bindgen(js_name = addPacket)]
    pub fn add_packet(&mut self, packet_bytes: &[u8]) -> bool {
        if self.last_result.is_some() {
            return true;
        }
        if let Some(result) = self.inner.add_packet(packet_bytes) {
            self.last_result = Some(result);
            true
        } else {
            false
        }
    }

    /// 復元済みなら payload を返す。
    pub fn payload(&self) -> Option<Vec<u8>> {
        self.last_result.clone()
    }

    /// 受信パケット数。
    #[wasm_bindgen(js_name = packetsReceived)]
    pub fn packets_received(&self) -> u32 {
        self.inner.packets_received()
    }

    /// 想定 payload サイズ (byte)。
    #[wasm_bindgen(js_name = payloadSize)]
    pub fn payload_size(&self) -> u64 {
        self.inner.payload_size()
    }
}

// =============================================================
// vcode (video-native 2D code) の wasm バインディング
// app/rust/src/api/vcode.rs (frb 版) と同一ロジックを wasm-bindgen で公開する。
// =============================================================
use beyond_qr_vcode as vcode;
use beyond_qr_vcode::scan::{scan_frame, scan_frame_tracked, GrayImage, Quad};

#[wasm_bindgen]
pub struct VcodeTx {
    encoder: fountain::Encoder,
    layout: vcode::Layout,
    bpc: u8,
}

#[wasm_bindgen]
impl VcodeTx {
    /// payload には先頭に CRC-32 が付与される (受信側は vcodeUnwrapPayload で検証して剥がす)。
    #[wasm_bindgen(constructor)]
    pub fn new(payload: &[u8], extra_repair: u32, grid_w: u8, grid_h: u8, bits_per_cell: u8) -> VcodeTx {
        let bpc = if bits_per_cell == 2 { 2 } else { 1 };
        let layout = vcode::Layout {
            block: 20,
            grid_w: grid_w.clamp(2, 12) as usize,
            grid_h: grid_h.clamp(2, 12) as usize,
        };
        let wrapped = vcode::wrap_payload(payload);
        VcodeTx {
            encoder: fountain::Encoder::new(&wrapped, layout.packet_size(bpc) as u16, extra_repair),
            layout,
            bpc,
        }
    }

    #[wasm_bindgen(js_name = packetCount)]
    pub fn packet_count(&self) -> u32 {
        self.encoder.packet_count() as u32
    }

    #[wasm_bindgen(js_name = frameCount)]
    pub fn frame_count(&self) -> u32 {
        let bc = self.layout.block_count();
        self.encoder.packet_count().div_ceil(bc) as u32
    }

    #[wasm_bindgen(js_name = frameWidth)]
    pub fn frame_width(&self) -> u32 {
        self.layout.width() as u32
    }

    #[wasm_bindgen(js_name = frameHeight)]
    pub fn frame_height(&self) -> u32 {
        self.layout.height() as u32
    }

    /// i 番目フレームのグレースケール画素 (0=黒,255=白, 行優先, width*height)。
    #[wasm_bindgen(js_name = frameGray)]
    pub fn frame_gray(&self, i: u32) -> Vec<u8> {
        let bc = self.layout.block_count();
        let pc = self.encoder.packet_count();
        let header = vcode::FrameHeader {
            version: vcode::VERSION,
            bits_per_cell: self.bpc,
            layout: self.layout,
            frame_seq: (i % 0x10000) as u16,
            oti: {
                let mut oti = [0u8; 12];
                oti.copy_from_slice(&self.encoder.oti_bytes());
                oti
            },
        };
        let payload_len = self.layout.block_payload_len(self.bpc);
        let blocks: Vec<Vec<u8>> = (0..bc)
            .map(|j| {
                let mut p = self.encoder.packet((i as usize * bc + j) % pc);
                p.resize(payload_len, 0);
                p
            })
            .collect();
        vcode::encode_frame(&header, &blocks, 1).data
    }
}

/// Fountain 復元結果のエンドツーエンド CRC-32 を検証して剥がす。
/// undefined = 復元結果が破損 (受信側はデコーダを作り直して続行すべき)。
#[wasm_bindgen(js_name = vcodeUnwrapPayload)]
pub fn vcode_unwrap_payload(payload: &[u8]) -> Option<Vec<u8>> {
    vcode::unwrap_payload(payload)
}

/// 元のファイル名/MIME をヘッダに埋めて送信ペイロードを作る (受信側で元名・種別を復元する用)。
/// これを VcodeTx に渡す payload とする。
#[wasm_bindgen(js_name = vcodeWrapFile)]
pub fn vcode_wrap_file(name: &str, mime: &str, data: &[u8]) -> Vec<u8> {
    vcode::wrap_file(name, mime, data)
}

/// 復元済みペイロードのファイルメタ (JS から name/mime/data を読む)。
#[wasm_bindgen]
pub struct VcodeFile {
    name: String,
    mime: String,
    data: Vec<u8>,
}

#[wasm_bindgen]
impl VcodeFile {
    #[wasm_bindgen(getter)]
    pub fn name(&self) -> String {
        self.name.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn mime(&self) -> String {
        self.mime.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn data(&self) -> Vec<u8> {
        self.data.clone()
    }
}

/// 復元済みペイロードから元のファイル名/MIME/中身を取り出す。ヘッダが無ければ undefined。
#[wasm_bindgen(js_name = vcodeUnwrapFile)]
pub fn vcode_unwrap_file(buf: &[u8]) -> Option<VcodeFile> {
    vcode::unwrap_file(buf).map(|m| VcodeFile { name: m.name, mime: m.mime, data: m.data })
}

#[wasm_bindgen]
pub struct VcodeScanReport {
    detected: bool,
    blocks_ok: u32,
    blocks_total: u32,
    oti: Vec<u8>,
    packets: Vec<Vec<u8>>,
}

#[wasm_bindgen]
impl VcodeScanReport {
    #[wasm_bindgen(getter)]
    pub fn detected(&self) -> bool {
        self.detected
    }
    #[wasm_bindgen(getter, js_name = blocksOk)]
    pub fn blocks_ok(&self) -> u32 {
        self.blocks_ok
    }
    #[wasm_bindgen(getter, js_name = blocksTotal)]
    pub fn blocks_total(&self) -> u32 {
        self.blocks_total
    }
    #[wasm_bindgen(getter)]
    pub fn oti(&self) -> Vec<u8> {
        self.oti.clone()
    }
    #[wasm_bindgen(js_name = packetCount)]
    pub fn packet_count(&self) -> u32 {
        self.packets.len() as u32
    }
    pub fn packet(&self, i: u32) -> Vec<u8> {
        self.packets[i as usize].clone()
    }
}

fn vcode_fail() -> VcodeScanReport {
    VcodeScanReport { detected: false, blocks_ok: 0, blocks_total: 0, oti: vec![], packets: vec![] }
}

fn vcode_success(result: vcode::scan::ScanResult, layout: vcode::Layout) -> VcodeScanReport {
    let frame = result.frame;
    let pkt_len = 4 + fountain::oti_symbol_size(&frame.header.oti) as usize;
    let packets: Vec<Vec<u8>> = frame
        .blocks
        .into_iter()
        .flatten()
        .map(|mut p| {
            p.truncate(pkt_len.min(p.len()));
            p
        })
        .collect();
    VcodeScanReport {
        detected: true,
        blocks_ok: packets.len() as u32,
        blocks_total: layout.block_count() as u32,
        oti: frame.header.oti.to_vec(),
        packets,
    }
}

fn vcode_rotate(y: &[u8], w: usize, h: usize, stride: usize, rot: u32) -> (Vec<u8>, usize, usize) {
    let (rw, rh) = match rot {
        90 | 270 => (h, w),
        _ => (w, h),
    };
    let mut gray = vec![0u8; rw * rh];
    for sy in 0..h {
        let row = &y[sy * stride..sy * stride + w];
        for sx in 0..w {
            let (dx, dy) = match rot {
                90 => (rw - 1 - sy, sx),
                180 => (rw - 1 - sx, rh - 1 - sy),
                270 => (sy, rh - 1 - sx),
                _ => (sx, sy),
            };
            gray[dy * rw + dx] = row[sx];
        }
    }
    (gray, rw, rh)
}

#[wasm_bindgen]
pub struct VcodeRx {
    last: Option<(u32, vcode::Layout, [(f32, f32); 4])>,
    /// 探索するレイアウトの固定指定 (None = CANDIDATES を総当たり)
    forced: Option<vcode::Layout>,
}

#[wasm_bindgen]
impl VcodeRx {
    #[wasm_bindgen(constructor)]
    pub fn new() -> VcodeRx {
        VcodeRx { last: None, forced: None }
    }

    /// 探索するレイアウトを 1 つに固定する (grid_w = 0 で解除)。送信側の格子が分かっている
    /// 場合に候補総当たりを避け、初回検出を候補数分だけ速くする。
    #[wasm_bindgen(js_name = setLayout)]
    pub fn set_layout(&mut self, grid_w: u8, grid_h: u8) {
        self.forced = if grid_w == 0 || grid_h == 0 {
            None
        } else {
            Some(vcode::Layout::from_grid(grid_w as usize, grid_h as usize))
        };
    }

    /// グレースケール Y プレーンから vcode をスキャンする。ブラウザでは RGBA→輝度に変換して
    /// stride=width, rotation_deg=0 で渡せばよい。
    pub fn scan(
        &mut self,
        y: &[u8],
        width: u32,
        height: u32,
        stride: u32,
        rotation_deg: u32,
        guide_frac: f64,
    ) -> VcodeScanReport {
        let (w, h, stride) = (width as usize, height as usize, stride as usize);
        if stride < w || y.len() < stride * h {
            return vcode_fail();
        }

        if let Some((rot, layout, corners)) = self.last {
            let (gray, rw, rh) = vcode_rotate(y, w, h, stride, rot);
            let img = GrayImage { w: rw, h: rh, data: &gray };
            if let Ok(result) = scan_frame_tracked(&img, &corners, layout) {
                self.last = Some((rot, layout, result.corners));
                return vcode_success(result, layout);
            }
        }

        // ガイド枠 (中央・正方形クロップの guide_frac 幅) を初期値にコードを探す。
        // 実機は手持ちでコードの写る大きさが一定しないため、UI が示す基準枠 (guide_frac) を
        // 中心に一回り小さい/大きいスケールも試す。基準スケールを先頭に置き、よくある構図で
        // 早期に確定させる。ロック後は self.last 経由のトラッキングに移り多スケール探索は走らない。
        let base = guide_frac.clamp(0.4, 0.98);
        let fracs = [base, (base * 0.78).max(0.4), (base * 1.15).min(0.98)];
        let cands: Vec<vcode::Layout> = match self.forced {
            Some(l) => vec![l],
            None => vcode::Layout::CANDIDATES.to_vec(),
        };
        for rot in [rotation_deg % 360, (rotation_deg + 180) % 360] {
            let (gray, rw, rh) = vcode_rotate(y, w, h, stride, rot);
            let img = GrayImage { w: rw, h: rh, data: &gray };
            let cx = rw as f32 / 2.0;
            let cy = rh as f32 / 2.0;
            for &layout in &cands {
                for &frac in &fracs {
                    let gw = (frac * rw as f64) as f32;
                    let gh = (gw * layout.height() as f32 / layout.width() as f32).min(rh as f32 * 0.95);
                    let guide = Quad {
                        tl: (cx - gw / 2.0, cy - gh / 2.0),
                        tr: (cx + gw / 2.0, cy - gh / 2.0),
                        br: (cx + gw / 2.0, cy + gh / 2.0),
                        bl: (cx - gw / 2.0, cy + gh / 2.0),
                    };
                    if let Ok(result) = scan_frame(&img, &guide, layout) {
                        self.last = Some((rot, layout, result.corners));
                        return vcode_success(result, layout);
                    }
                }
            }
        }
        self.last = None;
        vcode_fail()
    }
}

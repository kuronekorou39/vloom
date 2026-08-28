//! Vloom コアの Python バインディング (PyO3)。
//!
//! `core-wasm` (ブラウザ) / `app/rust` (Flutter) と同じ Rust コアを Python へ公開する。
//! デスクトップ送信側 (`desktop/`) はこれを呼ぶので、フレームのバイト列は PWA・アプリと
//! 完全に一致する。エンコーダを Python 側に書き起こすと実装が 3 つ目に分岐するため、
//! ここを通す。
//!
//! ビルド: `maturin develop --release -m py/Cargo.toml`

use pyo3::prelude::*;
use pyo3::types::PyBytes;

use vloom_fountain as fountain;
use vloom_vcode as vcode;

/// 送信側ハンドル。ペイロードを RaptorQ で符号化し、vcode フレームを 1 枚ずつ描く。
///
/// `core-wasm` の `VcodeTx` と同一のロジック (パケットの循環割り当て、ゼロパディング、
/// フレームヘッダの frame_seq) を持つ。
#[pyclass]
pub struct VcodeTx {
    encoder: fountain::Encoder,
    layout: vcode::Layout,
    bpc: u8,
}

#[pymethods]
impl VcodeTx {
    /// payload には先頭に CRC-32 が付与される (受信側は unwrap_payload で検証して剥がす)。
    /// extra_repair はソースブロックあたりのリペアパケット数。
    #[new]
    #[pyo3(signature = (payload, extra_repair, grid_w, grid_h, bits_per_cell))]
    fn new(payload: &[u8], extra_repair: u32, grid_w: u8, grid_h: u8, bits_per_cell: u8) -> Self {
        let bpc = if bits_per_cell == 2 { 2 } else { 1 };
        let layout = vcode::Layout {
            block: 20,
            // 上限はヘッダの u8 と、受信側が現実に読める密度で決まる。縦長格子
            // (11x14) や超密 (13x12) を通すため 20 まで許す。ここで黙って丸めると
            // 「指定した格子と違うものが出る」ので、範囲は広めに取っておく。
            grid_w: grid_w.clamp(2, 20) as usize,
            grid_h: grid_h.clamp(2, 20) as usize,
        };
        let wrapped = vcode::wrap_payload(payload);
        VcodeTx {
            encoder: fountain::Encoder::new(&wrapped, layout.packet_size(bpc) as u16, extra_repair),
            layout,
            bpc,
        }
    }

    /// 生成済みパケット総数 (source + repair)。
    #[getter]
    fn packet_count(&self) -> u32 {
        self.encoder.packet_count() as u32
    }

    /// 全パケットを載せるのに要るフレーム数。送信はこれを循環表示する。
    #[getter]
    fn frame_count(&self) -> u32 {
        self.encoder
            .packet_count()
            .div_ceil(self.layout.block_count()) as u32
    }

    /// フレームの幅 (セル数 = ピクセル数、scale=1 で描くため)。
    #[getter]
    fn frame_width(&self) -> u32 {
        self.layout.width() as u32
    }

    /// フレームの高さ (セル数)。
    #[getter]
    fn frame_height(&self) -> u32 {
        self.layout.height() as u32
    }

    /// 1 フレームあたりの実効ペイロード (バイト)。理論スループットの計算用。
    #[getter]
    fn bytes_per_frame(&self) -> u32 {
        (self.layout.block_count() * self.layout.packet_size(self.bpc)) as u32
    }

    /// i 番目のフレームのグレースケール画素 (0=黒, 255=白, 行優先, width*height)。
    /// QImage.Format_Grayscale8 にそのまま渡せる。
    fn frame_gray<'py>(&self, py: Python<'py>, i: u32) -> Bound<'py, PyBytes> {
        let block_count = self.layout.block_count();
        let packet_count = self.encoder.packet_count();
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
        let blocks: Vec<Vec<u8>> = (0..block_count)
            .map(|j| {
                let mut p = self
                    .encoder
                    .packet((i as usize * block_count + j) % packet_count);
                p.resize(payload_len, 0);
                p
            })
            .collect();
        PyBytes::new(py, &vcode::encode_frame(&header, &blocks, 1).data)
    }
}

/// 元のファイル名/MIME をヘッダに埋めて送信ペイロードを作る。
/// 受信側はこれを剥がして元の名前・種別でファイルを復元する。
#[pyfunction]
fn wrap_file<'py>(py: Python<'py>, name: &str, mime: &str, data: &[u8]) -> Bound<'py, PyBytes> {
    PyBytes::new(py, &vcode::wrap_file(name, mime, data))
}

/// レイアウト (格子 + 階調) から RaptorQ の packet_size を返す。
/// 送信前にソースパケット数を見積もってリペア数を決めるのに使う。
#[pyfunction]
fn packet_size(bits_per_cell: u8) -> u32 {
    let bpc = if bits_per_cell == 2 { 2 } else { 1 };
    vcode::Layout::V0.packet_size(bpc) as u32
}

#[pymodule]
fn vloom_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<VcodeTx>()?;
    m.add_function(wrap_pyfunction!(wrap_file, m)?)?;
    m.add_function(wrap_pyfunction!(packet_size, m)?)?;
    m.add("VCODE_VERSION", vcode::VERSION)?;
    Ok(())
}

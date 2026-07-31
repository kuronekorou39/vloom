//! vcode v0 — 動画ネイティブな独自 2D コードのフレームフォーマット。
//!
//! QR が印刷物向けに払っているコスト (ファインダー/アライメント/全か無かの RS) を捨て、
//! 画面→カメラの動画ストリーミング専用に再設計したもの。
//!
//! 設計の柱:
//!   - フレームは独立した「ブロック」の格子。各ブロックが RaptorQ パケット 1 個 + CRC-32 を持ち、
//!     部分的に壊れたフレームからも読めたブロックだけ回収できる (全か無かをやめる)
//!   - 機能パターンは四隅マーカー + 上下ストリップのみ (面積 ~12%、QR の機能パターン+EC より小さい)
//!   - フレーム内 EC は持たない。フレーム欠落・ブロック欠落は上位の fountain (RaptorQ) が吸収する
//!   - v0 は 1 bit/セル (白黒)。ヘッダに bits_per_cell を持ち、将来の輝度多値化 (2bit) に備える
//!
//! フレーム構造 (セル座標、デフォルトレイアウト 100x92):
//!   - 上ストリップ (行 0..6): 両端に 6x6 コーナーマーカー、間にヘッダ (24 byte、CRC-32 込み) のコピー
//!   - データ領域 (行 6..86): 20x20 セルのブロックが 5x4 = 20 個。ブロック = 46 byte payload + CRC-32
//!   - 下ストリップ (行 86..92): 両端にコーナーマーカー、間に市松の較正/タイミングパターン
//!
//! v1 でブロック/ヘッダの CRC を 16→32 bit に拡張した。サンプリングオフセットの
//! リトライ (25 通り) x 長時間受信では CRC-16 の偽陽性 (1/65536) が現実に発生し、
//! ゴミパケットが RaptorQ を汚染して復元結果全体を壊すため。
//!
//! この v0 デコーダは「理想チャネル」(スケール整数倍・歪みなしのビットマップ) を仮定する。
//! 実カメラ画像からの検出・射影補正・サンプリングは次段で別モジュールとして実装する。

pub mod scan;

/// 上下ストリップの高さ (セル)
pub const STRIP_H: usize = 6;
/// コーナーマーカーの一辺 (セル)
pub const CORNER: usize = 6;
/// ヘッダ先頭のマジックバイト
pub const MAGIC: u8 = 0xB9;
/// ヘッダのシリアライズ長 (CRC-32 込み)
pub const HEADER_LEN: usize = 24;
/// フォーマットバージョン (v1: CRC を 16→32 bit 化。v0 とは非互換)
pub const VERSION: u8 = 1;

/// CRC-32/ISO-HDLC (init=0xFFFFFFFF, poly=0xEDB88320 反転形, xorout=0xFFFFFFFF)
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// ブロック格子のレイアウト。ヘッダに載るのでデコーダは事前知識なしで復元できる。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// ブロックの一辺 (セル)
    pub block: usize,
    /// 横方向のブロック数
    pub grid_w: usize,
    /// 縦方向のブロック数
    pub grid_h: usize,
}

impl Layout {
    /// ブロックの一辺 (セル)。全レイアウト共通で、部分回収の粒度を決める。
    pub const BLOCK: usize = 20;

    /// デフォルトレイアウト: 100x92 セル、20 ブロック、実効 880 byte/フレーム
    pub const V0: Layout = Layout { block: Self::BLOCK, grid_w: 5, grid_h: 4 };

    /// 高密度レイアウト: 140x132 セル、42 ブロック、実効 1848 byte/フレーム。
    /// カメラ 1080p 解析前提 (720p ではセルが 4px 前後になり限界)。
    pub const V1_DENSE: Layout = Layout { block: Self::BLOCK, grid_w: 7, grid_h: 6 };

    /// 超高密度: 180x172 セル、72 ブロック。bpc=2 で 6624 byte/フレーム。
    /// 6px/セル を確保するには受信 1080px 相当が要る (= カメラ 2160p 級)。
    pub const V2_ULTRA: Layout = Layout { block: Self::BLOCK, grid_w: 9, grid_h: 8 };

    /// 最大密度: 220x212 セル、110 ブロック。bpc=2 で 10120 byte/フレーム。
    /// 6px/セル には受信 1320px 相当。4K カメラ + 近距離・固定でようやく成立する領域。
    pub const V3_MAX: Layout = Layout { block: Self::BLOCK, grid_w: 11, grid_h: 10 };

    /// 受信側がヘッダ確定前に試すレイアウト候補。実運用で当たりやすい順に並べる
    /// (総当たりは初回検出のみで、ロック後はトラッキングに移る)。
    pub const CANDIDATES: [Layout; 4] =
        [Layout::V1_DENSE, Layout::V0, Layout::V2_ULTRA, Layout::V3_MAX];

    /// 格子指定からレイアウトを作る (block は共通で BLOCK)
    pub fn from_grid(grid_w: usize, grid_h: usize) -> Layout {
        Layout { block: Self::BLOCK, grid_w, grid_h }
    }

    pub fn width(&self) -> usize {
        self.grid_w * self.block
    }

    pub fn height(&self) -> usize {
        self.grid_h * self.block + 2 * STRIP_H
    }

    pub fn block_count(&self) -> usize {
        self.grid_w * self.grid_h
    }

    /// 1 ブロックのセル数から得られる総バイト数 (bpc = bits/セル、1 or 2)
    pub fn block_bytes(&self, bpc: u8) -> usize {
        self.block * self.block * bpc as usize / 8
    }

    /// CRC-32 を除いたブロックペイロード長 (= シリアライズ済み RaptorQ パケットがそのまま入る)
    pub fn block_payload_len(&self, bpc: u8) -> usize {
        self.block_bytes(bpc) - 4
    }

    /// このレイアウトに合わせる場合の RaptorQ packet_size
    /// (シリアライズ済みパケット = 4 byte payload ID + packet_size)
    pub fn packet_size(&self, bpc: u8) -> usize {
        self.block_payload_len(bpc) - 4
    }

    /// ブロック bi (行優先) の左上セル座標 (row, col)
    pub(crate) fn block_origin(&self, bi: usize) -> (usize, usize) {
        let by = bi / self.grid_w;
        let bx = bi % self.grid_w;
        (STRIP_H + by * self.block, bx * self.block)
    }
}

/// フレームヘッダ。上ストリップに CRC 付きで複数コピー格納される。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub version: u8,
    pub bits_per_cell: u8,
    pub layout: Layout,
    /// 表示シーケンス番号 (受信統計・重複検出用)
    pub frame_seq: u16,
    /// RaptorQ の OTI (12 byte)。全フレームに載せるので受信はどのフレームからでも開始できる
    pub oti: [u8; 12],
}

impl FrameHeader {
    pub fn serialize(&self) -> [u8; HEADER_LEN] {
        let mut buf = [0u8; HEADER_LEN];
        buf[0] = MAGIC;
        buf[1] = self.version;
        buf[2] = self.bits_per_cell;
        buf[3] = self.layout.block as u8;
        buf[4] = self.layout.grid_w as u8;
        buf[5] = self.layout.grid_h as u8;
        buf[6..8].copy_from_slice(&self.frame_seq.to_le_bytes());
        buf[8..20].copy_from_slice(&self.oti);
        let crc = crc32(&buf[..20]);
        buf[20..24].copy_from_slice(&crc.to_be_bytes());
        buf
    }

    /// magic・version・CRC が一致しなければ None
    pub fn deserialize(buf: &[u8]) -> Option<Self> {
        if buf.len() < HEADER_LEN || buf[0] != MAGIC || buf[1] != VERSION {
            return None;
        }
        let crc_stored = u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]);
        if crc32(&buf[..20]) != crc_stored {
            return None;
        }
        Some(Self {
            version: buf[1],
            bits_per_cell: buf[2],
            layout: Layout {
                block: buf[3] as usize,
                grid_w: buf[4] as usize,
                grid_h: buf[5] as usize,
            },
            frame_seq: u16::from_le_bytes([buf[6], buf[7]]),
            oti: buf[8..20].try_into().unwrap(),
        })
    }
}

/// グレースケール画像 (行優先、0=黒, 255=白)
pub struct Bitmap {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Bitmap {
    pub fn new_white(w: usize, h: usize) -> Self {
        Self { w, h, data: vec![255; w * h] }
    }

    pub fn get(&self, x: usize, y: usize) -> u8 {
        self.data[y * self.w + x]
    }

    pub fn set(&mut self, x: usize, y: usize, v: u8) {
        self.data[y * self.w + x] = v;
    }

    /// セル (row, col) を scale x scale ピクセルで塗る
    fn fill_cell(&mut self, scale: usize, row: usize, col: usize, black: bool) {
        self.fill_cell_gray(scale, row, col, if black { 0 } else { 255 });
    }

    /// セル (row, col) を任意のグレー値で塗る (輝度多値用)
    fn fill_cell_gray(&mut self, scale: usize, row: usize, col: usize, v: u8) {
        for dy in 0..scale {
            for dx in 0..scale {
                self.set(col * scale + dx, row * scale + dy, v);
            }
        }
    }

    /// セル (row, col) の中心をサンプリングして黒なら true
    fn sample_cell(&self, scale: usize, row: usize, col: usize) -> bool {
        self.sample_cell_value(scale, row, col) < 128
    }

    /// セル (row, col) の中心のグレー値
    fn sample_cell_value(&self, scale: usize, row: usize, col: usize) -> u8 {
        self.get(col * scale + scale / 2, row * scale + scale / 2)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    /// ビットマップ寸法が scale の整数倍でない、またはストリップ幅すら確保できない
    BadDimensions,
    /// 探索窓内にコーナーマーカー候補が見つからない (0=TL,1=TR,2=BR,3=BL)
    CornerNotFound(u8),
    /// 四隅マーカーが期待パターンと一致しない
    CornerMismatch,
    /// どのヘッダコピーも CRC を通らなかった
    HeaderNotFound,
    /// ヘッダのレイアウトとビットマップ寸法が矛盾
    LayoutMismatch,
}

/// デコード結果。blocks[i] は CRC が通ればペイロード、壊れていれば None (部分回収)。
#[derive(Debug)]
pub struct DecodedFrame {
    pub header: FrameHeader,
    pub blocks: Vec<Option<Vec<u8>>>,
}

#[derive(Clone, Copy)]
pub(crate) enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// コーナーマーカーのパターン。外周 1 セルは全コーナー共通で黒 (検出用)、
/// 内部 4x4 はコーナーごとに異なる (将来の回転判定用)。
pub(crate) fn corner_black(which: Corner, r: usize, c: usize) -> bool {
    let border = r == 0 || r == CORNER - 1 || c == 0 || c == CORNER - 1;
    if border {
        return true;
    }
    match which {
        Corner::TopLeft => true,                                     // 塗りつぶし
        Corner::TopRight => false,                                   // 白抜きリング
        Corner::BottomLeft => (2..4).contains(&r) && (2..4).contains(&c), // 中央 2x2 のみ黒
        Corner::BottomRight => (r + c) % 2 == 0,                     // 市松
    }
}

/// 4 コーナーの (種別, 左上セル座標)
pub(crate) fn corner_origins(w: usize, h: usize) -> [(Corner, usize, usize); 4] {
    [
        (Corner::TopLeft, 0, 0),
        (Corner::TopRight, 0, w - CORNER),
        (Corner::BottomLeft, h - CORNER, 0),
        (Corner::BottomRight, h - CORNER, w - CORNER),
    ]
}

/// ヘッダ領域のセルを行優先で列挙 (上ストリップのコーナー間、行 0 のタイミング行を除く)
pub(crate) fn header_cells(w: usize) -> impl Iterator<Item = (usize, usize)> {
    (1..STRIP_H).flat_map(move |r| (CORNER..w - CORNER).map(move |c| (r, c)))
}

/// 較正パターンのセル値 (上端タイミング行と下ストリップで使用)。
/// 周期パターン (市松/BWBW) は 2 セルずれても一致してしまい位置合わせの
/// 位相トラップになるため、座標ハッシュによる擬似ランダム既知パターンを使う
/// (自己相関が鋭く、1 セルのずれでも一致率が ~50% に落ちる)。
pub(crate) fn calib_black(r: usize, c: usize) -> bool {
    let mut x = (r as u32).wrapping_mul(0x9E37_79B1) ^ (c as u32).wrapping_mul(0x85EB_CA77);
    x ^= x >> 13;
    x = x.wrapping_mul(0xC2B2_AE3D);
    x ^= x >> 16;
    x & 1 == 1
}

/// バイト列を MSB-first のビット列にする
fn byte_bits(bytes: &[u8]) -> impl Iterator<Item = bool> + '_ {
    bytes
        .iter()
        .flat_map(|&b| (0..8).map(move |j| (b >> (7 - j)) & 1 == 1))
}

/// 輝度レベル (0..4) → グレー値。レベル 0 = 黒、3 = 白。
pub(crate) fn level_gray(level: u8) -> u8 {
    [0u8, 85, 170, 255][level as usize & 3]
}

/// グレー値 → 最近傍の輝度レベル (理想チャネル用。実カメラは局所較正で正規化してから判定)
pub(crate) fn nearest_level(v: u8) -> u8 {
    match v {
        0..=42 => 0,
        43..=127 => 1,
        128..=212 => 2,
        _ => 3,
    }
}

/// ビット列 (MSB-first) をバイト列に戻す
pub(crate) fn bits_to_bytes(bits: &[bool]) -> Vec<u8> {
    bits.chunks(8)
        .map(|ch| ch.iter().fold(0u8, |acc, &b| (acc << 1) | b as u8))
        .collect()
}

/// フレームを 1 枚エンコードする。
/// blocks は各ブロックのペイロード (長さ layout.block_payload_len(1))。
/// block_count() より少ない場合、余りは全ゼロセル (CRC 不成立 = 受信側で None) で埋める。
pub fn encode_frame(header: &FrameHeader, blocks: &[Vec<u8>], scale: usize) -> Bitmap {
    let layout = header.layout;
    let (w, h) = (layout.width(), layout.height());
    assert!(scale >= 1);
    assert!(blocks.len() <= layout.block_count(), "ブロック数超過");
    assert!(
        w > 2 * CORNER + HEADER_LEN * 8 / STRIP_H,
        "ヘッダ 1 コピーも入らないレイアウト"
    );
    let mut bm = Bitmap::new_white(w * scale, h * scale);

    // コーナーマーカー
    for (which, or, oc) in corner_origins(w, h) {
        for r in 0..CORNER {
            for c in 0..CORNER {
                bm.fill_cell(scale, or + r, oc + c, corner_black(which, r, c));
            }
        }
    }

    // 上端タイミング行 (行 0、擬似ランダム較正パターン)
    for c in CORNER..w - CORNER {
        bm.fill_cell(scale, 0, c, calib_black(0, c));
    }

    // ヘッダ: 領域に入るだけコピーを繰り返す (100 セル幅なら行 1..6 に丁度 2 コピー)
    let hdr_bits: Vec<bool> = byte_bits(&header.serialize()).collect();
    let cells: Vec<(usize, usize)> = header_cells(w).collect();
    for (i, &(r, c)) in cells.iter().enumerate() {
        let bit = hdr_bits[i % hdr_bits.len()];
        // 端数領域に中途半端なコピーは書かない (白のまま)
        if i < (cells.len() / hdr_bits.len()) * hdr_bits.len() {
            bm.fill_cell(scale, r, c, bit);
        }
    }

    // 下ストリップ: 擬似ランダム較正パターン
    for r in h - STRIP_H..h {
        for c in CORNER..w - CORNER {
            bm.fill_cell(scale, r, c, calib_black(r, c));
        }
    }

    // データブロック: payload + CRC-32 を行優先で敷き詰める。
    // bpc=1: 1 セル 1 ビット (白黒)。bpc=2: 1 セル 2 ビット (輝度 4 値、MSB-first)。
    // 構造セル (コーナー/ヘッダ/較正) は bpc に関わらず白黒のまま。
    let bpc = header.bits_per_cell;
    assert!(bpc == 1 || bpc == 2, "bits_per_cell は 1 か 2");
    for bi in 0..layout.block_count() {
        let content: Vec<u8> = if bi < blocks.len() {
            assert_eq!(blocks[bi].len(), layout.block_payload_len(bpc), "ペイロード長不一致");
            let mut v = blocks[bi].clone();
            v.extend_from_slice(&crc32(&blocks[bi]).to_be_bytes());
            v
        } else {
            vec![0u8; layout.block_bytes(bpc)]
        };
        let (or, oc) = layout.block_origin(bi);
        let bits: Vec<bool> = byte_bits(&content).collect();
        for i in 0..layout.block * layout.block {
            let (r, c) = (or + i / layout.block, oc + i % layout.block);
            if bpc == 1 {
                bm.fill_cell(scale, r, c, bits[i]);
            } else {
                // 規約: bit ペア (b0,b1) MSB-first → level = b0*2+b1 → グレー値 level_gray(level)
                let level = (bits[i * 2] as u8) << 1 | bits[i * 2 + 1] as u8;
                bm.fill_cell_gray(scale, r, c, level_gray(level));
            }
        }
    }

    bm
}

/// コーナーマーカーの許容一致率。実カメラ経由では境界セルの誤りが出るため
/// 完全一致ではなくこの比率で判定する (CRC が最終防衛線なので緩くてよい)。
pub(crate) const CORNER_MATCH_MIN: f32 = 0.85;

/// 理想チャネルのビットマップからフレームをデコードする。
/// 壊れたブロックは None として返し、読めたブロックだけ回収する。
pub fn decode_frame(bm: &Bitmap, scale: usize) -> Result<DecodedFrame, FrameError> {
    if scale == 0 || bm.w % scale != 0 || bm.h % scale != 0 {
        return Err(FrameError::BadDimensions);
    }
    let (w, h) = (bm.w / scale, bm.h / scale);
    decode_from_sampler(&|r, c| bm.sample_cell_value(scale, r, c), w, h)
}

/// セル (row, col) → グレー値 のサンプラーからフレームをデコードする共通経路 (理想チャネル用)。
/// 構造セルは 128 閾値の白黒、データブロックはヘッダの bits_per_cell に応じて
/// 白黒 (1bit) または最近傍 4 値 (2bit) で読む。
pub(crate) fn decode_from_sampler(
    sample: &dyn Fn(usize, usize) -> u8,
    w: usize,
    h: usize,
) -> Result<DecodedFrame, FrameError> {
    if w < 2 * CORNER + 1 || h < 2 * STRIP_H + 1 {
        return Err(FrameError::BadDimensions);
    }
    let black = |r: usize, c: usize| sample(r, c) < 128;

    // 四隅マーカー検証 (一致率で判定)
    let mut matched = 0usize;
    let total = 4 * CORNER * CORNER;
    for (which, or, oc) in corner_origins(w, h) {
        for r in 0..CORNER {
            for c in 0..CORNER {
                if black(or + r, oc + c) == corner_black(which, r, c) {
                    matched += 1;
                }
            }
        }
    }
    if (matched as f32) < CORNER_MATCH_MIN * total as f32 {
        return Err(FrameError::CornerMismatch);
    }

    // ヘッダ: 各コピーを順に試し、最初に CRC が通ったものを採用
    let hdr_cell_bits: Vec<bool> = header_cells(w).map(|(r, c)| black(r, c)).collect();
    let copy_bits = HEADER_LEN * 8;
    let header = (0..hdr_cell_bits.len() / copy_bits)
        .find_map(|k| {
            let bytes = bits_to_bytes(&hdr_cell_bits[k * copy_bits..(k + 1) * copy_bits]);
            FrameHeader::deserialize(&bytes)
        })
        .ok_or(FrameError::HeaderNotFound)?;

    let layout = header.layout;
    let bpc = header.bits_per_cell;
    if layout.width() != w || layout.height() != h || !(bpc == 1 || bpc == 2) {
        return Err(FrameError::LayoutMismatch);
    }

    // データブロック: CRC が通ったものだけ回収
    let blocks = (0..layout.block_count())
        .map(|bi| {
            let (or, oc) = layout.block_origin(bi);
            let mut bits = Vec::with_capacity(layout.block * layout.block * bpc as usize);
            for i in 0..layout.block * layout.block {
                let (r, c) = (or + i / layout.block, oc + i % layout.block);
                if bpc == 1 {
                    bits.push(black(r, c));
                } else {
                    let level = nearest_level(sample(r, c));
                    bits.push(level & 2 != 0);
                    bits.push(level & 1 != 0);
                }
            }
            let bytes = bits_to_bytes(&bits);
            let (payload, crc) = bytes.split_at(layout.block_payload_len(bpc));
            if crc32(payload) == u32::from_be_bytes([crc[0], crc[1], crc[2], crc[3]]) {
                Some(payload.to_vec())
            } else {
                None
            }
        })
        .collect();

    Ok(DecodedFrame { header, blocks })
}

/// 送信ペイロード先頭に CRC-32 を付与する (エンドツーエンド検証用)。
/// ブロック CRC をすり抜けたゴミパケットが RaptorQ を汚染した場合に、
/// 壊れた復元結果を保存してしまう事故を受信完了時に検出する最終防衛線。
pub fn wrap_payload(data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + data.len());
    v.extend_from_slice(&crc32(data).to_be_bytes());
    v.extend_from_slice(data);
    v
}

/// wrap_payload の逆。CRC が一致しなければ None (= 復元結果が破損している)。
pub fn unwrap_payload(buf: &[u8]) -> Option<Vec<u8>> {
    if buf.len() < 4 {
        return None;
    }
    let (crc, data) = buf.split_at(4);
    if crc32(data) == u32::from_be_bytes([crc[0], crc[1], crc[2], crc[3]]) {
        Some(data.to_vec())
    } else {
        None
    }
}

/// ファイルメタ (名前 + MIME) を先頭に付けた自己記述コンテナのマジック。
/// 先頭の NUL で実ファイル (テキスト/JPEG/PNG/ZIP 等) の先頭との衝突を避ける。
pub const FILE_MAGIC: [u8; 4] = [0x00, 0x56, 0x46, 0x31]; // \0 V F 1 = "Vcode File v1"

/// unwrap_file の戻り値。元のファイル名・MIME・中身。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMeta {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
}

/// ファイル名・MIME を先頭ヘッダに付けて中身と 1 本のバイト列にする。
/// レイアウト: [MAGIC 4][ver 1][name_len u16 LE][name][mime_len u16 LE][mime][data...]。
/// vcode は生バイトしか運ばないため、これを payload として送ると受信側で元名・種別を復元できる。
pub fn wrap_file(name: &str, mime: &str, data: &[u8]) -> Vec<u8> {
    let nb = name.as_bytes();
    let mb = mime.as_bytes();
    // 名前/MIME は u16 長に収める (異常長は切り詰め。実ファイル名で起きることはない)
    let nlen = nb.len().min(u16::MAX as usize);
    let mlen = mb.len().min(u16::MAX as usize);
    let mut v = Vec::with_capacity(4 + 1 + 2 + nlen + 2 + mlen + data.len());
    v.extend_from_slice(&FILE_MAGIC);
    v.push(1); // version
    v.extend_from_slice(&(nlen as u16).to_le_bytes());
    v.extend_from_slice(&nb[..nlen]);
    v.extend_from_slice(&(mlen as u16).to_le_bytes());
    v.extend_from_slice(&mb[..mlen]);
    v.extend_from_slice(data);
    v
}

/// wrap_file の逆。マジックが無い/壊れている場合は None (= 旧形式の生バイト。
/// 受信側は従来どおり中身推測 + タイムスタンプ名にフォールバックする)。
pub fn unwrap_file(buf: &[u8]) -> Option<FileMeta> {
    if buf.len() < 7 || buf[..4] != FILE_MAGIC || buf[4] != 1 {
        return None;
    }
    let mut off = 5usize;
    let nlen = u16::from_le_bytes([buf[off], buf[off + 1]]) as usize;
    off += 2;
    if buf.len() < off + nlen + 2 {
        return None;
    }
    let name = String::from_utf8(buf[off..off + nlen].to_vec()).ok()?;
    off += nlen;
    let mlen = u16::from_le_bytes([buf[off], buf[off + 1]]) as usize;
    off += 2;
    if buf.len() < off + mlen {
        return None;
    }
    let mime = String::from_utf8(buf[off..off + mlen].to_vec()).ok()?;
    off += mlen;
    Some(FileMeta { name, mime, data: buf[off..].to_vec() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_header(frame_seq: u16) -> FrameHeader {
        FrameHeader {
            version: VERSION,
            bits_per_cell: 1,
            layout: Layout::V0,
            frame_seq,
            oti: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        }
    }

    /// 決定的な擬似ランダムペイロード
    fn test_blocks(n: usize, len: usize, seed: u8) -> Vec<Vec<u8>> {
        (0..n)
            .map(|bi| {
                (0..len)
                    .map(|i| (i as u8).wrapping_mul(31).wrapping_add(bi as u8 ^ seed))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn crc32_known_vector() {
        // CRC-32/ISO-HDLC の標準テストベクタ
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }

    #[test]
    fn file_wrap_roundtrip() {
        let data = b"\x50\x4b\x03\x04 fake docx zip bytes".to_vec();
        let wrapped = wrap_file("report.docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", &data);
        let got = unwrap_file(&wrapped).expect("ヘッダを解けるはず");
        assert_eq!(got.name, "report.docx");
        assert_eq!(got.mime, "application/vnd.openxmlformats-officedocument.wordprocessingml.document");
        assert_eq!(got.data, data);
    }

    #[test]
    fn file_wrap_empty_meta() {
        let data = vec![1u8, 2, 3];
        let wrapped = wrap_file("", "", &data);
        let got = unwrap_file(&wrapped).unwrap();
        assert_eq!(got.name, "");
        assert_eq!(got.mime, "");
        assert_eq!(got.data, data);
    }

    #[test]
    fn unwrap_file_rejects_raw_bytes() {
        // 旧形式 (マジックなしの生バイト) は None を返し、受信側が従来処理にフォールバックできる
        assert_eq!(unwrap_file(b"\xff\xd8plain jpeg start"), None);
        assert_eq!(unwrap_file(b"PK\x03\x04zip"), None);
        assert_eq!(unwrap_file(b"hello"), None);
        assert_eq!(unwrap_file(b""), None);
    }

    #[test]
    fn payload_wrap_roundtrip() {
        let data = b"hello vcode".to_vec();
        let wrapped = wrap_payload(&data);
        assert_eq!(wrapped.len(), data.len() + 4);
        assert_eq!(unwrap_payload(&wrapped), Some(data.clone()));
        // 1 バイト壊すと検出される
        let mut bad = wrapped.clone();
        bad[7] ^= 0x01;
        assert_eq!(unwrap_payload(&bad), None);
    }

    #[test]
    fn header_roundtrip() {
        let h = test_header(1234);
        let buf = h.serialize();
        assert_eq!(FrameHeader::deserialize(&buf), Some(h));
        // 1 バイト壊すと CRC で棄却される
        let mut bad = buf;
        bad[9] ^= 0x40;
        assert_eq!(FrameHeader::deserialize(&bad), None);
    }

    #[test]
    fn layout_v0_capacity() {
        let l = Layout::V0;
        assert_eq!((l.width(), l.height()), (100, 92));
        assert_eq!(l.block_count(), 20);
        assert_eq!(l.block_bytes(1), 50);
        assert_eq!(l.block_payload_len(1), 46);
        assert_eq!(l.packet_size(1), 42);
        // ヘッダ領域 = 5 * (100-12) = 440 セル → 24byte*8=192bit が 2 コピー (+56 セル余り)
        assert_eq!(header_cells(l.width()).count(), 440);
    }

    #[test]
    fn frame_roundtrip_scale1() {
        let header = test_header(7);
        let blocks = test_blocks(20, Layout::V0.block_payload_len(1), 0xA5);
        let bm = encode_frame(&header, &blocks, 1);
        let decoded = decode_frame(&bm, 1).unwrap();
        assert_eq!(decoded.header, header);
        for (i, b) in decoded.blocks.iter().enumerate() {
            assert_eq!(b.as_deref(), Some(blocks[i].as_slice()), "block {i}");
        }
    }

    #[test]
    fn frame_roundtrip_2bpc() {
        // 輝度 4 値 (2bit/セル): ブロック容量が 2 倍 (payload 96B, packet 92B) になる
        let l = Layout::V0;
        assert_eq!(l.block_bytes(2), 100);
        assert_eq!(l.block_payload_len(2), 96);
        assert_eq!(l.packet_size(2), 92);

        let mut header = test_header(12);
        header.bits_per_cell = 2;
        let blocks = test_blocks(20, l.block_payload_len(2), 0xE7);
        let bm = encode_frame(&header, &blocks, 2);
        let decoded = decode_frame(&bm, 2).unwrap();
        assert_eq!(decoded.header, header);
        for (i, b) in decoded.blocks.iter().enumerate() {
            assert_eq!(b.as_deref(), Some(blocks[i].as_slice()), "block {i}");
        }
    }

    #[test]
    fn frame_roundtrip_scale3() {
        let header = test_header(8);
        let blocks = test_blocks(20, Layout::V0.block_payload_len(1), 0x5A);
        let bm = encode_frame(&header, &blocks, 3);
        assert_eq!((bm.w, bm.h), (300, 276));
        let decoded = decode_frame(&bm, 3).unwrap();
        assert_eq!(decoded.header, header);
        for (i, b) in decoded.blocks.iter().enumerate() {
            assert_eq!(b.as_deref(), Some(blocks[i].as_slice()), "block {i}");
        }
    }

    #[test]
    fn filler_blocks_decode_as_none() {
        let header = test_header(9);
        // 20 ブロック中 5 個だけ実データ
        let blocks = test_blocks(5, Layout::V0.block_payload_len(1), 0x11);
        let bm = encode_frame(&header, &blocks, 1);
        let decoded = decode_frame(&bm, 1).unwrap();
        for i in 0..5 {
            assert!(decoded.blocks[i].is_some());
        }
        for i in 5..20 {
            assert_eq!(decoded.blocks[i], None, "filler block {i} が誤って回収された");
        }
    }

    #[test]
    fn partial_corruption_recovers_intact_blocks() {
        let header = test_header(10);
        let blocks = test_blocks(20, Layout::V0.block_payload_len(1), 0x77);
        let mut bm = encode_frame(&header, &blocks, 1);

        // ブロック格子 (bx,by) = (1..3, 1..3) の 4 ブロックを覆う黒塗り
        // = セル座標 行 26..66, 列 20..60 (データ領域は行 6 開始)
        for y in 26..66 {
            for x in 20..60 {
                bm.set(x, y, 0);
            }
        }
        let decoded = decode_frame(&bm, 1).unwrap();
        assert_eq!(decoded.header, header, "ヘッダはデータ領域の破損の影響を受けない");

        let expect_dead = [6usize, 7, 11, 12]; // by*5+bx for (1,1),(2,1),(1,2),(2,2)
        for i in 0..20 {
            if expect_dead.contains(&i) {
                assert_eq!(decoded.blocks[i], None, "block {i} は破損しているはず");
            } else {
                assert_eq!(
                    decoded.blocks[i].as_deref(),
                    Some(blocks[i].as_slice()),
                    "block {i} は無傷で回収できるはず (部分回収)"
                );
            }
        }
    }

    #[test]
    fn corner_corruption_is_detected() {
        let header = test_header(11);
        let blocks = test_blocks(20, Layout::V0.block_payload_len(1), 0x33);
        let mut bm = encode_frame(&header, &blocks, 1);
        // TL マーカー全体を白塗り (一致率 85% を下回る)
        for y in 0..CORNER {
            for x in 0..CORNER {
                bm.set(x, y, 255);
            }
        }
        assert_eq!(decode_frame(&bm, 1).unwrap_err(), FrameError::CornerMismatch);
    }
}

//! vcode (動画ネイティブ独自 2D コード) の Flutter 向け FFI ラッパ。
//!
//! 送信: VcodeTx が payload を RaptorQ で符号化し、フレーム画像 (グレースケール, scale=1)
//!       を生成する。Flutter 側は nearest-neighbor 拡大で表示するだけ。
//! 受信: カメラの Y プレーンを vcode_scan_gray に渡す。ストライド除去・回転・ガイド枠の
//!       計算は Rust 側で行い、回収できたパケット列を返す (Fountain 投入は Dart 側)。

use vloom_fountain as fountain;
use vloom_vcode as vcode;
use vloom_vcode::scan::{locate_code, scan_frame, scan_frame_tracked, scan_frame_wide, GrayImage, Quad};

/// 送信側ハンドル。payload を vcode フレーム列に変換する。
pub struct VcodeTx {
    encoder: fountain::Encoder,
    layout: vcode::Layout,
    bpc: u8,
}

/// フレーム画像 (グレースケール、1 セル = 1 ピクセル)
pub struct VcodeFrameImage {
    pub width: u32,
    pub height: u32,
    /// 0=黒, 255=白 の行優先グレースケール
    pub pixels: Vec<u8>,
}

impl VcodeTx {
    /// payload を vcode 用に符号化する。extra_repair はリペアパケット追加数。
    /// grid_w x grid_h はブロック格子 (5x4=標準, 7x6=高密度)。
    /// bits_per_cell: 1=白黒 (packet 42B), 2=輝度4値 (packet 92B)。
    /// payload には先頭に CRC-32 が付与される (受信側は vcode_unwrap_payload で検証して剥がす)。
    #[flutter_rust_bridge::frb(sync)]
    pub fn new(payload: Vec<u8>, extra_repair: u32, grid_w: u8, grid_h: u8, bits_per_cell: u8) -> VcodeTx {
        let bpc = if bits_per_cell == 2 { 2 } else { 1 };
        let layout = vcode::Layout {
            block: 20,
            grid_w: grid_w.clamp(2, 12) as usize,
            grid_h: grid_h.clamp(2, 12) as usize,
        };
        let wrapped = vcode::wrap_payload(&payload);
        VcodeTx {
            encoder: fountain::Encoder::new(&wrapped, layout.packet_size(bpc) as u16, extra_repair),
            layout,
            bpc,
        }
    }

    #[flutter_rust_bridge::frb(sync)]
    pub fn packet_count(&self) -> u32 {
        self.encoder.packet_count() as u32
    }

    /// ループ 1 周のフレーム数 (= ceil(packet_count / ブロック数))
    #[flutter_rust_bridge::frb(sync)]
    pub fn frame_count(&self) -> u32 {
        let bc = self.layout.block_count();
        (self.encoder.packet_count().div_ceil(bc)) as u32
    }

    /// i 番目のフレーム画像を生成する。パケットは循環割り当てなので全フレームが満杯になる。
    #[flutter_rust_bridge::frb(sync)]
    pub fn frame_gray(&self, i: u32) -> VcodeFrameImage {
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
        // raptorq がシンボルサイズを丸めるため、シリアライズ済みパケットが
        // ペイロード長より短いことがある → ゼロパディング (受信側は OTI 長で切り出す)
        let payload_len = self.layout.block_payload_len(self.bpc);
        let blocks: Vec<Vec<u8>> = (0..bc)
            .map(|j| {
                let mut p = self.encoder.packet((i as usize * bc + j) % pc);
                p.resize(payload_len, 0);
                p
            })
            .collect();
        let bm = vcode::encode_frame(&header, &blocks, 1);
        VcodeFrameImage {
            width: bm.w as u32,
            height: bm.h as u32,
            pixels: bm.data,
        }
    }
}

/// Fountain 復元結果のエンドツーエンド CRC-32 を検証して剥がす。
/// None = ブロック CRC をすり抜けたゴミパケットで復元結果が破損している
/// (受信側はデコーダを作り直して受信を続行すべき)。
#[flutter_rust_bridge::frb(sync)]
pub fn vcode_unwrap_payload(payload: Vec<u8>) -> Option<Vec<u8>> {
    vcode::unwrap_payload(&payload)
}

/// 元のファイル名/MIME をヘッダに埋めて送信ペイロードを作る (受信側で元名・種別を復元する用)。
/// これを VcodeTx に渡す payload とする。
#[flutter_rust_bridge::frb(sync)]
pub fn vcode_wrap_file(name: String, mime: String, data: Vec<u8>) -> Vec<u8> {
    vcode::wrap_file(&name, &mime, &data)
}

/// unwrap_file の Flutter 向けミラー。ヘッダが無ければ None (旧形式=従来処理へフォールバック)。
pub struct VcodeFile {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
}

/// 復元済みペイロードから元のファイル名/MIME/中身を取り出す。ヘッダが無ければ None。
#[flutter_rust_bridge::frb(sync)]
pub fn vcode_unwrap_file(buf: Vec<u8>) -> Option<VcodeFile> {
    vcode::unwrap_file(&buf).map(|m| VcodeFile { name: m.name, mime: m.mime, data: m.data })
}

/// スキャン結果。detected=false のとき error に理由 (デバッグログ用)。
pub struct VcodeScanReport {
    pub detected: bool,
    /// トラッキング (前フレームからの追従) で検出したか (効果検証用)
    pub tracked: bool,
    pub frame_seq: u32,
    pub oti: Vec<u8>,
    /// CRC が通ったブロックのペイロード (= シリアライズ済み RaptorQ パケット) 列
    pub packets: Vec<Vec<u8>>,
    pub blocks_ok: u32,
    pub blocks_total: u32,
    pub error: Option<String>,
    /// 検出した 4 隅 (回転後画像座標, tl,tr,br,bl の 8 値)。UI が「今どこを読んでいるか」を
    /// 毎フレーム描くために返す。ScanResult が既に持つ値を渡すだけでコストはない。
    pub corners: Vec<f32>,
    /// corners の座標系 (回転後画像の寸法と、その回転)
    pub img_w: u32,
    pub img_h: u32,
    pub rot: u32,
    /// スキャン内訳 (マイクロ秒)。実機がカメラのフレーム間隔に追従できないとき、
    /// Y プレーンの回転コピーと探索・デコードのどちらが効いているかを切り分ける。
    pub rotate_us: u32,
    pub decode_us: u32,
    /// debug_dump=true のとき、回転処理後のグレースケール画像 (PC 側解析用)
    pub debug_gray: Option<Vec<u8>>,
    pub debug_w: u32,
    pub debug_h: u32,
}

fn fail(reason: &str) -> VcodeScanReport {
    VcodeScanReport {
        detected: false,
        tracked: false,
        frame_seq: 0,
        oti: vec![],
        packets: vec![],
        blocks_ok: 0,
        blocks_total: 0,
        error: Some(reason.to_string()),
        corners: vec![],
        img_w: 0,
        img_h: 0,
        rot: 0,
        rotate_us: 0,
        decode_us: 0,
        debug_gray: None,
        debug_w: 0,
        debug_h: 0,
    }
}

fn success(
    result: vloom_vcode::scan::ScanResult,
    tracked: bool,
    layout: vcode::Layout,
    rot: u32,
    img_w: usize,
    img_h: usize,
    rotate_us: u32,
    decode_us: u32,
) -> VcodeScanReport {
    let c = result.corners;
    let corners = vec![c[0].0, c[0].1, c[1].0, c[1].1, c[2].0, c[2].1, c[3].0, c[3].1];
    let frame = result.frame;
    // ブロックペイロードはゼロパディングされていることがあるため、
    // OTI のシンボルサイズから実パケット長 (4 + symbol_size) に切り出す
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
        tracked,
        frame_seq: frame.header.frame_seq as u32,
        oti: frame.header.oti.to_vec(),
        blocks_ok: packets.len() as u32,
        blocks_total: layout.block_count() as u32,
        packets,
        error: None,
        corners,
        img_w: img_w as u32,
        img_h: img_h as u32,
        rot,
        rotate_us,
        decode_us,
        debug_gray: None,
        debug_w: 0,
        debug_h: 0,
    }
}

/// 位置合わせ (acquire) の結果。detected=true なら corners (回転後画像座標, tl,tr,br,bl の 8 値) と
/// rot・格子を seed() に渡すと、その位置に追従した状態で受信を始められる。中央ガイド枠に頼らない。
pub struct VcodeAcquireReport {
    pub detected: bool,
    /// 検出時の回転 (seed に渡す)
    pub rot: u32,
    pub grid_w: u8,
    pub grid_h: u8,
    pub blocks_ok: u32,
    pub blocks_total: u32,
    /// 回転後画像での 4 隅 [tl.x, tl.y, tr.x, tr.y, br.x, br.y, bl.x, bl.y]
    pub corners: Vec<f32>,
    /// 回転後画像の寸法 (UI が corners を表示座標へ写すのに使う)
    pub img_w: u32,
    pub img_h: u32,
}

fn fail_acquire() -> VcodeAcquireReport {
    VcodeAcquireReport {
        detected: false,
        rot: 0,
        grid_w: 0,
        grid_h: 0,
        blocks_ok: 0,
        blocks_total: 0,
        corners: vec![],
        img_w: 0,
        img_h: 0,
    }
}

/// ストライド除去 + 回転 (rot: 0/90/180/270)。戻りは (回転後画像, 幅, 高さ)。
fn rotate_y_plane(y: &[u8], w: usize, h: usize, stride: usize, rot: u32) -> (Vec<u8>, usize, usize) {
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

/// 受信側ハンドル。前フレームで成功した 4 隅と回転を保持し、
/// 次フレームは全数粗探索をスキップして追従 (トラッキング) する。
///
/// scan() の引数:
/// - stride: Y プレーンの行バイト数 (>= width)
/// - rotation_deg: 画像を起こす回転 (0/90/180/270)。Android は通常 sensorOrientation を渡す
/// - guide_frac: 回転後画像の幅に対するガイド枠幅の比 (UI のガイド枠と同じ値を渡す)
#[flutter_rust_bridge::frb(opaque)]
pub struct VcodeRx {
    /// 直近成功時の (回転 deg, レイアウト, 精密化後の 4 隅)
    last: Option<(u32, vcode::Layout, [(f32, f32); 4])>,
    /// 直近に検出できた (回転, レイアウト)。ロックが外れた後のフル探索で最初に試す。
    ///
    /// これが無いと復帰のたびに 回転2 × 候補3 × スケール3 = 18 通りを総当たりし、
    /// 実機では 1 フレーム 300〜440ms (= 2〜3fps) かかる。その間コードは何十フレームも
    /// 進むので事実上復帰できず、「合わせているのに取得が始まらない」状態になる。
    /// 構図は変わっていないことが多いので、同じ組み合わせを先に試せば 3 通りで戻れる。
    last_ok: Option<(u32, vcode::Layout)>,
    /// 探索するレイアウトの固定指定 (None = CANDIDATES を総当たり)
    forced: Option<vcode::Layout>,
}

impl VcodeRx {
    #[flutter_rust_bridge::frb(sync)]
    pub fn new() -> VcodeRx {
        VcodeRx { last: None, last_ok: None, forced: None }
    }

    /// 探索するレイアウトを 1 つに固定する (grid_w = 0 で解除)。
    /// 候補総当たりは初回検出とacquireのコストを候補数に比例して増やすので、送信側の格子が
    /// 分かっている場合 (計測・自動掃引・能力交換後) はこれで固定する。
    #[flutter_rust_bridge::frb(sync)]
    pub fn set_layout(&mut self, grid_w: u8, grid_h: u8) {
        self.forced = if grid_w == 0 || grid_h == 0 {
            None
        } else {
            Some(vcode::Layout::from_grid(grid_w as usize, grid_h as usize))
        };
    }

    /// 探索対象のレイアウト候補
    fn candidates(&self) -> Vec<vcode::Layout> {
        match self.forced {
            Some(l) => vec![l],
            None => vcode::Layout::CANDIDATES.to_vec(),
        }
    }

    /// カメラの Y プレーンから vcode をスキャンする。
    /// トラッキング成功時は report.tracked = true。
    /// 注: sync にしない。非同期 (Rust ワーカースレッド実行) にすることで
    /// UI isolate をブロックせず、カメラプレビューのカクつきを防ぐ。
    pub fn scan(
        &mut self,
        y: Vec<u8>,
        width: u32,
        height: u32,
        stride: u32,
        rotation_deg: u32,
        guide_frac: f64,
        debug_dump: bool,
    ) -> VcodeScanReport {
        let (w, h, stride) = (width as usize, height as usize, stride as usize);
        if stride < w || y.len() < stride * h {
            return fail("Y プレーン寸法不正");
        }

        // トラッキング: 前回成功した回転・レイアウト・4 隅から追従を試す
        if let Some((rot, layout, corners)) = self.last {
            let t_rot = std::time::Instant::now();
            let (gray, rw, rh) = rotate_y_plane(&y, w, h, stride, rot);
            let rotate_us = t_rot.elapsed().as_micros() as u32;
            let img = GrayImage { w: rw, h: rh, data: &gray };
            let t_dec = std::time::Instant::now();
            if let Ok(result) = scan_frame_tracked(&img, &corners, layout) {
                let decode_us = t_dec.elapsed().as_micros() as u32;
                self.last = Some((rot, layout, result.corners));
                self.last_ok = Some((rot, layout));
                return success(result, true, layout, rot, rw, rh, rotate_us, decode_us);
            }
            // 追従失敗 → フル探索へフォールバック (ロック解除はフル探索も失敗した時)
        }

        // フル探索: 回転 (指定値と 180 度違い) x レイアウト候補を順に試す。
        // レイアウトはヘッダにも載っているが、格子を張る前に既知セル座標が必要なので候補試行する。
        // ガイド枠 (中央・guide_frac 幅) を初期値にコードを探す。手持ちで写る大きさが
        // 一定しないため、UI が示す基準枠 (guide_frac) を中心に一回り小さい/大きいスケールも
        // 試す。基準スケールを先頭に置き、よくある構図で早期確定させる。ロック後はトラッキングへ。
        // 毎フレームのフル探索は軽さを最優先する。以前は 回転2 × スケール3 = 6 通りを
        // 試して 1 フレーム 300ms かかっており、カメラが 32fps で進むため
        // 「探索している間に 10 フレーム進み、位置がずれてまた探索」の悪循環になっていた。
        // 大きく外れた向き・大きさは自動 acquire (4 回転 × 多位置 × 3 スケール) が拾うので、
        // ここは「ほぼ正しい構図」だけを見ればよい。
        let base = guide_frac.clamp(0.4, 0.98);
        let fracs = [base, (base * 0.8).max(0.4)];
        let cands = self.candidates();
        let mut errors = Vec::new();

        // テクスチャからコードの位置と大きさを推定し、その 1 点を最初に試す。
        // 固定スケールのガイド枠は「画面いっぱいに大きく写した」「中心から外れた」構図を
        // 取りこぼす (実機で発生: コード幅が画像の 87% のとき、どのスケールも±48px に
        // 収まらず永久に未検出)。推定は数 ms で、偽陽性はコーナー照合が棄却する。
        if let Some((rot, layout)) = self.last_ok.or(self.forced.map(|l| (rotation_deg % 360, l))) {
            let t_rot = std::time::Instant::now();
            let (gray, rw, rh) = rotate_y_plane(&y, w, h, stride, rot);
            let rotate_us = t_rot.elapsed().as_micros() as u32;
            let img = GrayImage { w: rw, h: rh, data: &gray };
            let t_dec = std::time::Instant::now();
            if let Some((cx, cy, bw, _bh)) = locate_code(&img, 0.94) {
                let gh = bw * layout.height() as f32 / layout.width() as f32;
                let guide = Quad {
                    tl: (cx - bw / 2.0, cy - gh / 2.0),
                    tr: (cx + bw / 2.0, cy - gh / 2.0),
                    br: (cx + bw / 2.0, cy + gh / 2.0),
                    bl: (cx - bw / 2.0, cy + gh / 2.0),
                };
                if let Ok(result) = scan_frame(&img, &guide, layout) {
                    let decode_us = t_dec.elapsed().as_micros() as u32;
                    self.last = Some((rot, layout, result.corners));
                    self.last_ok = Some((rot, layout));
                    return success(result, false, layout, rot, rw, rh, rotate_us, decode_us);
                }
            }
        }

        // 近道: 直近に検出できた回転・格子だけを先に試す。構図が変わっていなければ
        // これで戻れるので、総当たり (18 通り) に落ちる頻度を大きく下げられる。
        if let Some((rot, layout)) = self.last_ok {
            let t_rot = std::time::Instant::now();
            let (gray, rw, rh) = rotate_y_plane(&y, w, h, stride, rot);
            let rotate_us = t_rot.elapsed().as_micros() as u32;
            let img = GrayImage { w: rw, h: rh, data: &gray };
            let (cx, cy) = (rw as f32 / 2.0, rh as f32 / 2.0);
            let t_dec = std::time::Instant::now();
            for &frac in &fracs {
                let gw = (frac * rw as f64) as f32;
                let gh = (gw * layout.height() as f32 / layout.width() as f32)
                    .min(rh as f32 * 0.95);
                let guide = Quad {
                    tl: (cx - gw / 2.0, cy - gh / 2.0),
                    tr: (cx + gw / 2.0, cy - gh / 2.0),
                    br: (cx + gw / 2.0, cy + gh / 2.0),
                    bl: (cx - gw / 2.0, cy + gh / 2.0),
                };
                if let Ok(result) = scan_frame(&img, &guide, layout) {
                    let decode_us = t_dec.elapsed().as_micros() as u32;
                    self.last = Some((rot, layout, result.corners));
                    return success(result, false, layout, rot, rw, rh, rotate_us, decode_us);
                }
            }
        }
        for rot in [rotation_deg % 360] {
            let t_rot = std::time::Instant::now();
            let (gray, rw, rh) = rotate_y_plane(&y, w, h, stride, rot);
            let rotate_us = t_rot.elapsed().as_micros() as u32;
            let t_dec = std::time::Instant::now();
            let img = GrayImage { w: rw, h: rh, data: &gray };
            let cx = rw as f32 / 2.0;
            let cy = rh as f32 / 2.0;

            for &layout in &cands {
                for &frac in &fracs {
                    // ガイド枠: 中央配置、幅 = frac * 画像幅、アスペクトはレイアウト準拠
                    let gw = (frac * rw as f64) as f32;
                    let gh = (gw * layout.height() as f32 / layout.width() as f32).min(rh as f32 * 0.95);
                    let guide = Quad {
                        tl: (cx - gw / 2.0, cy - gh / 2.0),
                        tr: (cx + gw / 2.0, cy - gh / 2.0),
                        br: (cx + gw / 2.0, cy + gh / 2.0),
                        bl: (cx - gw / 2.0, cy + gh / 2.0),
                    };

                    match scan_frame(&img, &guide, layout) {
                        Err(e) => errors.push(format!("rot{rot}/{}x{}:{e:?}", layout.grid_w, layout.grid_h)),
                        Ok(result) => {
                            self.last = Some((rot, layout, result.corners));
                            self.last_ok = Some((rot, layout));
                            let decode_us = t_dec.elapsed().as_micros() as u32;
                            return success(result, false, layout, rot, rw, rh, rotate_us, decode_us);
                        }
                    }
                }
            }
            if debug_dump {
                // 最初の回転の処理済み画像を添付して返す (PC 側 debug_scan での解析用)
                let mut report = fail(&errors.join(" / "));
                report.debug_gray = Some(gray);
                report.debug_w = rw as u32;
                report.debug_h = rh as u32;
                self.last = None;
                return report;
            }
        }
        self.last = None;
        fail(&errors.join(" / "))
    }

    /// 位置合わせ: 画面全体を多位置 × スケール × 全回転で sweep し、中央から外れた/傾いた
    /// コードでも初回取得する。1 回きりの重い処理なので scan() とは別 (非同期ワーカー実行)。
    /// 成功時は seed() に渡すべき rot・格子・4 隅を返す。self.last は変更しない (確認後に seed する)。
    /// thorough=false は locate_code 由来の 1 点のみ試す軽量版 (自動起動用、数十 ms)。
    /// thorough=true は従来の全 sweep も行う (手動ボタン用、数秒かかってよい)。
    /// 自動起動で全 sweep を回すと、見つからないとき数秒間フレーム処理が止まり、
    /// 「周期的にフリーズする」体感になる (実機で 7〜9 秒の停止として観測)。
    pub fn acquire(
        &mut self,
        y: Vec<u8>,
        width: u32,
        height: u32,
        stride: u32,
        rotation_deg: u32,
        thorough: bool,
    ) -> VcodeAcquireReport {
        let (w, h, stride) = (width as usize, height as usize, stride as usize);
        if stride < w || y.len() < stride * h {
            return fail_acquire();
        }
        // 90 度単位の全回転を試す (縦横入れ替え・上下逆にも対応)。取得は 1 回きりなので重くてよい。
        let rots = [
            rotation_deg % 360,
            (rotation_deg + 90) % 360,
            (rotation_deg + 180) % 360,
            (rotation_deg + 270) % 360,
        ];
        // ガイド枠の大きさ (画像幅比) と中心位置 (画像比) を振る。小さめスケールで隅寄りも拾う。
        let scales = [0.7f64, 0.5, 0.38];
        let centers = [0.5f32, 0.32, 0.68];
        // 面内の傾き (deg)。ガイド枠は水平前提なので、コードが 10〜15° を超えて傾くと
        // コーナーが粗探索の捕捉範囲 (±96px) から外れて掴めない。手持ちでは傾きが
        // 普通に起きるため、傾けた初期枠も試す。0° を先頭にして通常構図を最速で通す。
        let tilts = [0.0f32, 12.0, -12.0, 24.0, -24.0];
        // acquire は格子の固定指定 (forced) を無視して全候補を試す。
        // 格子はヘッダに書いてあり、本来ユーザーが合わせるものではない。固定は毎フレームの
        // scan() を速くするための最適化にすぎず、送信側と食い違ったとき acquire まで
        // その格子しか試さないと「見えているのに 1 つも掴めない」状態に陥る
        // (実機で発生: 受信 7x6 固定 × 送信 5x4 → 自動取得 26 連敗)。
        // forced があれば先頭に置き、一致時は最速で確定させる。
        let mut cands: Vec<vcode::Layout> = Vec::new();
        if let Some(f) = self.forced {
            cands.push(f);
        }
        for l in vcode::Layout::CANDIDATES {
            if !cands.contains(&l) {
                cands.push(l);
            }
        }
        if !cands.contains(&vcode::Layout::V3_MAX) {
            cands.push(vcode::Layout::V3_MAX);
        }
        for rot in rots {
            let (gray, rw, rh) = rotate_y_plane(&y, w, h, stride, rot);
            let img = GrayImage { w: rw, h: rh, data: &gray };
            // まずテクスチャ推定の 1 点から (スケール総当たりの穴を避ける本命経路)
            if let Some((cx, cy, bw, _bh)) = locate_code(&img, 0.94) {
                for &layout in &cands {
                    let aspect = layout.height() as f32 / layout.width() as f32;
                    let gh = bw * aspect;
                    for &deg in &tilts {
                        let (sin, cos) = deg.to_radians().sin_cos();
                        let rot_pt = |dx: f32, dy: f32| {
                            (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos)
                        };
                        let (hw2, hh2) = (bw / 2.0, gh / 2.0);
                        let guide = Quad {
                            tl: rot_pt(-hw2, -hh2),
                            tr: rot_pt(hw2, -hh2),
                            br: rot_pt(hw2, hh2),
                            bl: rot_pt(-hw2, hh2),
                        };
                        if let Ok(result) = scan_frame_wide(&img, &guide, layout) {
                            let ok = result.frame.blocks.iter().filter(|b| b.is_some()).count();
                            let c = result.corners;
                            return VcodeAcquireReport {
                                detected: true,
                                rot,
                                grid_w: layout.grid_w as u8,
                                grid_h: layout.grid_h as u8,
                                blocks_ok: ok as u32,
                                blocks_total: layout.block_count() as u32,
                                corners: vec![
                                    c[0].0, c[0].1, c[1].0, c[1].1, c[2].0, c[2].1, c[3].0, c[3].1,
                                ],
                                img_w: rw as u32,
                                img_h: rh as u32,
                            };
                        }
                    }
                }
            }
            if !thorough {
                continue; // 自動起動は locate のみ。外したら次の機会に任せる
            }
            for &layout in &cands {
                let aspect = layout.height() as f32 / layout.width() as f32;
                for &s in &scales {
                    let gw = (s * rw as f64) as f32;
                    let gh = (gw * aspect).min(rh as f32 * 0.95);
                    for &cxf in &centers {
                        for &cyf in &centers {
                            let cx = (cxf * rw as f32).clamp(gw / 2.0, rw as f32 - gw / 2.0);
                            let cy = (cyf * rh as f32).clamp(gh / 2.0, rh as f32 - gh / 2.0);
                            // 傾きは中央位置でのみ振る (9 位置 × 5 傾きは重すぎる。
                            // 傾いた構図はたいてい画面中央に収めようとしているため)。
                            let is_center = cxf == 0.5 && cyf == 0.5;
                            let tilt_set: &[f32] = if is_center { &tilts } else { &tilts[..1] };
                            for &deg in tilt_set {
                            let (sin, cos) = deg.to_radians().sin_cos();
                            let rot_pt = |dx: f32, dy: f32| {
                                (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos)
                            };
                            let (hw, hh) = (gw / 2.0, gh / 2.0);
                            let guide = Quad {
                                tl: rot_pt(-hw, -hh),
                                tr: rot_pt(hw, -hh),
                                br: rot_pt(hw, hh),
                                bl: rot_pt(-hw, hh),
                            };
                            if let Ok(result) = scan_frame_wide(&img, &guide, layout) {
                                let ok = result.frame.blocks.iter().filter(|b| b.is_some()).count();
                                let c = result.corners;
                                return VcodeAcquireReport {
                                    detected: true,
                                    rot,
                                    grid_w: layout.grid_w as u8,
                                    grid_h: layout.grid_h as u8,
                                    blocks_ok: ok as u32,
                                    blocks_total: layout.block_count() as u32,
                                    corners: vec![
                                        c[0].0, c[0].1, c[1].0, c[1].1, c[2].0, c[2].1, c[3].0, c[3].1,
                                    ],
                                    img_w: rw as u32,
                                    img_h: rh as u32,
                                };
                            }
                            }
                        }
                    }
                }
            }
        }
        fail_acquire()
    }

    /// acquire で得た (回転, 格子, 4 隅) をトラッキングの種として設定する。
    /// これ以降 scan() は最初からこの位置に追従した状態で始まる (中央ガイド枠に頼らない)。
    #[flutter_rust_bridge::frb(sync)]
    pub fn seed(&mut self, rot: u32, grid_w: u8, grid_h: u8, corners: Vec<f32>) {
        if corners.len() < 8 {
            return;
        }
        let layout = vcode::Layout::from_grid(grid_w as usize, grid_h as usize);
        // acquire で確定した組み合わせも、ロックが外れたときの近道に使う
        self.last_ok = Some((rot, layout));
        let c = [
            (corners[0], corners[1]),
            (corners[2], corners[3]),
            (corners[4], corners[5]),
            (corners[6], corners[7]),
        ];
        self.last = Some((rot, layout, c));
    }
}

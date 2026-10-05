//! 実機が保存したフレームを正解のビット列と突き合わせ、CRC に落ちたブロックが
//! 「何ビット外していたか」の分布を出す。
//!
//! ブロックは 400 セルすべてが合って初めて回収できる。落ちたブロックの大半が
//! 1〜数ビットの誤りなら、ブロック内に軽い誤り訂正を入れる価値がある。大半が
//! 大きく壊れている (混ざり帯・位置ずれ) なら、誤り訂正では救えない。形式を変える前に、
//! 手元のダンプだけでこの見込みを確かめるためのもの。
//!
//! 正解は送ったファイルから作り直す (送信アプリと同じ手順で RaptorQ のパケット列を
//! 生成する)。名前・MIME・リペア率が送信時と違うとパケットが一致しないので、
//! 回収できたブロックが正解に見つからなければその場で止める。
//!
//!     cargo run --release -p vloom-vcode --example block_errors -- \
//!         --file web/pwa/testdata/test-1MB.jpg --grid 13x18 --size 1200x1600 \
//!         scratch/dumps/*.gray
//!
//! 任意: --name <送信時のファイル名> --mime <MIME> --repair <リペア率 %、既定 50>
//!       --global-thr (比較用: 閾値を従来の「全体で 1 つ」に戻す。scan::set_row_threshold)
//!       --block-thr <0|1|2> (比較用: ブロックごとの閾値の使い方。既定 2。scan::set_block_threshold_mode)

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;

use vloom_fountain::Encoder;
use vloom_vcode::markers::locate_markers;
use vloom_vcode::scan::{
    block_samples, block_thresholds, scan_frame, scan_frame_tracked, set_row_threshold,
    set_block_threshold_mode, GrayImage, Quad,
};
use vloom_vcode::{crc32, wrap_file, wrap_payload, Layout};

/// 誤りビット数の区分 (上限値)。最後は「それ以上」
const BINS: [usize; 8] = [0, 1, 2, 3, 4, 8, 16, 40];
/// 「t ビットまで訂正できたら」を試す t
const T_LIST: [usize; 6] = [1, 2, 3, 4, 6, 8];
/// BCH で 1 ビット訂正に要る検査ビット数 (符号長 400 <= 511 = 2^9 - 1)
const BCH_BITS_PER_T: usize = 9;
/// 余裕 (閾値からの距離) が小さい順に、ブロックのこの割合を「怪しいセル」と見なす
const SUSPECT_FRAC: f32 = 0.10;

struct Args {
    file: String,
    name: Option<String>,
    mime: Option<String>,
    repair_pct: usize,
    layout: Layout,
    w: usize,
    h: usize,
    dumps: Vec<String>,
}

fn parse_args() -> Args {
    let mut a = Args {
        file: String::new(),
        name: None,
        mime: None,
        repair_pct: 50,
        layout: Layout::V6_XDENSE,
        w: 1200,
        h: 1600,
        dumps: Vec::new(),
    };
    let pair = |s: &str| -> (usize, usize) {
        let (x, y) = s.split_once('x').expect("13x18 / 1200x1600 の形で");
        (x.parse().unwrap(), y.parse().unwrap())
    };
    let mut it = env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| panic!("{arg} に値が要る"));
        match arg.as_str() {
            "--file" => a.file = val(),
            "--name" => a.name = Some(val()),
            "--mime" => a.mime = Some(val()),
            "--repair" => a.repair_pct = val().parse().unwrap(),
            "--grid" => {
                let (gw, gh) = pair(&val());
                a.layout = Layout::from_grid(gw, gh);
            }
            "--size" => (a.w, a.h) = pair(&val()),
            "--global-thr" => set_row_threshold(false),
            "--block-thr" => set_block_threshold_mode(val().parse().unwrap()),
            _ => a.dumps.push(arg),
        }
    }
    if a.file.is_empty() || a.dumps.is_empty() {
        eprintln!(
            "usage: block_errors --file <送ったファイル> [--name n] [--mime m] [--repair 50] \
             [--grid 13x18] [--size 1200x1600] <dump.gray>..."
        );
        std::process::exit(2);
    }
    a
}

/// 送信アプリ (Python の mimetypes) が付けるはずの MIME。計測に使う種類だけ
fn guess_mime(name: &str) -> &'static str {
    match name.rsplit('.').next().map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

/// ブロックに描かれるビット列 (ペイロード + CRC-32、MSB-first、true = 黒)
fn block_bits(payload: &[u8]) -> Vec<bool> {
    let mut bytes = payload.to_vec();
    bytes.extend_from_slice(&crc32(payload).to_be_bytes());
    bytes.iter().flat_map(|&b| (0..8).rev().map(move |k| (b >> k) & 1 == 1)).collect()
}

#[derive(Default)]
struct Tally {
    frames: usize,
    blocks: usize,
    ok: usize,
    /// 落ちたブロックの誤りビット数 (±0.5 セルの範囲で最良のオフセット)
    near: Vec<usize>,
    /// 同じく ±1.5 セルまで広げたとき
    wide: Vec<usize>,
    /// 少数誤りブロックの誤りセルについて: (誤りセル数, うち怪しいセルに入っていた数)
    err_cells: usize,
    err_suspect: usize,
    /// 誤りセル / 全セル の、上下左右で正解が逆の隣の数 (孤立したセルほど大きい)
    err_opp: usize,
    err_nb: usize,
    all_opp: usize,
    all_nb: usize,
}

fn main() {
    let args = parse_args();
    let layout = args.layout;
    let n_cells = layout.block * layout.block;

    // 正解のパケット列を、送信アプリ (desktop/app.py) と同じ手順で作る
    let data = fs::read(&args.file).expect("送ったファイルが読めない");
    let name = args.name.clone().unwrap_or_else(|| {
        Path::new(&args.file).file_name().unwrap().to_string_lossy().into_owned()
    });
    let mime = args.mime.clone().unwrap_or_else(|| guess_mime(&name).to_string());
    let source = wrap_file(&name, &mime, &data);
    let packet_size = layout.packet_size(1);
    let extra_repair = (source.len().div_ceil(packet_size) * args.repair_pct).div_ceil(100);
    let enc = Encoder::new(&wrap_payload(&source), packet_size as u16, extra_repair as u32);
    let pc = enc.packet_count();
    let bc = layout.block_count();
    let payload_len = layout.block_payload_len(1);
    let truth: Vec<Vec<u8>> = (0..pc)
        .map(|i| {
            let mut p = enc.packet(i);
            p.resize(payload_len, 0);
            p
        })
        .collect();
    let index: HashMap<&[u8], usize> =
        truth.iter().enumerate().map(|(i, p)| (p.as_slice(), i)).collect();
    println!(
        "正解: {name} ({mime}) {} byte / パケット {pc} (リペア {extra_repair}) / 1 巡 {} 枚",
        data.len(),
        pc.div_ceil(bc)
    );

    // オフセットの候補: 復号が試す ±0.5 セルと、位置ずれを見るための ±1.5 セル
    let grid = |half: i32| -> Vec<(f32, f32)> {
        (-half..=half)
            .flat_map(|iy| (-half..=half).map(move |ix| (ix as f32 * 0.25, iy as f32 * 0.25)))
            .collect()
    };
    let (near_offs, wide_offs) = (grid(2), grid(6));

    let mut total = Tally::default();
    // 直接検出できた枚の四隅。先に全部の枚から集めておき、失敗した枚の初期値に使う
    let mut seeds: Vec<[(f32, f32); 4]> = Vec::new();
    for path in &args.dumps {
        let Ok(raw) = fs::read(path) else { continue };
        if raw.len() < args.w * args.h {
            continue;
        }
        let img = GrayImage { w: args.w, h: args.h, data: &raw };
        if let Some(r) = locate_markers(&img).and_then(|q| scan_frame(&img, &q, layout).ok()) {
            seeds.push(r.corners);
        }
    }
    for path in &args.dumps {
        let raw = fs::read(path).expect("ダンプが読めない");
        if raw.len() < args.w * args.h {
            eprintln!("{path}: {}x{} に足りない ({} byte)、飛ばす", args.w, args.h, raw.len());
            continue;
        }
        let img = GrayImage { w: args.w, h: args.h, data: &raw };
        // 実機と同じくマーカー直接検出 → フル探索 → 追従 (定常経路) の順。
        // 実機は前の枚の四隅から追従しているので、1 枚だけ渡されると直接検出に
        // 失敗する枚がある (手持ちで多い)。そこを捨てると標本が読みやすい枚に偏るので、
        // 同じ撮影の他の枚で決まった四隅を初期値にして探し直す
        let direct = locate_markers(&img).and_then(|q| scan_frame(&img, &q, layout).ok());
        let seeded = || {
            seeds.iter().rev().find_map(|c: &[(f32, f32); 4]| {
                let q = Quad { tl: c[0], tr: c[1], br: c[2], bl: c[3] };
                scan_frame(&img, &q, layout).ok()
            })
        };
        let scanned = direct
            .or_else(seeded)
            .and_then(|r| scan_frame_tracked(&img, &r.corners, layout).ok());
        if let Some(r) = &scanned {
            seeds.push(r.corners);
        }
        let Some(r) = scanned else {
            println!("{path}: 検出できない (集計に入れない)");
            continue;
        };
        if r.frame.header.bits_per_cell != 1 {
            println!("{path}: 1bit ではない (集計に入れない)");
            continue;
        }
        assert_eq!(
            r.frame.header.oti,
            enc.oti_bytes(),
            "{path}: OTI が正解と違う。--file / --name / --mime / --repair が送信時と合っていない"
        );

        // 回収できたブロックから「このフレームがパケット列のどこか」を割り出す。
        // ブロック j に載るのはパケット (i*bc + j) % pc なので、(p - j) % pc が枚ごとに決まる。
        // 切り替わりをまたいだ 1 枚には前後 2 枚ぶんが写るので、候補は複数になる
        let mut bases: Vec<usize> = Vec::new();
        for (j, b) in r.frame.blocks.iter().enumerate() {
            let Some(b) = b else { continue };
            let p = *index.get(b.as_slice()).unwrap_or_else(|| {
                panic!("{path}: 回収したブロック {j} が正解に無い。送信時の指定と合っていない")
            });
            let base = (p + pc - j % pc) % pc;
            if !bases.contains(&base) {
                bases.push(base);
            }
        }
        if bases.is_empty() {
            println!("{path}: 回収 0 (どの枚か決められないので集計に入れない)");
            continue;
        }
        // 回収が片側だけのときに備えて、前後の枚も候補に入れる
        for b in bases.clone() {
            for nb in [(b + bc) % pc, (b + pc - bc % pc) % pc] {
                if !bases.contains(&nb) {
                    bases.push(nb);
                }
            }
        }

        let mut t = Tally { frames: 1, blocks: bc, ..Default::default() };
        let mut map = String::new();
        for (j, b) in r.frame.blocks.iter().enumerate() {
            if j % layout.grid_w == 0 {
                map.push_str("\n  ");
            }
            if b.is_some() {
                t.ok += 1;
                map.push('#');
                continue;
            }
            let thr = block_thresholds(&img, &r.homography, layout, j);
            let cands: Vec<Vec<bool>> =
                bases.iter().map(|&base| block_bits(&truth[(base + j) % pc])).collect();
            // (誤りビット数, サンプル値, 正解) が最小の組を、オフセットの範囲ごとに探す
            let best = |offs: &[(f32, f32)]| -> (usize, Vec<f32>, usize) {
                let mut best = (usize::MAX, Vec::new(), 0usize);
                for &(dx, dy) in offs {
                    let v = block_samples(&img, &r.homography, layout, j, dx, dy);
                    for (ci, bits) in cands.iter().enumerate() {
                        let d = v
                            .iter()
                            .zip(&thr)
                            .zip(bits)
                            .filter(|&((&s, &t), &b)| (s < t) != b)
                            .count();
                        if d < best.0 {
                            best = (d, v.clone(), ci);
                        }
                    }
                }
                best
            };
            let (dn, v, ci) = best(&near_offs);
            let (dw, _, _) = best(&wide_offs);
            t.near.push(dn);
            t.wide.push(dw);
            map.push(match dn {
                0..=9 => char::from_digit(dn as u32, 10).unwrap(),
                10..=40 => '+',
                _ => '.',
            });

            // 少数誤りのブロックだけ、誤りセルの素性を見る (大きく壊れたものは別の現象)
            if (1..=*T_LIST.last().unwrap()).contains(&dn) {
                let bits = &cands[ci];
                let mut order: Vec<usize> = (0..n_cells).collect();
                order.sort_by(|&a, &b| {
                    (v[a] - thr[a]).abs().partial_cmp(&(v[b] - thr[b]).abs()).unwrap()
                });
                let n_suspect = (n_cells as f32 * SUSPECT_FRAC) as usize;
                let suspect = &order[..n_suspect];
                for i in 0..n_cells {
                    let (row, col) = (i / layout.block, i % layout.block);
                    let mut nb = 0;
                    let mut opp = 0;
                    let mut look = |ok: bool, k: usize| {
                        if ok {
                            nb += 1;
                            opp += (bits[k] != bits[i]) as usize;
                        }
                    };
                    look(row > 0, i.wrapping_sub(layout.block));
                    look(row + 1 < layout.block, i + layout.block);
                    look(col > 0, i.wrapping_sub(1));
                    look(col + 1 < layout.block, i + 1);
                    t.all_opp += opp;
                    t.all_nb += nb;
                    if (v[i] < thr[i]) != bits[i] {
                        t.err_cells += 1;
                        t.err_suspect += suspect.contains(&i) as usize;
                        t.err_opp += opp;
                        t.err_nb += nb;
                    }
                }
            }
        }
        println!(
            "\n{path}: seq={} 回収 {}/{} ({:.0}%)",
            r.frame.header.frame_seq,
            t.ok,
            bc,
            t.ok as f64 * 100.0 / bc as f64
        );
        println!("  地図 (# 回収 / 数字 = 誤りビット数 / + 10〜40 / . それ以上):{map}");
        report(&t, n_cells);
        total.frames += 1;
        total.blocks += t.blocks;
        total.ok += t.ok;
        total.near.extend(t.near);
        total.wide.extend(t.wide);
        total.err_cells += t.err_cells;
        total.err_suspect += t.err_suspect;
        total.err_opp += t.err_opp;
        total.err_nb += t.err_nb;
        total.all_opp += t.all_opp;
        total.all_nb += t.all_nb;
    }
    if total.frames > 1 {
        println!("\n==== 合計 ({} 枚) ====", total.frames);
        report(&total, n_cells);
    }
}

fn report(t: &Tally, n_cells: usize) {
    let failed = t.near.len();
    if failed == 0 {
        println!("  落ちたブロックなし");
        return;
    }
    let hist = |d: &[usize]| -> String {
        let mut out = Vec::new();
        let mut lo = 0usize;
        for &hi in BINS.iter() {
            let n = d.iter().filter(|&&x| x >= lo && x <= hi).count();
            let label = if lo == hi { format!("{hi}") } else { format!("{lo}-{hi}") };
            out.push(format!("{label}:{n}"));
            lo = hi + 1;
        }
        out.push(format!("{lo}+:{}", d.iter().filter(|&&x| x >= lo).count()));
        out.join("  ")
    };
    println!("  落ちた {failed} ブロックの誤りビット数 (400 セル中)");
    println!("    ±0.5 セル内の最良: {}", hist(&t.near));
    println!("    ±1.5 セル内の最良: {}", hist(&t.wide));

    // t ビット訂正を入れた場合の見込み。検査ビットのぶんペイロードが減るので、
    // 回収率の伸びと差し引きした正味 (今の回収量を 1 とした比) も出す
    let payload_bits = n_cells - 32;
    println!("  t ビット訂正があった場合 (検査 {BCH_BITS_PER_T}t ビット、±0.5 セル内):");
    for &tt in T_LIST.iter() {
        let saved = t.near.iter().filter(|&&d| d <= tt).count();
        let rate = (t.ok + saved) as f64 / t.blocks as f64;
        let net = (t.ok + saved) as f64 * (payload_bits - BCH_BITS_PER_T * tt) as f64
            / (t.ok.max(1) as f64 * payload_bits as f64);
        println!(
            "    t={tt}: 救える {saved:>4}  回収率 {:.1}% → {:.1}%  正味 x{net:.2}",
            t.ok as f64 * 100.0 / t.blocks as f64,
            rate * 100.0
        );
    }
    if t.err_cells > 0 {
        println!(
            "  少数誤り (1〜{} ビット) のブロックの誤りセル {} 個:",
            T_LIST.last().unwrap(),
            t.err_cells
        );
        println!(
            "    閾値に近い下位 {:.0}% のセルに入っていた: {:.0}%  (高ければ「怪しいセル」を当てにできる)",
            SUSPECT_FRAC * 100.0,
            t.err_suspect as f64 * 100.0 / t.err_cells as f64
        );
        println!(
            "    正解が逆の隣の割合: 誤りセル {:.0}% / 全セル {:.0}%  (差が大きければ孤立セルがボケで潰れている)",
            t.err_opp as f64 * 100.0 / t.err_nb.max(1) as f64,
            t.all_opp as f64 * 100.0 / t.all_nb.max(1) as f64
        );
    }
}

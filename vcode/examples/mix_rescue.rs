//! 切り替わりで混ざったブロックを「片方の枚が手元にあれば」読めるかを、連続した実機
//! ダンプで再現する。
//!
//! 送信フレームの切り替わりにかかった行では、前後 2 枚 (A, B) が足し合わさって写る。
//! A のビット列が分かっていれば、セルを「A が黒」「A が白」の 2 組に分けて、組ごとに
//! 閾値を引くだけで B が読める (混合比を推定しなくてよい)。正解を A として与えた上限は
//! 落ちたブロックの 8 割 (docs/measurements/2026-10-05-block-errors.md)。
//!
//! ここでは受信側が実際に持っているものだけを使う。ブロックの位置ごとに「直近に読めた
//! 中身」を覚えておき、落ちたブロックではそれを A として試す。読めたものはまた覚えるので、
//! 続けて混ざる行でも順に辿れる。試験フレーム (vcode::probe) を撮ったダンプを使うので、
//! 読めたブロックが正しいかは中身を作り直して確かめられる。
//!
//!     adb shell am start ... --es grid auto --ei dumpall 80 --ei dumpevery 1
//!     cargo run --release --features parallel -p vloom-vcode --example mix_rescue -- \
//!         13x18 scratch/dumps/seq-13x18-30
//!
//! 画像は 1200x1600、ファイル名は vcode_all<番号>_... (番号順が撮影順) を前提にしている。

use std::collections::HashSet;
use std::env;
use std::fs;

use vloom_vcode::markers::locate_markers;
use vloom_vcode::probe::{probe_block_payload, ProbeBlock, ProbeTag};
use vloom_vcode::scan::{block_samples, scan_frame, scan_frame_tracked_with, GrayImage};
use vloom_vcode::{crc32, Layout};

const W: usize = 1200;
const H: usize = 1600;
/// ブロックの位置ごとに覚えておく「直近に読めた中身」の数
const MEMORY: usize = 2;
/// 1 ブロックが本番で運ぶバイト数
const BLOCK_BYTES: usize = 42;

/// ブロックに描かれるビット列 (ペイロード + CRC-32、MSB-first、true = 黒)
fn block_bits(payload: &[u8]) -> Vec<bool> {
    let mut bytes = payload.to_vec();
    bytes.extend_from_slice(&crc32(payload).to_be_bytes());
    bytes.iter().flat_map(|&b| (0..8).rev().map(move |k| (b >> k) & 1 == 1)).collect()
}

/// 値の集まりを 2 つに分ける閾値 (Otsu)。分けられなければ None
fn split_threshold(v: &[f32]) -> Option<f32> {
    if v.len() < 16 {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let total: f32 = s.iter().sum();
    let (mut best, mut thr, mut acc) = (-1.0f32, 0.0f32, 0.0f32);
    for i in 1..s.len() {
        acc += s[i - 1];
        let (m0, m1) = (acc / i as f32, (total - acc) / (s.len() - i) as f32);
        let var = i as f32 * (s.len() - i) as f32 * (m0 - m1) * (m0 - m1);
        if var > best {
            best = var;
            thr = (s[i - 1] + s[i]) / 2.0;
        }
    }
    Some(thr)
}

/// known を既知の枚として、samples からもう 1 枚のペイロードを読む。CRC が通れば返す
fn decode_given(samples: &[f32], known: &[bool], payload_len: usize) -> Option<Vec<u8>> {
    let pick = |black: bool| -> Vec<f32> {
        samples.iter().zip(known).filter(|&(_, &k)| k == black).map(|(&s, _)| s).collect()
    };
    let (t1, t0) = (split_threshold(&pick(true))?, split_threshold(&pick(false))?);
    let bits: Vec<bool> =
        samples.iter().zip(known).map(|(&s, &k)| s < if k { t1 } else { t0 }).collect();
    let bytes: Vec<u8> =
        bits.chunks(8).map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b as u8)).collect();
    let (payload, crc) = bytes.split_at(payload_len);
    (crc32(payload) == u32::from_be_bytes([crc[0], crc[1], crc[2], crc[3]])).then(|| payload.to_vec())
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: mix_rescue <grid> <ダンプのディレクトリ>");
        std::process::exit(2);
    }
    let (gw, gh) = args[1].split_once('x').expect("格子は 13x18 の形で");
    let layout = Layout::from_grid(gw.parse().unwrap(), gh.parse().unwrap());
    let payload_len = layout.block_payload_len(1);
    let bc = layout.block_count();

    // 撮影順 (ファイル名の番号順) に並べる
    let mut files: Vec<(u32, String)> = fs::read_dir(&args[2])
        .expect("ディレクトリが読めない")
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let n = name.strip_prefix("vcode_all")?.split('_').next()?.parse().ok()?;
            Some((n, e.path().to_string_lossy().into_owned()))
        })
        .collect();
    files.sort();

    let mut corners: Option<[(f32, f32); 4]> = None;
    let mut offsets: Option<Vec<Option<(f32, f32)>>> = None;
    let mut memory: Vec<Vec<Vec<u8>>> = vec![Vec::new(); bc];
    let (mut base, mut plus): (HashSet<(u32, u16)>, HashSet<(u32, u16)>) = Default::default();
    let (mut frames_lo, mut frames_hi, mut fps) = (u32::MAX, 0u32, 0u8);
    let (mut images, mut failed, mut tried, mut rescued, mut wrong) = (0, 0, 0, 0, 0);

    for (_, path) in &files {
        let raw = fs::read(path).expect("読めない");
        if raw.len() < W * H {
            continue;
        }
        let img = GrayImage { w: W, h: H, data: &raw };
        // 実機と同じく、前の枚の四隅から追従し、切れたらマーカーから掴み直す
        let scanned = corners
            .and_then(|c| scan_frame_tracked_with(&img, &c, layout, offsets.as_deref()).ok())
            .or_else(|| locate_markers(&img).and_then(|q| scan_frame(&img, &q, layout).ok()));
        let Some(r) = scanned else {
            corners = None;
            continue;
        };
        let Some(tag) = ProbeTag::from_oti(&r.frame.header.oti) else {
            eprintln!("{path}: 試験フレームではない");
            continue;
        };
        images += 1;
        fps = tag.fps;
        let mut note = |p: &[u8], set: &mut HashSet<(u32, u16)>| {
            if let Some(id) = ProbeBlock::parse(p) {
                frames_lo = frames_lo.min(id.frame);
                frames_hi = frames_hi.max(id.frame);
                set.insert((id.frame, id.block));
            }
        };
        let mut learned: Vec<(usize, Vec<u8>)> = Vec::new();
        for (bi, b) in r.frame.blocks.iter().enumerate() {
            if let Some(p) = b {
                note(p, &mut base);
                note(p, &mut plus);
                learned.push((bi, p.clone()));
                continue;
            }
            failed += 1;
            if memory[bi].is_empty() {
                continue;
            }
            tried += 1;
            // オフセットは前の枚でこの位置が通ったものを中心に、まわり 0.25 セル
            let (ox, oy) = offsets.as_ref().and_then(|o| o[bi]).unwrap_or((0.0, 0.0));
            let mut hit: Option<Vec<u8>> = None;
            'search: for (dx, dy) in [(0.0, 0.0), (0.25, 0.0), (-0.25, 0.0), (0.0, 0.25), (0.0, -0.25)] {
                let v = block_samples(&img, &r.homography, layout, bi, ox + dx, oy + dy);
                for known in memory[bi].iter().rev() {
                    if let Some(p) = decode_given(&v, &block_bits(known), payload_len) {
                        if &p != known {
                            hit = Some(p);
                            break 'search;
                        }
                    }
                }
            }
            if let Some(p) = hit {
                rescued += 1;
                // 正しいか: 素性から中身を作り直して突き合わせる
                let ok = ProbeBlock::parse(&p)
                    .map(|id| probe_block_payload(layout, tag, id.frame, id.block as usize) == p && id.block as usize == bi)
                    .unwrap_or(false);
                if ok {
                    note(&p, &mut plus);
                    learned.push((bi, p));
                } else {
                    wrong += 1;
                }
            }
        }
        for (bi, p) in learned {
            let m = &mut memory[bi];
            if !m.contains(&p) {
                m.push(p);
                if m.len() > MEMORY {
                    m.remove(0);
                }
            }
        }
        corners = Some(r.corners);
        offsets = Some(r.block_offsets);
    }

    if images == 0 || frames_hi < frames_lo {
        println!("読めた枚がない");
        return;
    }
    let span = (frames_hi - frames_lo + 1) as f64;
    let kbps = |n: usize| n as f64 * BLOCK_BYTES as f64 * fps as f64 / span / 1024.0;
    println!(
        "{} 枚 (送信 {fps}fps、フレーム {frames_lo}〜{frames_hi} = {span:.0} 枚ぶん)",
        images
    );
    println!("  落ちたブロック {failed} / 覚えがあって試した {tried} / 読めた {rescued} (うち誤り {wrong})");
    println!(
        "  重複なしのブロック: 従来 {} → 救済あり {}  ({:+.1}%)",
        base.len(),
        plus.len(),
        (plus.len() as f64 / base.len().max(1) as f64 - 1.0) * 100.0
    );
    println!("  実効: 従来 {:.1} KB/s → 救済あり {:.1} KB/s", kbps(base.len()), kbps(plus.len()));
}

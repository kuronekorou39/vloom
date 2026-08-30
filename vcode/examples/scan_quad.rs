//! 実機由来の raw グレー画像に対し、四隅と格子を手で与えてスキャンする。
//!
//! 「画像は綺麗なのに掴めない」とき、探索 (位置推定) が悪いのか照合・復号が悪いのかを
//! 切り分ける。真の四隅を与えて scan_frame が通れば探索側、通らなければ照合側。
//!
//!     cargo run --release -p vloom-vcode --example scan_quad -- \
//!         <path> <w> <h> <grid_w> <grid_h> <tlx> <tly> <trx> <try> <brx> <bry> <blx> <bly>
use std::env;
use std::fs;
use vloom_vcode::markers::locate_markers;
use vloom_vcode::scan::{locate_code, scan_frame, scan_frame_wide, GrayImage, Quad};
use vloom_vcode::Layout;

fn main() {
    let a: Vec<String> = env::args().collect();
    if a.len() < 14 {
        eprintln!("引数: <path> <w> <h> <grid_w> <grid_h> <tlx> <tly> <trx> <try> <brx> <bry> <blx> <bly>");
        std::process::exit(2);
    }
    let (w, h) = (a[2].parse::<usize>().unwrap(), a[3].parse::<usize>().unwrap());
    let layout = Layout::from_grid(a[4].parse().unwrap(), a[5].parse().unwrap());
    let f = |i: usize| a[i].parse::<f32>().unwrap();
    let quad = Quad { tl: (f(6), f(7)), tr: (f(8), f(9)), br: (f(10), f(11)), bl: (f(12), f(13)) };
    let data = fs::read(&a[1]).unwrap();
    assert_eq!(data.len(), w * h, "raw サイズ不一致");
    let img = GrayImage { w, h, data: &data };
    println!("格子 {}x{} = {}x{} セル, 画像 {w}x{h}", layout.grid_w, layout.grid_h, layout.width(), layout.height());

    let report = |name: &str, r: Result<vloom_vcode::scan::ScanResult, vloom_vcode::FrameError>| match r {
        Ok(r) => {
            let ok = r.frame.blocks.iter().filter(|b| b.is_some()).count();
            println!("  {name}: 検出  blocks {}/{}  seq={}  corners {:?}",
                ok, layout.block_count(), r.frame.header.frame_seq, r.corners);
            // どのブロックが落ちたかの地図 (# = 回収, . = 失敗)。位置合わせのずれなら
            // 片側や帯状に、物理要因 (ピント/帯) なら領域として現れる
            for row in r.frame.blocks.chunks(layout.grid_w) {
                let line: String = row.iter().map(|b| if b.is_some() { '#' } else { '.' }).collect();
                println!("      {line}");
            }
        }
        Err(e) => println!("  {name}: {e:?}"),
    };

    // REPEAT=N で scan_frame の所要時間を測る (並列化の効果や段階ごとの比率を見る)
    if let Ok(n) = std::env::var("REPEAT").map(|v| v.parse::<usize>().unwrap_or(0)) {
        if n > 0 {
            let t = std::time::Instant::now();
            for _ in 0..n {
                let _ = scan_frame(&img, &quad, layout);
            }
            println!("scan_frame x{n}: 平均 {:.2} ms", t.elapsed().as_secs_f64() * 1000.0 / n as f64);
            let t = std::time::Instant::now();
            for _ in 0..n {
                let _ = vloom_vcode::scan::scan_frame_tracked(&img, &[quad.tl, quad.tr, quad.br, quad.bl], layout);
            }
            println!("scan_frame_tracked x{n}: 平均 {:.2} ms", t.elapsed().as_secs_f64() * 1000.0 / n as f64);
        }
    }
    println!("== 手で与えた四隅 ==");
    report("scan_frame (±48)", scan_frame(&img, &quad, layout));
    report("scan_frame_wide (±96)", scan_frame_wide(&img, &quad, layout));

    // 少しずらした四隅でも掴めるか (探索の余裕を見る)
    for d in [20.0f32, 40.0, 60.0] {
        let q = Quad {
            tl: (quad.tl.0 - d, quad.tl.1 - d),
            tr: (quad.tr.0 + d, quad.tr.1 - d),
            br: (quad.br.0 + d, quad.br.1 + d),
            bl: (quad.bl.0 - d, quad.bl.1 + d),
        };
        report(&format!("外側へ {d}px"), scan_frame(&img, &q, layout));
    }

    println!("== マーカー直接検出 (locate_markers) ==");
    let t = std::time::Instant::now();
    match locate_markers(&img) {
        None => {
            println!("  見つからず ({:?})", t.elapsed());
            let names = ["TL", "TR", "BL", "BR"];
            for (i, v) in vloom_vcode::markers::debug_candidates(&img).iter().enumerate() {
                println!("    {} 候補 {:?}", names[i], v);
            }
        }
        Some(q) => {
            println!("  四隅 {:?} ({:?})", q, t.elapsed());
            report("scan_frame", scan_frame(&img, &q, layout));
        }
    }

    println!("== locate_code の推定から (アプリの通常受信の最初の経路) ==");
    match locate_code(&img, 0.94) {
        None => println!("  推定できず"),
        Some((cx, cy, bw, bh)) => {
            println!("  推定: 中心 ({cx:.0},{cy:.0}) 幅 {bw:.0} 高 {bh:.0}");
            let gh = bw * layout.height() as f32 / layout.width() as f32;
            let g = Quad {
                tl: (cx - bw / 2.0, cy - gh / 2.0),
                tr: (cx + bw / 2.0, cy - gh / 2.0),
                br: (cx + bw / 2.0, cy + gh / 2.0),
                bl: (cx - bw / 2.0, cy + gh / 2.0),
            };
            report("scan_frame", scan_frame(&img, &g, layout));
            report("scan_frame_wide", scan_frame_wide(&img, &g, layout));
        }
    }

    println!("== 中央ガイド frac 0.8 / 0.64 (通常受信のフォールバック) ==");
    for frac in [0.8f32, 0.64] {
        let gw = frac * w as f32;
        let gh = (gw * layout.height() as f32 / layout.width() as f32).min(h as f32 * 0.95);
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let g = Quad {
            tl: (cx - gw / 2.0, cy - gh / 2.0),
            tr: (cx + gw / 2.0, cy - gh / 2.0),
            br: (cx + gw / 2.0, cy + gh / 2.0),
            bl: (cx - gw / 2.0, cy + gh / 2.0),
        };
        report(&format!("frac {frac}"), scan_frame(&img, &g, layout));
    }
}

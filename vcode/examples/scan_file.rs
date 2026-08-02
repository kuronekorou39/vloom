//! 実機由来のグレースケール画像 (raw, 8bit) をスキャナにかけ、どの段階で
//! 失敗するかを調べる。実機で「見えているのに掴めない」ときの一次調査用。
//!
//!     cargo run --release -p vloom-vcode --example scan_file -- <path> <w> <h>

use std::env;
use std::fs;

use vloom_vcode::scan::{locate_code, scan_frame, scan_frame_wide, GrayImage, Quad};
use vloom_vcode::Layout;

fn guide(cx: f32, cy: f32, gw: f32, gh: f32, deg: f32) -> Quad {
    let (sin, cos) = deg.to_radians().sin_cos();
    let p = |dx: f32, dy: f32| (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos);
    let (hw, hh) = (gw / 2.0, gh / 2.0);
    Quad { tl: p(-hw, -hh), tr: p(hw, -hh), br: p(hw, hh), bl: p(-hw, hh) }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let (path, w, h) = (&args[1], args[2].parse::<usize>().unwrap(), args[3].parse::<usize>().unwrap());
    let data = fs::read(path).unwrap();
    assert_eq!(data.len(), w * h, "raw サイズ不一致");
    let img = GrayImage { w, h, data: &data };
    let layouts = [Layout::V1_DENSE, Layout::V0, Layout::V2_ULTRA];

    // 1) アプリの通常受信と同じ: 中央 frac 0.8
    println!("== 中央ガイド (frac 0.8, scan_frame ±48) ==");
    for layout in layouts {
        let gw = 0.8 * w as f32;
        let gh = (gw * layout.height() as f32 / layout.width() as f32).min(h as f32 * 0.95);
        let g = guide(w as f32 / 2.0, h as f32 / 2.0, gw, gh, 0.0);
        match scan_frame(&img, &g, layout) {
            Ok(r) => {
                let ok = r.frame.blocks.iter().filter(|b| b.is_some()).count();
                println!("  {}x{}: 検出! blocks {}/{} corners {:?}",
                    layout.grid_w, layout.grid_h, ok, layout.block_count(), r.corners);
            }
            Err(e) => println!("  {}x{}: {e:?}", layout.grid_w, layout.grid_h),
        }
    }

    // 2) acquire 相当: 位置 x スケール x 傾きの sweep (rot は 0 のみ = 画像は回転済み前提)
    println!("== acquire 相当 (scan_frame_wide ±96, 位置/スケール/傾き sweep) ==");
    let scales = [0.9f32, 0.7, 0.5, 0.38];
    let centers = [0.5f32, 0.32, 0.68];
    let tilts = [0.0f32, 12.0, -12.0, 24.0, -24.0];
    for layout in layouts {
        let aspect = layout.height() as f32 / layout.width() as f32;
        let mut found = false;
        'outer: for &s in &scales {
            let gw = s * w as f32;
            let gh = (gw * aspect).min(h as f32 * 0.95);
            for &cxf in &centers {
                for &cyf in &centers {
                    let cx = (cxf * w as f32).clamp(gw / 2.0, w as f32 - gw / 2.0);
                    let cy = (cyf * h as f32).clamp(gh / 2.0, h as f32 - gh / 2.0);
                    let tilt_set: &[f32] =
                        if cxf == 0.5 && cyf == 0.5 { &tilts } else { &tilts[..1] };
                    for &deg in tilt_set {
                        let g = guide(cx, cy, gw, gh, deg);
                        if let Ok(r) = scan_frame_wide(&img, &g, layout) {
                            let ok = r.frame.blocks.iter().filter(|b| b.is_some()).count();
                            println!(
                                "  {}x{}: 検出! s={s} c=({cxf},{cyf}) tilt={deg} blocks {}/{} corners {:?}",
                                layout.grid_w, layout.grid_h, ok, layout.block_count(), r.corners
                            );
                            found = true;
                            break 'outer;
                        }
                    }
                }
            }
        }
        if !found {
            println!("  {}x{}: 全 sweep 失敗", layout.grid_w, layout.grid_h);
        }
    }

    // 3) 実測した真の квад を直接与える (探索が悪いのか、照合が悪いのかの切り分け)
    println!("== 実測 quad を直接指定 (中心 548,888 / 幅 961) ==");
    for layout in layouts {
        let gw = 961.0f32;
        let gh = gw * layout.height() as f32 / layout.width() as f32;
        let g = guide(548.0, 435.0 + gh / 2.0, gw, gh, 0.0);
        match scan_frame_wide(&img, &g, layout) {
            Ok(r) => {
                let ok = r.frame.blocks.iter().filter(|b| b.is_some()).count();
                println!("  {}x{}: 検出! blocks {}/{}", layout.grid_w, layout.grid_h, ok, layout.block_count());
            }
            Err(e) => println!("  {}x{}: {e:?}", layout.grid_w, layout.grid_h),
        }
    }


    // 4) 総当たり: 位置・幅を細かく振って「どこなら検出できるか」を探す。
    //    どこかで通れば探索カバレッジの問題、全滅なら照合そのものの問題。
    println!("== 総当たり (5x4/7x6, 中心±, 幅 700..1000) ==");
    for layout in [Layout::V0, Layout::V1_DENSE] {
        let aspect = layout.height() as f32 / layout.width() as f32;
        let mut hits = 0;
        for wpx in (700..=1000).step_by(30) {
            for cx in (420..=660).step_by(30) {
                for cy in (700..=1050).step_by(30) {
                    let g = guide(cx as f32, cy as f32, wpx as f32, wpx as f32 * aspect, 0.0);
                    if let Ok(r) = scan_frame_wide(&img, &g, layout) {
                        let ok = r.frame.blocks.iter().filter(|b| b.is_some()).count();
                        if hits == 0 {
                            println!("  {}x{}: HIT w={wpx} c=({cx},{cy}) blocks {}/{} corners {:?}",
                                layout.grid_w, layout.grid_h, ok, layout.block_count(), r.corners);
                        }
                        hits += 1;
                    }
                }
            }
        }
        println!("  {}x{}: ヒット {hits} 通り", layout.grid_w, layout.grid_h);
    }


    // 5) locate_code → その位置で各レイアウトを 1 回ずつ試す (acquire の新方式)
    println!("== locate_code → 直接スキャン ==");
    match locate_code(&img, 0.94) {
        None => println!("  locate_code: 見つからず"),
        Some((cx, cy, bw, bh)) => {
            println!("  推定: 中心 ({cx:.0},{cy:.0}) {bw:.0}x{bh:.0}");
            for layout in [Layout::V0, Layout::V1_DENSE, Layout::V2_ULTRA] {
                let gh = bw * layout.height() as f32 / layout.width() as f32;
                let g = guide(cx, cy, bw, gh, 0.0);
                match scan_frame_wide(&img, &g, layout) {
                    Ok(r) => {
                        let ok = r.frame.blocks.iter().filter(|b| b.is_some()).count();
                        println!("  {}x{}: 検出! blocks {}/{}", layout.grid_w, layout.grid_h, ok, layout.block_count());
                    }
                    Err(e) => println!("  {}x{}: {e:?}", layout.grid_w, layout.grid_h),
                }
            }
        }
    }


    // 6) locate 位置の周辺をジッターして収束域を地図化する
    println!("== 収束域の地図 (5x4, locate 中心 540,856 周辺) ==");
    let layout = Layout::V0;
    let aspect = layout.height() as f32 / layout.width() as f32;
    for wpx in [920.0f32, 940.0, 952.0, 968.0] {
        let mut line = format!("  w={wpx:4.0}: ");
        for dy in [-60i32, -40, -20, 0, 20, 40, 60] {
            let g = guide(540.0, 856.0 + dy as f32, wpx, wpx * aspect, 0.0);
            let hit = scan_frame_wide(&img, &g, layout).is_ok();
            line.push_str(if hit { "o" } else { "." });
        }
        println!("{line}  (dy=-60..+60)");
    }
}

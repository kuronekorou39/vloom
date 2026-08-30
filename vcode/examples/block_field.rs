//! 実機ダンプに対して、ブロックごとの最適サブセルオフセット (誤差の場) を表示する。
//!
//!     cargo run --release -p vloom-vcode --example block_field -- <path> <w> <h> <grid_w> <grid_h>
//!
//! 4 隅はマーカー直接検出 → scan_frame の精密化で決め、その射影変換に対する各ブロックの
//! ずれ (セル単位、±1.5) を格子で出す。周辺ほど大きく外向き/内向きなら歪曲、一様なら 4 隅の誤差。
use std::env;
use std::fs;
use vloom_vcode::markers::locate_markers;
use vloom_vcode::scan::{block_offset_field, scan_frame, GrayImage};
use vloom_vcode::Layout;

fn main() {
    let a: Vec<String> = env::args().collect();
    if a.len() < 6 {
        eprintln!("引数: <path> <w> <h> <grid_w> <grid_h>");
        std::process::exit(2);
    }
    let (w, h) = (a[2].parse::<usize>().unwrap(), a[3].parse::<usize>().unwrap());
    let layout = Layout::from_grid(a[4].parse().unwrap(), a[5].parse().unwrap());
    let data = fs::read(&a[1]).unwrap();
    let img = GrayImage { w, h, data: &data };
    let q = locate_markers(&img).expect("マーカーが見つからない");
    let r = scan_frame(&img, &q, layout).expect("scan_frame 失敗");
    let ok = r.frame.blocks.iter().filter(|b| b.is_some()).count();
    println!("通常復号 {}/{}  四隅 {:?}", ok, layout.block_count(), r.corners);
    let field = block_offset_field(&img, &r.homography, layout);
    let found = field.iter().filter(|f| f.is_some()).count();
    println!("オフセット探索 (±1.5 セル) で通る {}/{}", found, layout.block_count());
    println!("各ブロックの (dx, dy) セル。'  --  ' は通らず:");
    for row in field.chunks(layout.grid_w) {
        let line: Vec<String> = row
            .iter()
            .map(|f| match f {
                Some((dx, dy)) => format!("{:+.2},{:+.2}", dx, dy),
                None => "  --  ".to_string(),
            })
            .collect();
        println!("  {}", line.join(" "));
    }
}

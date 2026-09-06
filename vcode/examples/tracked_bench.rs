//! 実機が保存したフレーム (raw 8bit グレースケール) で、追従スキャンの
//! 所要時間と回収ブロック数を測る。
//!
//! 実機の `scan=NNms` が想定より遅いときに、原因がコード側か構図側かを切り分ける。
//! 同じ 1 枚を使うので、スキャナを変更した前後の比較がそのまま成立する
//! (実機を持ち出して構図を再現する必要がない)。
//!
//! ダンプは受信アプリの Intent `--ei dump N` で保存される:
//!     adb pull /storage/emulated/0/Android/data/<pkg>/files/vcode_ok_1200x1600.gray
//!
//!     cargo run --release --features parallel -p vloom-vcode \
//!         --example tracked_bench -- <path> <w> <h> [grid]

use std::env;
use std::fs;
use std::time::Instant;

use vloom_vcode::markers::locate_markers;
use vloom_vcode::scan::{scan_frame, scan_frame_tracked, GrayImage};
use vloom_vcode::Layout;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: tracked_bench <path> <w> <h> [grid]");
        std::process::exit(2);
    }
    let (path, w, h) = (&args[1], args[2].parse::<usize>().unwrap(), args[3].parse::<usize>().unwrap());
    let layout = match args.get(4) {
        Some(g) => {
            let (a, b) = g.split_once('x').expect("格子は 13x18 の形で");
            Layout::from_grid(a.parse().unwrap(), b.parse().unwrap())
        }
        None => Layout::V6_XDENSE,
    };
    let data = fs::read(path).expect("読めない");
    assert!(data.len() >= w * h, "画像が {}x{} に足りない ({} byte)", w, h, data.len());
    let img = GrayImage { w, h, data: &data };

    // 追従の初期値を得る。実機と同じくマーカー直接検出から入る
    let quad = locate_markers(&img).expect("四隅マーカーを検出できない");
    let first = match scan_frame(&img, &quad, layout) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("フル探索が失敗: {e:?}");
            std::process::exit(1);
        }
    };
    let total = layout.block_count();
    let ok0 = first.frame.blocks.iter().filter(|b| b.is_some()).count();
    println!("フル探索: 回収 {ok0}/{total}  seq={}", first.frame.header.frame_seq);

    // 追従スキャン (実機の定常経路)。1 回目で温めてから測る
    let corners = first.corners;
    let warm = scan_frame_tracked(&img, &corners, layout).expect("追従が失敗");
    let ok = warm.frame.blocks.iter().filter(|b| b.is_some()).count();
    let runs = 30;
    let t0 = Instant::now();
    for _ in 0..runs {
        let _ = scan_frame_tracked(&img, &corners, layout);
    }
    let ms = t0.elapsed().as_secs_f64() * 1000.0 / runs as f64;
    println!("追従スキャン: {ms:.1} ms/枚  回収 {ok}/{total} ({:.0}%)",
             ok as f64 * 100.0 / total as f64);
    println!("(実機のログ [vcode-rx] rust(rot+dec) と突き合わせる。回転コピーは含まない)");

    // 1 セルが何画素で写っているか。3px/セル を切ると 1bit でも読めなくなる
    let c = warm.corners;
    let dist = |a: (f32, f32), b: (f32, f32)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
    let px_w = dist(c[0], c[1]) / layout.width() as f32;
    let px_h = dist(c[0], c[3]) / layout.height() as f32;
    println!(
        "
写り: 上辺 {:.0}px / 左辺 {:.0}px → {:.2} x {:.2} px/セル{}",
        dist(c[0], c[1]), dist(c[0], c[3]), px_w, px_h,
        if px_w.min(px_h) < 3.0 { "  ← 3px/セル を割っている" } else { "" }
    );

    // どのブロックが落ちたかの地図。散らばっていれば露出やピント、横帯なら送信
    // フレームの切り替わりの混ざり、縁や片側なら位置合わせのずれ (docs の切り分け)
    println!("
回収できたブロック (# = 回収, . = 落ち):");
    let okmap: Vec<bool> = warm.frame.blocks.iter().map(|b| b.is_some()).collect();
    for row in okmap.chunks(layout.grid_w) {
        let line: String = row.iter().map(|&b| if b { '#' } else { '.' }).collect();
        let n = row.iter().filter(|&&b| b).count();
        println!("  {line}  {n}/{}", layout.grid_w);
    }
}

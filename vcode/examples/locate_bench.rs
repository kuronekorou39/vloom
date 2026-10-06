//! 実機ダンプに対して、マーカー直接検出 (locate_markers) がどれだけ通るかを数える。
//!
//! 受信は前の枚の四隅から追従するので、直接検出の弱さは普段の統計に出ない
//! (出るのは「初回取得が遅い」「追従が切れたあと掴み直せない」という形)。
//! 1 枚ずつ独立に検出させて、見つかった四隅からフル探索の復号まで通るかを数える。
//!
//!     cargo run --release -p vloom-vcode --example locate_bench -- <grid> <dump.gray>...
//!
//! 画像は 1200x1600 を前提にしている (Pixel 9a の受信解像度)。

use std::env;
use std::fs;
use std::time::Instant;

use vloom_vcode::markers::locate_markers_layout;
use vloom_vcode::scan::{scan_frame, GrayImage};
use vloom_vcode::Layout;

const W: usize = 1200;
const H: usize = 1600;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: locate_bench <grid> <dump.gray>...");
        std::process::exit(2);
    }
    let (gw, gh) = args[1].split_once('x').expect("格子は 13x18 の形で");
    let layout = Layout::from_grid(gw.parse().unwrap(), gh.parse().unwrap());
    let (mut n, mut found, mut decoded, mut first, mut listed) = (0, 0, 0, 0, 0);
    let mut ms = 0.0;
    for path in &args[2..] {
        let raw = fs::read(path).expect("読めない");
        if raw.len() < W * H {
            continue;
        }
        let img = GrayImage { w: W, h: H, data: &raw };
        n += 1;
        let t0 = Instant::now();
        let quad = locate_markers_layout(&img);
        ms += t0.elapsed().as_secs_f64() * 1e3;
        if let Some((q, guesses)) = quad {
            found += 1;
            // 格子を知らされずに、写りだけから当てられたか (先頭 / 候補のどこか)
            first += (guesses.first() == Some(&layout)) as usize;
            listed += guesses.contains(&layout) as usize;
            // 見つかった四隅が正しいかは、そこからの復号が通るかで確かめる
            if scan_frame(&img, &q, layout).is_ok() {
                decoded += 1;
            }
        }
    }
    println!(
        "{n} 枚: 四隅 {found} / 復号 {decoded} / 格子の見積もり 先頭 {first}・候補内 {listed}  ({:.1} ms/枚)",
        ms / n.max(1) as f64
    );
}

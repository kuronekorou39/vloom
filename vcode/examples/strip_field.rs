//! 実機ダンプに対して、上下ストリップの較正パターンが 4 隅の射影変換からどれだけ
//! ずれているかを、横方向の区間ごとに表示する。
//!
//!     cargo run --release -p vloom-vcode --example strip_field -- <path> <w> <h> <grid_w> <grid_h>
//!
//! 4 隅はマーカー直接検出の結果をそのまま使う (照合や復号が通らない枚でも測れる)。
//! 平面なら全区間が (0, 0) 付近、湾曲した画面なら中央に向かって系統的にずれる。
//! block_field はブロックの CRC が通らないと測れないが、こちらは既知パターンとの一致で
//! 測るので、1 ブロックも読めない枚の形が分かる。
use std::env;
use std::fs;
use vloom_vcode::markers::locate_markers;
use vloom_vcode::scan::{strip_offset_field, GrayImage, Homography};
use vloom_vcode::Layout;

/// 探索する範囲 (セル)。湾曲では数セルずれるので block_field (±1.5) より広く取る
const HALF_CELLS: f32 = 4.0;

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
    let (wc, hc) = (layout.width() as f32, layout.height() as f32);
    let hm = Homography::from_quad(
        &[(0.0, 0.0), (wc, 0.0), (wc, hc), (0.0, hc)],
        &[q.tl, q.tr, q.br, q.bl],
    )
    .expect("四隅が退化している");
    let dist = |p: (f32, f32), r: (f32, f32)| ((p.0 - r.0).powi(2) + (p.1 - r.1).powi(2)).sqrt();
    println!(
        "四隅 tl{:?} tr{:?} br{:?} bl{:?}",
        q.tl, q.tr, q.br, q.bl
    );
    println!(
        "辺の長さ: 上 {:.0} / 下 {:.0} / 左 {:.0} / 右 {:.0} px  ({:.2} px/セル)",
        dist(q.tl, q.tr),
        dist(q.bl, q.br),
        dist(q.tl, q.bl),
        dist(q.tr, q.br),
        dist(q.tl, q.tr) / wc
    );
    // 区間はブロックの列と揃える (どの列のブロックがどれだけずれるかに読み替えられる)
    let field = strip_offset_field(&img, &hm, layout, layout.grid_w, HALF_CELLS);
    println!("区間 (左→右) ごとの最良オフセット (dx, dy) セルと一致率:");
    for (name, k) in [("上 (行 0)   ", 0usize), ("下 (較正帯) ", 1)] {
        let line: Vec<String> = field
            .iter()
            .map(|f| format!("{:+.2},{:+.2}({:.0}%)", f[k].0, f[k].1, f[k].2 * 100.0))
            .collect();
        println!("  {name} {}", line.join(" "));
    }
}

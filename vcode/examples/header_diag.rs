//! 「四隅は合うのにヘッダが読めない」の調査: 送信側の正解フレーム (セル解像度の
//! raw、0=黒 255=白) と、カメラ画像を四隅の射影でサンプルした値を比べ、どの行・
//! どの列区間でビットが化けているかを出す。歪みならオフセットを振ると直る場所が
//! 見え、ボケなら振っても直らない。
//!
//!     cargo run --release -p vloom-vcode --example header_diag -- \
//!         <camera.gray> <w> <h> <ref.gray> <grid_w> <grid_h> <tlx> <tly> <trx> <try> <brx> <bry> <blx> <bly>
use std::env;
use std::fs;
use vloom_vcode::scan::{GrayImage, Homography, Quad};
use vloom_vcode::{Layout, STRIP_H};

fn main() {
    let a: Vec<String> = env::args().collect();
    let (w, h) = (a[2].parse::<usize>().unwrap(), a[3].parse::<usize>().unwrap());
    let layout = Layout::from_grid(a[5].parse().unwrap(), a[6].parse().unwrap());
    let f = |i: usize| a[i].parse::<f32>().unwrap();
    let quad = Quad { tl: (f(7), f(8)), tr: (f(9), f(10)), br: (f(11), f(12)), bl: (f(13), f(14)) };
    let cam = fs::read(&a[1]).unwrap();
    let reference = fs::read(&a[4]).unwrap();
    let (cw, ch) = (layout.width(), layout.height());
    assert_eq!(reference.len(), cw * ch, "正解フレームの寸法が格子と合わない");
    let img = GrayImage { w, h, data: &cam };
    let hm = Homography::from_quad(
        &[(0.0, 0.0), (cw as f32, 0.0), (cw as f32, ch as f32), (0.0, ch as f32)],
        &[quad.tl, quad.tr, quad.br, quad.bl],
    )
    .expect("射影が作れない");

    // 黒/白のしきい値: 四隅マーカー (既知) の内側の黒と、その周りの白から
    let sample = |r: usize, c: usize, dx: f32, dy: f32| -> f32 {
        let (x, y) = hm.map(c as f32 + 0.5 + dx, r as f32 + 0.5 + dy);
        img.bilinear(x, y)
    };
    let mut blacks = Vec::new();
    let mut whites = Vec::new();
    for r in 0..cw.min(ch) {
        for c in 0..cw {
            let v = reference[r * cw + c];
            if r < 24 && c < 24 && v < 128 { blacks.push(sample(r, c, 0.0, 0.0)); }
            if r < 24 && (24..28).contains(&c) && v >= 128 { whites.push(sample(r, c, 0.0, 0.0)); }
        }
    }
    let mean = |v: &Vec<f32>| v.iter().sum::<f32>() / v.len().max(1) as f32;
    let (b, wv) = (mean(&blacks), mean(&whites));
    let thr = (b + wv) / 2.0;
    println!("黒 {b:.0} / 白 {wv:.0} / しきい値 {thr:.0}");

    // 行ごと (上ストリップ 0..STRIP_H と、データ先頭 3 行、最下部 3 行) の一致率。
    // 縦オフセットを振って、どこで最良になるかも出す。
    let dys = [-1.0f32, -0.75, -0.5, -0.25, 0.0, 0.25, 0.5, 0.75, 1.0];
    let rows: Vec<usize> = (0..STRIP_H).chain(STRIP_H..STRIP_H + 3).chain(ch - 3..ch).collect();
    println!("行  一致率(dy=0)  最良 dy → 一致率   [列を 6 区間に分けた最良 dy]");
    for &r in &rows {
        let agree = |dy: f32, c0: usize, c1: usize| -> f32 {
            let mut ok = 0;
            let mut n = 0;
            for c in c0..c1 {
                let want = reference[r * cw + c] < 128;
                let got = sample(r, c, 0.0, dy) < thr;
                n += 1;
                if want == got { ok += 1; }
            }
            ok as f32 / n.max(1) as f32
        };
        let (best_dy, best) = dys
            .iter()
            .map(|&dy| (dy, agree(dy, 0, cw)))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .unwrap();
        let segs: Vec<String> = (0..6)
            .map(|s| {
                let (c0, c1) = (s * cw / 6, (s + 1) * cw / 6);
                let (d, v) = dys
                    .iter()
                    .map(|&dy| (dy, agree(dy, c0, c1)))
                    .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
                    .unwrap();
                format!("{d:+.2}:{:.0}%", v * 100.0)
            })
            .collect();
        println!("{r:3}   {:5.1}%      {best_dy:+.2} → {:5.1}%   [{}]", agree(0.0, 0, cw) * 100.0, best * 100.0, segs.join(" "));
    }
}

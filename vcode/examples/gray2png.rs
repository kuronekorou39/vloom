//! 実機からダンプした生グレースケール (.gray) を PNG に変換する。
//! 実行: cargo run -p vloom-vcode --example gray2png -- <input.gray> <width> <height>

use std::fs;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (input, w, h) = (&args[1], args[2].parse::<u32>().unwrap(), args[3].parse::<u32>().unwrap());
    let data = fs::read(input).expect("入力を読めない");
    assert_eq!(data.len(), (w * h) as usize, "サイズ不一致");
    let out = format!("{input}.png");
    let file = fs::File::create(&out).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&data).unwrap();
    println!("wrote {out}");
}

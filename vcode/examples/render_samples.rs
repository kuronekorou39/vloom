//! vcode フレームのテスト画像を vcode/samples/ に PNG で出力する (目視確認用)。
//!
//! 実行: cargo run -p vloom-vcode --example render_samples
//!
//! 出力 (すべて gitignore 済み):
//!   - frame_clean.png     : 全 20 ブロックにデータが載った通常フレーム
//!   - frame_partial.png   : 実データ 5 ブロック + フィラー 15 ブロック
//!   - frame_corrupted.png : 中央 4 ブロックを黒塗り破損させたフレーム
//!                           (このフレームでも残り 16 ブロック + ヘッダは回収できる)

use vloom_vcode::{decode_frame, encode_frame, Bitmap, FrameHeader, Layout, VERSION};
use std::fs;
use std::path::Path;

/// 表示・目視用の拡大率 (100x92 セル → 800x736 px)
const SCALE: usize = 8;

fn save_png(bm: &Bitmap, path: &Path) {
    let file = fs::File::create(path).expect("PNG ファイルを作成できない");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), bm.w as u32, bm.h as u32);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("PNG ヘッダ書き込み失敗");
    writer.write_image_data(&bm.data).expect("PNG データ書き込み失敗");
    println!("wrote {} ({}x{})", path.display(), bm.w, bm.h);
}

/// 決定的な擬似ランダムペイロード (実データらしい見た目にする)
fn random_blocks(n: usize, len: usize) -> Vec<Vec<u8>> {
    let mut state: u64 = 0x1234_5678_9ABC_DEF0;
    (0..n)
        .map(|_| {
            (0..len)
                .map(|_| {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    (state >> 33) as u8
                })
                .collect()
        })
        .collect()
}

fn main() {
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
    fs::create_dir_all(&out_dir).expect("samples/ を作成できない");

    let layout = Layout::V0;
    let header = FrameHeader {
        version: VERSION,
        bits_per_cell: 1,
        layout,
        frame_seq: 0,
        oti: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
    };
    let blocks = random_blocks(layout.block_count(), layout.block_payload_len(1));

    // 1. 通常フレーム
    let clean = encode_frame(&header, &blocks, SCALE);
    save_png(&clean, &out_dir.join("frame_clean.png"));

    // 2. 実データ 5 ブロックのみ (残りはフィラー = 白ゼロ領域)
    let partial = encode_frame(&header, &blocks[..5].to_vec(), SCALE);
    save_png(&partial, &out_dir.join("frame_partial.png"));

    // 3. 中央 4 ブロック (bx,by)=(1..3,1..3) を黒塗り破損
    let mut corrupted = Bitmap {
        w: clean.w,
        h: clean.h,
        data: clean.data.clone(),
    };
    for y in 26 * SCALE..66 * SCALE {
        for x in 20 * SCALE..60 * SCALE {
            corrupted.set(x, y, 0);
        }
    }
    save_png(&corrupted, &out_dir.join("frame_corrupted.png"));

    // 破損フレームでも部分回収できることをその場で確認して表示
    let decoded = decode_frame(&corrupted, SCALE).expect("ヘッダ/コーナーは無傷のはず");
    let ok = decoded.blocks.iter().filter(|b| b.is_some()).count();
    println!(
        "frame_corrupted.png: {}/{} ブロック回収 (QR なら 0/1 = 全損)",
        ok,
        layout.block_count()
    );
}

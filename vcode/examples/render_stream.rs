//! 実機 E2E テスト用: 本物の RaptorQ パケットを載せた vcode フレーム列を PNG で生成し、
//! ブラウザでアニメーション表示する HTML も出力する。
//!
//! 実行: cargo run -p vloom-vcode --example render_stream [payload_bytes|ファイルパス] [grid_w] [grid_h] [bpc]
//! 第 1 引数が数値ならその長さの擬似ランダムデータ、ファイルパスならその実ファイルを送る。
//! 出力: vcode/samples/stream/tx_NNN.png + vcode/samples/stream/index.html (gitignore 済み)
//!
//! PC でフルスクリーン表示し、スマホアプリの「V受信」をかざして受信を確認する。

use vloom_fountain::Encoder;
use vloom_vcode::{encode_frame, FrameHeader, Layout, VERSION};
use std::fs;
use std::path::Path;

const SCALE: usize = 8; // 100x92 セル → 800x736 px

fn save_png(bm: &vloom_vcode::Bitmap, path: &Path) {
    let file = fs::File::create(path).expect("PNG ファイルを作成できない");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), bm.w as u32, bm.h as u32);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("PNG ヘッダ書き込み失敗");
    writer.write_image_data(&bm.data).expect("PNG データ書き込み失敗");
}

fn main() {
    // 第 1 引数: 数値 → 擬似ランダムデータのバイト数 / それ以外 → 実ファイルパス
    let arg1 = std::env::args().nth(1);
    let input_file: Option<String> = match &arg1 {
        Some(s) if s.parse::<usize>().is_err() => Some(s.clone()),
        _ => None,
    };
    let payload_len: usize = arg1.and_then(|s| s.parse().ok()).unwrap_or(20_000);
    let grid_w: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(5);
    let grid_h: usize = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(4);
    let bpc: u8 = std::env::args().nth(4).and_then(|s| s.parse().ok()).unwrap_or(1);
    assert!(bpc == 1 || bpc == 2, "bpc は 1 か 2");

    // 実ファイル指定ならそれを、なければ決定的な擬似ランダムペイロード
    let payload: Vec<u8> = match &input_file {
        Some(path) => fs::read(path).unwrap_or_else(|e| panic!("{path} を読めない: {e}")),
        None => {
            let mut state: u64 = 0x0BEA_D5_C0DE;
            (0..payload_len)
                .map(|_| {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    (state >> 33) as u8
                })
                .collect()
        }
    };
    let payload_len = payload.len();
    if let Some(path) = &input_file {
        println!("入力ファイル: {path} ({payload_len} バイト)");
    }

    let layout = Layout { block: 20, grid_w, grid_h };
    let source_packets = payload_len.div_ceil(layout.packet_size(bpc));
    let encoder = Encoder::new(&payload, layout.packet_size(bpc) as u16, (source_packets / 2) as u32);
    let bc = layout.block_count();
    let pc = encoder.packet_count();
    let n_frames = pc.div_ceil(bc);

    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples").join("stream");
    fs::create_dir_all(&out_dir).expect("samples/stream を作成できない");

    let mut oti = [0u8; 12];
    oti.copy_from_slice(&encoder.oti_bytes());

    for f in 0..n_frames {
        let header = FrameHeader {
            version: VERSION,
            bits_per_cell: bpc,
            layout,
            frame_seq: f as u16,
            oti,
        };
        // アプリの VcodeTx と同じ循環割り当て (全フレーム満杯)。
        // raptorq のシンボル丸めでパケットが短い場合はゼロパディング。
        let payload_len = layout.block_payload_len(bpc);
        let blocks: Vec<Vec<u8>> = (0..bc)
            .map(|j| {
                let mut p = encoder.packet((f * bc + j) % pc);
                p.resize(payload_len, 0);
                p
            })
            .collect();
        let bm = encode_frame(&header, &blocks, SCALE);
        save_png(&bm, &out_dir.join(format!("tx_{f:03}.png")));
    }

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="ja">
<head>
<meta charset="UTF-8">
<title>vcode stream ({payload_len} B, {n_frames} frames)</title>
<style>
  body {{ margin: 0; background: #fff; display: flex; flex-direction: column;
         align-items: center; justify-content: center; height: 100vh; }}
  img {{ image-rendering: pixelated; height: min(92vh, 736px * 1.2); }}
  #info {{ font: 12px sans-serif; color: #888; margin-top: 8px; }}
</style>
</head>
<body>
<img id="f" src="tx_000.png">
<div id="info"></div>
<script>
  const N = {n_frames};
  let fps = Number(new URLSearchParams(location.search).get('fps') || 20);
  let i = 0, pass = 0;
  setInterval(() => {{
    i = (i + 1) % N;
    if (i === 0) pass++;
    document.getElementById('f').src = `tx_${{String(i).padStart(3, '0')}}.png`;
    document.getElementById('info').textContent =
      `frame ${{i}}/${{N}}  pass ${{pass}}  ${{fps}}fps  ({payload_len} B)  ?fps=N で変更`;
  }}, 1000 / fps);
</script>
</body>
</html>
"#
    );
    fs::write(out_dir.join("index.html"), html).expect("index.html 書き込み失敗");

    println!(
        "生成完了: {} バイト → {} source + repair = {} packets → {} フレーム",
        payload_len, source_packets, pc, n_frames
    );
    println!("表示: {}", out_dir.join("index.html").display());
}

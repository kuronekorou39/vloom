//! scan モジュールの合成チャネルテスト。
//! レンダリングしたフレームに透視変形・輝度勾配・ノイズを加えた「擬似カメラ画像」を作り、
//! ずれたガイド枠からのスキャンでヘッダ+ブロックが回収できることを検証する。

use vloom_vcode::scan::{scan_frame, scan_frame_wide, GrayImage, Homography, Quad};
use vloom_vcode::{encode_frame, FrameHeader, Layout, VERSION};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
}

fn test_frame_bpc(layout: Layout, seed: u8, bpc: u8) -> (FrameHeader, Vec<Vec<u8>>) {
    let header = FrameHeader {
        version: VERSION,
        bits_per_cell: bpc,
        layout,
        frame_seq: 42,
        oti: [9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 1, 2],
    };
    let blocks = (0..layout.block_count())
        .map(|bi| {
            (0..layout.block_payload_len(bpc))
                .map(|i| (i as u8).wrapping_mul(17).wrapping_add(bi as u8 ^ seed))
                .collect()
        })
        .collect();
    (header, blocks)
}

fn test_frame(layout: Layout, seed: u8) -> (FrameHeader, Vec<Vec<u8>>) {
    test_frame_bpc(layout, seed, 1)
}

/// レンダリング済みフレームを、指定した 4 隅へ射影変換して canvas に描き込む。
/// 輝度勾配 (左 75% → 右 95%) とノイズ (±8) も加える。
fn synth_camera_image(
    frame_px: &vloom_vcode::Bitmap,
    canvas_w: usize,
    canvas_h: usize,
    dst: &[(f32, f32); 4],
    noise_seed: u64,
) -> Vec<u8> {
    let src_quad = [
        (0.0, 0.0),
        (frame_px.w as f32, 0.0),
        (frame_px.w as f32, frame_px.h as f32),
        (0.0, frame_px.h as f32),
    ];
    // canvas → フレーム画素座標 の逆写像で各画素を埋める
    let h_fwd = Homography::from_quad(&src_quad, dst).unwrap();
    let h_inv = h_fwd.inverse().unwrap();
    let src = GrayImage { w: frame_px.w, h: frame_px.h, data: &frame_px.data };

    let mut rng = Lcg(noise_seed);
    let mut out = vec![250u8; canvas_w * canvas_h];
    for y in 0..canvas_h {
        for x in 0..canvas_w {
            let (sx, sy) = h_inv.map(x as f32, y as f32);
            let v = if sx < -1.0 || sy < -1.0 || sx > frame_px.w as f32 || sy > frame_px.h as f32 {
                250.0 // コード外は明るい背景
            } else {
                src.bilinear(sx, sy)
            };
            let gain = 0.75 + 0.20 * (x as f32 / canvas_w as f32);
            let noise = (rng.next() % 17) as f32 - 8.0;
            out[y * canvas_w + x] = (v * gain + noise).clamp(0.0, 255.0) as u8;
        }
    }
    out
}

#[test]
fn scan_recovers_from_perspective_and_noise() {
    let layout = Layout::V0;
    let (header, blocks) = test_frame(layout, 0xC3);
    let frame_px = encode_frame(&header, &blocks, 8); // 800x736

    // 少し傾いた台形に射影 (手持ちカメラの構図を模擬)
    let dst = [
        (180.0, 130.0),  // tl
        (1010.0, 155.0), // tr
        (985.0, 950.0),  // br
        (205.0, 920.0),  // bl
    ];
    let mut canvas = synth_camera_image(&frame_px, 1280, 1080, &dst, 0xFACE);

    // 実機で観測したクラッタを模擬: コード上方の暗い帯 (ブラウザのタブバー相当) と
    // コード下方のテキスト状の黒い点列 (ページの説明文相当)
    for y in 60..95 {
        for x in 100..1100 {
            canvas[y * 1280 + x] = 40;
        }
    }
    for x in (250..900).step_by(7) {
        for y in 985..997 {
            if (x / 7) % 3 != 0 {
                canvas[y * 1280 + x] = 20;
                canvas[y * 1280 + x + 3] = 25;
            }
        }
    }

    let img = GrayImage { w: 1280, h: 1080, data: &canvas };

    // ガイドは真の 4 隅から最大 25px ずらす (実機での構図ずれ相当)
    let guide = Quad {
        tl: (dst[0].0 - 22.0, dst[0].1 + 18.0),
        tr: (dst[1].0 + 25.0, dst[1].1 - 15.0),
        br: (dst[2].0 + 19.0, dst[2].1 + 24.0),
        bl: (dst[3].0 - 17.0, dst[3].1 - 25.0),
    };

    let result = scan_frame(&img, &guide, layout).expect("スキャン失敗");
    assert_eq!(result.frame.header, header);

    let ok = result.frame.blocks.iter().filter(|b| b.is_some()).count();
    assert!(ok >= 19, "回収ブロックが少なすぎる: {ok}/20");
    for (i, b) in result.frame.blocks.iter().enumerate() {
        if let Some(payload) = b {
            assert_eq!(payload, &blocks[i], "block {i} の内容不一致");
        }
    }
}

#[test]
fn scan_recovers_from_large_guide_offset() {
    // 可視ガイド枠に手持ちで合わせたときの構図ずれを模擬。
    // 旧実装の粗探索 (±32px) では届かない ~44px のずれでも、拡大した捕捉範囲 (±48px) で掴める。
    let layout = Layout::V0;
    let (header, blocks) = test_frame(layout, 0x6D);
    let frame_px = encode_frame(&header, &blocks, 8);
    let dst = [
        (180.0f32, 130.0),
        (1010.0, 155.0),
        (985.0, 950.0),
        (205.0, 920.0),
    ];
    let canvas = synth_camera_image(&frame_px, 1280, 1080, &dst, 0xA11C);
    let img = GrayImage { w: 1280, h: 1080, data: &canvas };
    // 各隅を真値から ~44px (±32 超) ずらす
    let guide = Quad {
        tl: (dst[0].0 - 44.0, dst[0].1 + 40.0),
        tr: (dst[1].0 + 43.0, dst[1].1 - 38.0),
        br: (dst[2].0 + 41.0, dst[2].1 + 44.0),
        bl: (dst[3].0 - 40.0, dst[3].1 - 42.0),
    };
    let result = scan_frame(&img, &guide, layout).expect("大きめガイドずれのスキャン失敗");
    assert_eq!(result.frame.header, header);
    let ok = result.frame.blocks.iter().filter(|b| b.is_some()).count();
    assert!(ok >= 19, "回収ブロックが少なすぎる: {ok}/20");
}

#[test]
fn scan_wide_recovers_from_far_guide_offset() {
    // acquire (位置合わせ) 相当。中央から大きく外れた/傾いたコードを、多位置 sweep の
    // 1 位置として渡した guide から広域探索 (±96) で取得できることを検証する。
    // 同じ ~78px ずれは通常受信 (±48) では取得できない (= acquire の存在意義)。
    let layout = Layout::V0;
    let (header, blocks) = test_frame(layout, 0x2E);
    let frame_px = encode_frame(&header, &blocks, 8);
    let dst = [
        (180.0f32, 130.0),
        (1010.0, 155.0),
        (985.0, 950.0),
        (205.0, 920.0),
    ];
    let canvas = synth_camera_image(&frame_px, 1280, 1080, &dst, 0xBEAD);
    let img = GrayImage { w: 1280, h: 1080, data: &canvas };
    // 各隅を真値から ~78px (±48 超, ±96 内) ずらす
    let guide = Quad {
        tl: (dst[0].0 - 78.0, dst[0].1 + 78.0),
        tr: (dst[1].0 + 78.0, dst[1].1 - 78.0),
        br: (dst[2].0 + 78.0, dst[2].1 + 78.0),
        bl: (dst[3].0 - 78.0, dst[3].1 - 78.0),
    };
    assert!(scan_frame(&img, &guide, layout).is_err(), "通常受信 (±48) では取得できないはず");
    let result = scan_frame_wide(&img, &guide, layout).expect("広域取得 (±96) で取得できるはず");
    assert_eq!(result.frame.header, header);
    let ok = result.frame.blocks.iter().filter(|b| b.is_some()).count();
    assert!(ok >= 18, "取得後の回収ブロックが少なすぎる: {ok}/20");
}

#[test]
fn scan_dense_layout_at_1080p_cell_resolution() {
    // 7x6 高密度レイアウト (140x132 セル)。1080p 実機相当の ~5.9px/セルで検証する。
    let layout = Layout::V1_DENSE;
    let (header, blocks) = test_frame(layout, 0x99);
    let frame_px = encode_frame(&header, &blocks, 6); // 840x792 px

    let dst = [
        (180.0f32, 130.0),
        (1010.0, 155.0),
        (985.0, 950.0),
        (205.0, 920.0),
    ];
    let canvas = synth_camera_image(&frame_px, 1280, 1080, &dst, 0x7777);
    let img = GrayImage { w: 1280, h: 1080, data: &canvas };
    let guide = Quad {
        tl: (dst[0].0 - 14.0, dst[0].1 + 12.0),
        tr: (dst[1].0 + 15.0, dst[1].1 - 10.0),
        br: (dst[2].0 + 12.0, dst[2].1 + 15.0),
        bl: (dst[3].0 - 11.0, dst[3].1 - 14.0),
    };

    let result = scan_frame(&img, &guide, layout).expect("高密度レイアウトのスキャン失敗");
    assert_eq!(result.frame.header, header);
    let ok = result.frame.blocks.iter().filter(|b| b.is_some()).count();
    assert!(ok >= 39, "高密度の回収ブロックが少なすぎる: {ok}/42");
}

#[test]
fn scan_2bpc_with_gain_gradient() {
    // 輝度 4 値 (2bit/セル)。横方向の輝度勾配 (gain 0.75→0.95) 下でも
    // ストリップ由来の局所較正で正しく量子化できることを検証する。
    let layout = Layout::V0;
    let (header, blocks) = test_frame_bpc(layout, 0xB2, 2);
    let frame_px = encode_frame(&header, &blocks, 8);

    let dst = [
        (180.0f32, 130.0),
        (1010.0, 155.0),
        (985.0, 950.0),
        (205.0, 920.0),
    ];
    let canvas = synth_camera_image(&frame_px, 1280, 1080, &dst, 0x4B4B);
    let img = GrayImage { w: 1280, h: 1080, data: &canvas };
    let guide = Quad {
        tl: (dst[0].0 - 15.0, dst[0].1 + 12.0),
        tr: (dst[1].0 + 14.0, dst[1].1 - 10.0),
        br: (dst[2].0 + 11.0, dst[2].1 + 13.0),
        bl: (dst[3].0 - 10.0, dst[3].1 - 14.0),
    };

    let result = scan_frame(&img, &guide, layout).expect("2bpc スキャン失敗");
    assert_eq!(result.frame.header, header);
    let ok = result.frame.blocks.iter().filter(|b| b.is_some()).count();
    assert!(ok >= 17, "2bpc の回収ブロックが少なすぎる: {ok}/20");
    for (i, b) in result.frame.blocks.iter().enumerate() {
        if let Some(payload) = b {
            assert_eq!(payload, &blocks[i], "block {i} の内容不一致");
        }
    }
}

#[test]
fn tracked_scan_follows_small_motion() {
    use vloom_vcode::scan::scan_frame_tracked;
    let layout = Layout::V0;
    let (header, blocks) = test_frame(layout, 0x5E);
    let frame_px = encode_frame(&header, &blocks, 8);

    let dst1 = [
        (180.0f32, 130.0),
        (1010.0, 155.0),
        (985.0, 950.0),
        (205.0, 920.0),
    ];
    let canvas1 = synth_camera_image(&frame_px, 1280, 1080, &dst1, 0x1111);
    let img1 = GrayImage { w: 1280, h: 1080, data: &canvas1 };
    let guide = Quad {
        tl: (dst1[0].0 - 15.0, dst1[0].1 + 10.0),
        tr: (dst1[1].0 + 12.0, dst1[1].1 - 8.0),
        br: (dst1[2].0 + 10.0, dst1[2].1 + 14.0),
        bl: (dst1[3].0 - 9.0, dst1[3].1 - 12.0),
    };
    let first = scan_frame(&img1, &guide, layout).expect("初回フル探索が失敗");

    // 手持ちのフレーム間変位を模擬: 全体が数 px 平行移動 + 微小な歪み
    let dst2 = [
        (dst1[0].0 + 4.0, dst1[0].1 - 3.0),
        (dst1[1].0 + 5.0, dst1[1].1 - 2.0),
        (dst1[2].0 + 3.0, dst1[2].1 - 4.0),
        (dst1[3].0 + 5.0, dst1[3].1 - 3.0),
    ];
    let canvas2 = synth_camera_image(&frame_px, 1280, 1080, &dst2, 0x2222);
    let img2 = GrayImage { w: 1280, h: 1080, data: &canvas2 };

    let tracked = scan_frame_tracked(&img2, &first.corners, layout).expect("追従スキャンが失敗");
    assert_eq!(tracked.frame.header, header);
    let ok = tracked.frame.blocks.iter().filter(|b| b.is_some()).count();
    assert!(ok >= 19, "追従スキャンの回収が少なすぎる: {ok}/20");
}

#[test]
fn scan_fails_gracefully_on_blank_image() {
    let blank = vec![250u8; 640 * 480];
    let img = GrayImage { w: 640, h: 480, data: &blank };
    let guide = Quad {
        tl: (100.0, 100.0),
        tr: (500.0, 100.0),
        br: (500.0, 460.0),
        bl: (100.0, 460.0),
    };
    assert!(scan_frame(&img, &guide, Layout::V0).is_err());
}

/// 密なレイアウトを「1 セル cell_px 画素」で写した擬似カメラ画像からスキャンし、
/// 回収できたブロック数を返す。密度の上限がセル解像度で決まることを測るための共通処理。
fn recover_at_cell_px(layout: Layout, bpc: u8, cell_px: usize, seed: u64) -> (usize, usize) {
    let (header, blocks) = test_frame_bpc(layout, 0x5A, bpc);
    let frame_px = encode_frame(&header, &blocks, cell_px);
    // コードの周りに余白を取った canvas に、わずかな透視ずれを付けて配置する
    let (cw, ch) = (frame_px.w + 320, frame_px.h + 260);
    let (fw, fh) = (frame_px.w as f32, frame_px.h as f32);
    let dst = [
        (150.0f32, 110.0),
        (150.0 + fw, 135.0),
        (130.0 + fw, 110.0 + fh),
        (170.0, 90.0 + fh),
    ];
    let canvas = synth_camera_image(&frame_px, cw, ch, &dst, seed);
    let img = GrayImage { w: cw, h: ch, data: &canvas };
    // ガイド枠は実機同様に数十 px ずらす (粗→細探索が吸収できる範囲)
    let guide = Quad {
        tl: (dst[0].0 - 16.0, dst[0].1 + 13.0),
        tr: (dst[1].0 + 15.0, dst[1].1 - 11.0),
        br: (dst[2].0 + 12.0, dst[2].1 + 14.0),
        bl: (dst[3].0 - 11.0, dst[3].1 - 15.0),
    };
    match scan_frame(&img, &guide, layout) {
        Err(_) => (0, layout.block_count()),
        Ok(result) => {
            assert_eq!(result.frame.header, header, "ヘッダ不一致");
            for (i, b) in result.frame.blocks.iter().enumerate() {
                if let Some(payload) = b {
                    assert_eq!(payload, &blocks[i], "block {i} の内容不一致");
                }
            }
            (
                result.frame.blocks.iter().filter(|b| b.is_some()).count(),
                layout.block_count(),
            )
        }
    }
}

#[test]
fn scan_ultra_layout_2bpc_at_6px_per_cell() {
    // 9x8 (180x172 セル) を輝度 4 値で、6px/セル = コード幅 1080px 相当で写す。
    // 2160p カメラで画面いっぱいに収めたときの想定条件。
    let (ok, total) = recover_at_cell_px(Layout::V2_ULTRA, 2, 6, 0x9E9E);
    assert!(ok * 10 >= total * 9, "9x8/2bpc の回収が少なすぎる: {ok}/{total}");
}

#[test]
fn scan_max_layout_2bpc_at_6px_per_cell() {
    // 11x10 (220x212 セル) を輝度 4 値で 6px/セル = コード幅 1320px 相当。
    // 4K カメラ + 近距離でようやく届く条件。
    let (ok, total) = recover_at_cell_px(Layout::V3_MAX, 2, 6, 0x3C3C);
    assert!(ok * 10 >= total * 9, "11x10/2bpc の回収が少なすぎる: {ok}/{total}");
}

#[test]
fn dense_layouts_degrade_below_4px_per_cell() {
    // 密度の限界を明示しておく: 4px/セル まで落とすと輝度 4 値は成立しなくなる。
    // 「受信解像度を上げないと格子は上げられない」という設計上の制約の裏付け。
    let (ok, total) = recover_at_cell_px(Layout::V2_ULTRA, 2, 4, 0x7A7A);
    assert!(ok < total, "4px/セル で全ブロック回収できてしまった: {ok}/{total}");
}

#[test]
fn scan_wide_recovers_tilted_code_from_tilted_guide() {
    // 面内に ~18° 傾いたコードを、12° 傾けた初期枠 (acquire の傾き刻みに相当) から
    // 広域探索で掴めることを検証する。刻みが 12° なら真値との差は最大 6° 程度で、
    // コーナー変位は ±96px の捕捉範囲に収まるはず。
    let layout = Layout::V0;
    let (header, blocks) = test_frame(layout, 0x4C);
    let frame_px = encode_frame(&header, &blocks, 8);
    let (fw, fh) = (frame_px.w as f32, frame_px.h as f32);
    // 18° 回転した配置 (中心 640,540)
    let (cx, cy) = (640.0f32, 540.0);
    let ang = 18.0f32.to_radians();
    let (sin, cos) = ang.sin_cos();
    let rot_pt = |dx: f32, dy: f32| (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos);
    let (hw, hh) = (fw / 2.0, fh / 2.0);
    let dst = [rot_pt(-hw, -hh), rot_pt(hw, -hh), rot_pt(hw, hh), rot_pt(-hw, hh)];
    let canvas = synth_camera_image(&frame_px, 1280, 1080, &dst, 0x71E7);
    let img = GrayImage { w: 1280, h: 1080, data: &canvas };
    // 12° の初期枠 (寸法は 5% 大きめ = スケール不一致も混ぜる)
    let gang = 12.0f32.to_radians();
    let (gs, gc) = gang.sin_cos();
    let g_pt = |dx: f32, dy: f32| (cx + dx * gc - dy * gs, cy + dx * gs + dy * gc);
    let (gw2, gh2) = (hw * 1.05, hh * 1.05);
    let guide = Quad {
        tl: g_pt(-gw2, -gh2),
        tr: g_pt(gw2, -gh2),
        br: g_pt(gw2, gh2),
        bl: g_pt(-gw2, gh2),
    };
    let result = scan_frame_wide(&img, &guide, layout).expect("傾き 18° を 12° 初期枠から取得できるはず");
    assert_eq!(result.frame.header, header);
    let ok = result.frame.blocks.iter().filter(|b| b.is_some()).count();
    assert!(ok >= 18, "傾き構図の回収ブロックが少なすぎる: {ok}/20");
}

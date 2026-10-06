//! 「さがす」モードの通し: 試験フレームを余白付きのキャンバスに置き、格子を知らせずに
//! マーカーから掴んで、ブロックの素性まで読めることを確かめる。
//! 受信側は送信側の格子の切り替えに付いていく必要があるので、格子ごとに確かめる。

use vloom_vcode::markers::locate_markers_layout;
use vloom_vcode::probe::{probe_frame, ProbeBlock, ProbeTag};
use vloom_vcode::scan::{scan_frame, GrayImage};
use vloom_vcode::Layout;

/// 1 セルを何画素で描くか (実機の 3 px/セル 前後に合わせる)
const SCALE: usize = 3;
/// コードの周りの余白 (px)
const PAD: usize = 60;

#[test]
fn probe_frames_are_found_without_knowing_the_grid() {
    for (setting, (gw, gh)) in [(11, 14), (13, 18), (15, 20), (16, 20), (7, 6)].into_iter().enumerate() {
        let layout = Layout::from_grid(gw, gh);
        let tag = ProbeTag { setting: setting as u8, settings: 5, fps: 20, dwell_ds: 40 };
        let bm = probe_frame(layout, tag, 1234);
        let (cw, ch) = (bm.w * SCALE + 2 * PAD, bm.h * SCALE + 2 * PAD);
        let mut canvas = vec![235u8; cw * ch];
        for y in 0..bm.h * SCALE {
            for x in 0..bm.w * SCALE {
                let v = if bm.get(x / SCALE, y / SCALE) < 128 { 22 } else { 235 };
                canvas[(PAD + y) * cw + PAD + x] = v;
            }
        }
        let img = GrayImage { w: cw, h: ch, data: &canvas };
        let (quad, guesses) = locate_markers_layout(&img).unwrap_or_else(|| panic!("{gw}x{gh}: マーカーが出ない"));
        assert_eq!(guesses.first(), Some(&layout), "{gw}x{gh}: 格子の見積もり {guesses:?}");
        let r = scan_frame(&img, &quad, layout).unwrap_or_else(|e| panic!("{gw}x{gh}: {e:?}"));
        assert_eq!(ProbeTag::from_oti(&r.frame.header.oti), Some(tag));
        for (bi, b) in r.frame.blocks.iter().enumerate() {
            let id = ProbeBlock::parse(b.as_ref().unwrap_or_else(|| panic!("{gw}x{gh}: ブロック {bi} が落ちた"))).unwrap();
            assert_eq!((id.frame, id.block as usize, id.setting), (1234, bi, setting as u8));
        }
    }
}

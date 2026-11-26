//! Android の受信で、カメラ (Camera2) の Y 面を Flutter を通さずに走査するための JNI 窓口。
//!
//! Flutter の camera プラグイン経由だと、カメラ画像 (数 MB) を Dart に渡すだけで UI isolate を
//! 1 枚 20ms 使い、解像度も 1600x1200 までしか出ない。Kotlin (NativeRxCamera) がスレッドごとに
//! このハンドルを 1 つ持ち、ImageReader の Y 面を直接渡す。Dart に返すのは走査の結果
//! (数十 KB) だけ。効果は docs/measurements/2026-11-26-native-camera-probe.md。
//!
//! 走査そのものは Dart から呼ぶ経路と同じ `VcodeRx` (api/vcode.rs)。結果は Dart 側
//! (lib/receive/native_cam.dart) が読む素朴なバイト列にする。並びを変えるときは両方を直す。

use std::sync::Mutex;

use jni_sys::{jboolean, jbyteArray, jclass, jfloat, jint, jlong, jobject, JNIEnv};

use vloom_vcode::{Layout, STRIP_H};

use crate::api::vcode::{VcodeAcquireReport, VcodeRx, VcodeScanReport};

/// 掴めていない間、この画素数を超える画像は縦横半分に縮めて探す。
///
/// 探索 (回転・格子・大きさの総当たり) は画素数に比例して重く、2560x1920 では何も写って
/// いないだけで 1 枚 1〜2 秒かかり、2 分で電池が 4℃ 上がった。位置を掴むだけなら
/// 1280x960 で足りる (従来の 1600x1200 より少し粗い程度)。掴んだら元の解像度で読む
const SEARCH_FULL_RES_PIXELS: usize = 1600 * 1200;

/// 縮めた画像では掴めない写り (セルが細かすぎる) に備えて、この回数に 1 回は元の解像度で探す
const SEARCH_FULL_RES_EVERY: u32 = 3;

/// 走査 1 本ぶんの状態。格子の指定や位置の種は別スレッド (プラットフォームチャネル) から
/// 来るので Mutex で守る
struct Scanner {
    rx: VcodeRx,
    /// 掴めていない状態で探した回数 (元の解像度で探す回を決める)
    searches: u32,
}

type Handle = Mutex<Scanner>;

/// 縦横を半分に縮める (2x2 の平均)。戻りは (画像, 幅, 高さ) で、行の詰め物はない
fn halve(y: &[u8], w: usize, h: usize, stride: usize) -> (Vec<u8>, usize, usize) {
    let (hw, hh) = (w / 2, h / 2);
    let mut out = vec![0u8; hw * hh];
    for (oy, row) in out.chunks_exact_mut(hw).enumerate() {
        let a = &y[2 * oy * stride..2 * oy * stride + 2 * hw];
        let b = &y[(2 * oy + 1) * stride..(2 * oy + 1) * stride + 2 * hw];
        for (ox, d) in row.iter_mut().enumerate() {
            let s = a[2 * ox] as u16 + a[2 * ox + 1] as u16 + b[2 * ox] as u16 + b[2 * ox + 1] as u16;
            *d = ((s + 2) / 4) as u8;
        }
    }
    (out, hw, hh)
}

impl Scanner {
    /// 1 枚走査する。追従できている間は元の解像度のまま読み、掴めていない間は縮めて探す。
    fn scan(&mut self, y: Vec<u8>, w: usize, h: usize, stride: usize, rot: u32) -> VcodeScanReport {
        let searching = !self.rx.is_tracking();
        if searching {
            self.searches = self.searches.wrapping_add(1);
        }
        let shrink = searching
            && w * h > SEARCH_FULL_RES_PIXELS
            && self.searches % SEARCH_FULL_RES_EVERY != 0;
        if !shrink {
            return self.rx.scan(y, w as u32, h as u32, stride as u32, rot, 0.8, false);
        }
        let (small, sw, sh) = halve(&y, w, h, stride);
        let mut rep = self.rx.scan(small, sw as u32, sh as u32, sw as u32, rot, 0.8, false);
        if rep.detected && rep.corners.len() >= 8 {
            // 掴めた。四隅を元の解像度の座標に直して追従の種にし、次の枚から元の解像度で読む。
            // この枚で読めたブロック (CRC は通っている) はそのまま返す
            for c in rep.corners.iter_mut() {
                *c *= 2.0;
            }
            rep.img_w *= 2;
            rep.img_h *= 2;
            let grid_w = rep.cells_w as usize / Layout::BLOCK;
            let grid_h = (rep.cells_h as usize).saturating_sub(2 * STRIP_H) / Layout::BLOCK;
            self.rx.seed(rep.rot, grid_w as u8, grid_h as u8, rep.corners.clone());
        }
        rep
    }
}

unsafe fn handle<'a>(h: jlong) -> &'a Handle {
    &*(h as *const Handle)
}

/// direct ByteBuffer の Y 面を、走査が前提にする stride * height の長さでコピーする。
/// YUV_420_888 の Y 面は最終行が stride に満たないことがあり、足りないぶんは 0 のままにする
unsafe fn copy_plane(env: *mut JNIEnv, buf: jobject, stride: jint, height: jint) -> Option<Vec<u8>> {
    let ptr = ((**env).GetDirectBufferAddress.unwrap())(env, buf) as *const u8;
    let cap = ((**env).GetDirectBufferCapacity.unwrap())(env, buf);
    if ptr.is_null() || cap <= 0 || stride <= 0 || height <= 0 {
        return None;
    }
    let need = stride as usize * height as usize;
    let mut y = vec![0u8; need];
    std::ptr::copy_nonoverlapping(ptr, y.as_mut_ptr(), need.min(cap as usize));
    Some(y)
}

unsafe fn to_byte_array(env: *mut JNIEnv, bytes: &[u8]) -> jbyteArray {
    let arr = ((**env).NewByteArray.unwrap())(env, bytes.len() as jint);
    if !arr.is_null() {
        ((**env).SetByteArrayRegion.unwrap())(env, arr, 0, bytes.len() as jint, bytes.as_ptr() as *const i8);
    }
    arr
}

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, v: &[u8]) {
        self.u32(v.len() as u32);
        self.0.extend_from_slice(v);
    }
    fn floats(&mut self, v: &[f32]) {
        self.u8(v.len() as u8);
        for f in v {
            self.0.extend_from_slice(&f.to_le_bytes());
        }
    }
    fn bools(&mut self, v: &[bool]) {
        self.u32(v.len() as u32);
        self.0.extend(v.iter().map(|&b| b as u8));
    }
}

fn encode_scan(r: &VcodeScanReport) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(64 + r.packets.iter().map(|p| p.len() + 2).sum::<usize>()));
    w.u8(r.detected as u8);
    w.u8(r.tracked as u8);
    for v in [
        r.frame_seq, r.blocks_ok, r.blocks_total, r.img_w, r.img_h, r.cells_w, r.cells_h, r.rot,
        r.rotate_us, r.decode_us,
    ] {
        w.u32(v);
    }
    w.bytes(&r.oti);
    w.floats(&r.corners);
    w.bytes(r.error.as_deref().unwrap_or("").as_bytes());
    w.bools(&r.block_ok);
    w.u32(r.packets.len() as u32);
    for p in &r.packets {
        w.u16(p.len() as u16);
        w.0.extend_from_slice(p);
    }
    w.0
}

fn encode_acquire(r: &VcodeAcquireReport) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(64 + r.block_ok.len()));
    w.u8(r.detected as u8);
    w.u8(r.grid_w);
    w.u8(r.grid_h);
    for v in [r.rot, r.blocks_ok, r.blocks_total, r.img_w, r.img_h, r.cells_w, r.cells_h] {
        w.u32(v);
    }
    w.floats(&r.corners);
    w.bools(&r.block_ok);
    w.0
}

#[no_mangle]
pub extern "system" fn Java_app_vloom_vloom_NativeRx_create(_env: *mut JNIEnv, _class: jclass) -> jlong {
    Box::into_raw(Box::new(Mutex::new(Scanner { rx: VcodeRx::new(), searches: 0 }))) as jlong
}

/// # Safety
/// `h` は create が返した値で、以後使わないこと。
#[no_mangle]
pub unsafe extern "system" fn Java_app_vloom_vloom_NativeRx_destroy(_env: *mut JNIEnv, _class: jclass, h: jlong) {
    if h != 0 {
        drop(Box::from_raw(h as *mut Handle));
    }
}

/// 探索する格子を固定する (grid_w = 0 で自動)。
///
/// # Safety
/// `h` は create が返した生きているハンドル。
#[no_mangle]
pub unsafe extern "system" fn Java_app_vloom_vloom_NativeRx_setLayout(
    _env: *mut JNIEnv,
    _class: jclass,
    h: jlong,
    grid_w: jint,
    grid_h: jint,
) {
    handle(h).lock().unwrap().rx.set_layout(grid_w as u8, grid_h as u8);
}

/// 位置検出の結果 (回転・格子・4 隅) を追従の種にする。
///
/// # Safety
/// `h` は create が返した生きているハンドル。
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "system" fn Java_app_vloom_vloom_NativeRx_seed(
    _env: *mut JNIEnv,
    _class: jclass,
    h: jlong,
    rot: jint,
    grid_w: jint,
    grid_h: jint,
    x0: jfloat,
    y0: jfloat,
    x1: jfloat,
    y1: jfloat,
    x2: jfloat,
    y2: jfloat,
    x3: jfloat,
    y3: jfloat,
) {
    handle(h).lock().unwrap().rx.seed(rot as u32, grid_w as u8, grid_h as u8, vec![x0, y0, x1, y1, x2, y2, x3, y3]);
}

/// 追従の状態を捨てる (受信のやり直し)。格子の指定も消えるので、呼び出し側で入れ直す。
///
/// # Safety
/// `h` は create が返した生きているハンドル。
#[no_mangle]
pub unsafe extern "system" fn Java_app_vloom_vloom_NativeRx_reset(_env: *mut JNIEnv, _class: jclass, h: jlong) {
    handle(h).lock().unwrap().rx = VcodeRx::new();
}

/// Y 面 (direct ByteBuffer) を 1 枚走査し、結果をバイト列で返す。
///
/// # Safety
/// `h` は生きているハンドル、`buf` は direct ByteBuffer。
#[no_mangle]
pub unsafe extern "system" fn Java_app_vloom_vloom_NativeRx_scan(
    env: *mut JNIEnv,
    _class: jclass,
    h: jlong,
    buf: jobject,
    width: jint,
    height: jint,
    stride: jint,
    rot: jint,
) -> jbyteArray {
    let Some(y) = copy_plane(env, buf, stride, height) else { return std::ptr::null_mut() };
    let report = handle(h).lock().unwrap().scan(y, width as usize, height as usize, stride as usize, rot as u32);
    to_byte_array(env, &encode_scan(&report))
}

/// 位置検出 (画面全体の探索)。thorough は手動ボタン用の全探索。
///
/// # Safety
/// `h` は生きているハンドル、`buf` は direct ByteBuffer。
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "system" fn Java_app_vloom_vloom_NativeRx_acquire(
    env: *mut JNIEnv,
    _class: jclass,
    h: jlong,
    buf: jobject,
    width: jint,
    height: jint,
    stride: jint,
    rot: jint,
    thorough: jboolean,
) -> jbyteArray {
    let Some(y) = copy_plane(env, buf, stride, height) else { return std::ptr::null_mut() };
    let (w, hgt, stride) = (width as usize, height as usize, stride as usize);
    let mut scanner = handle(h).lock().unwrap();
    // 位置を掴むだけなので、大きい画像は縮めて探す (理由は SEARCH_FULL_RES_PIXELS)。
    // 手動の全探索 (thorough) も同じ。元の解像度のままだと、何も写っていないときに
    // 結果が出るまで 20 秒以上かかった。縮めると掴めない細かいコードは、毎枚の走査が
    // 元の解像度でも探しているので、そちらで掴める
    let report = if w * hgt > SEARCH_FULL_RES_PIXELS {
        let (small, sw, sh) = halve(&y, w, hgt, stride);
        let mut r = scanner.rx.acquire(small, sw as u32, sh as u32, sw as u32, rot as u32, thorough != 0);
        for c in r.corners.iter_mut() {
            *c *= 2.0;
        }
        r.img_w *= 2;
        r.img_h *= 2;
        if r.detected {
            r
        } else {
            // 縮めると読めない細かいコード (30x20 を画角いっぱいに写すと 1 セル 1.6 画素) に備えて、
            // 元の解像度でも軽い探索 (マーカーから格子を見積もる。総当たりはしない) を試す。
            // 受信側の格子を固定していると毎枚の走査は別の格子を探さないので、ここで掴めないと
            // 「写っているのに掴めない」まま止まる
            scanner.rx.acquire(y, width as u32, height as u32, stride as u32, rot as u32, false)
        }
    } else {
        scanner.rx.acquire(y, width as u32, height as u32, stride as u32, rot as u32, thorough != 0)
    };
    to_byte_array(env, &encode_acquire(&report))
}

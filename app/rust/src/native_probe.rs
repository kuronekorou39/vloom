//! カメラ → スキャンを Flutter を通さずに直結したときの上限を測るための JNI 窓口 (実験用)。
//!
//! 今の受信は、カメラ画像を Flutter (Dart) に渡すだけで UI isolate を 1 枚 20ms 使い、
//! 毎秒 25 枚前後しか処理できない。Kotlin (Camera2) からこの窓口へ Y 面を直接渡し、
//! Rust のスレッドで走査すれば、60fps の撮影や高解像度の撮影に追従できるかを確かめられる。
//! 呼び出し側は android/app/src/profile の ProbeActivity (配布版には入らない)。
//!
//! 走査そのものは受信と同じ `VcodeRx::scan` を呼ぶ (追従の状態はスレッドごとに持つ。
//! アプリの ScanPool と同じ構成)。復元 (fountain) はせず、重複を除いたパケット数を数える。

use std::collections::HashSet;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use jni_sys::{jclass, jint, jobject, jstring, JNIEnv};

use crate::api::vcode::VcodeRx;

struct Job {
    y: Vec<u8>,
    w: u32,
    h: u32,
    stride: u32,
    rot: u32,
}

#[derive(Default)]
struct Stats {
    submitted: AtomicU64,
    /// 走査が追いつかず捨てた枚数 (待ち行列が満杯)
    dropped: AtomicU64,
    scanned: AtomicU64,
    detected: AtomicU64,
    blocks_ok: AtomicU64,
    blocks_total: AtomicU64,
    /// 全ブロック読めた枚数
    full_frames: AtomicU64,
    rotate_us: AtomicU64,
    decode_us: AtomicU64,
    /// 走査 1 回の所要 (コピー後〜結果まで)
    scan_us: AtomicU64,
    unique_bytes: AtomicU64,
    /// 重複を除いたパケット (先頭 4 バイト = SBN + ESI)
    packets: Mutex<HashSet<u32>>,
    /// 1 枚でも読めた送信フレームの番号
    seqs: Mutex<HashSet<u32>>,
}

struct Probe {
    tx: SyncSender<Job>,
    stats: Arc<Stats>,
}

static PROBE: Mutex<Option<Probe>> = Mutex::new(None);

/// コードを掴めていない間の探索 (1 回に数秒かかる) を回してよいのは 1 本だけ。
/// 全部の本で回すと、何も写っていないだけで全コアを使い切り、端末が数分で熱くなる
static SEARCHING: AtomicBool = AtomicBool::new(false);

fn worker(rx: Arc<Mutex<Receiver<Job>>>, stats: Arc<Stats>, grid_w: u8, grid_h: u8) {
    let mut scanner = VcodeRx::new();
    scanner.set_layout(grid_w, grid_h);
    // 直前の枚でコードを掴めていたか
    let mut locked = false;
    loop {
        // 送り手 (PROBE) が捨てられたら終わる
        let Ok(job) = rx.lock().unwrap().recv() else { return };
        let searching = !locked;
        if searching && SEARCHING.swap(true, Relaxed) {
            stats.dropped.fetch_add(1, Relaxed);
            continue; // ほかの本が探している
        }
        let t = std::time::Instant::now();
        let rep = scanner.scan(job.y, job.w, job.h, job.stride, job.rot, 0.85, false);
        if searching {
            SEARCHING.store(false, Relaxed);
        }
        locked = rep.detected;
        stats.scan_us.fetch_add(t.elapsed().as_micros() as u64, Relaxed);
        stats.scanned.fetch_add(1, Relaxed);
        if !rep.detected {
            continue;
        }
        stats.detected.fetch_add(1, Relaxed);
        stats.blocks_ok.fetch_add(rep.blocks_ok as u64, Relaxed);
        stats.blocks_total.fetch_add(rep.blocks_total as u64, Relaxed);
        if rep.blocks_ok == rep.blocks_total {
            stats.full_frames.fetch_add(1, Relaxed);
        }
        stats.rotate_us.fetch_add(rep.rotate_us as u64, Relaxed);
        stats.decode_us.fetch_add(rep.decode_us as u64, Relaxed);
        if rep.blocks_ok > 0 {
            stats.seqs.lock().unwrap().insert(rep.frame_seq);
        }
        let mut seen = stats.packets.lock().unwrap();
        for p in &rep.packets {
            if p.len() > 4 && seen.insert(u32::from_be_bytes([p[0], p[1], p[2], p[3]])) {
                stats.unique_bytes.fetch_add(p.len() as u64 - 4, Relaxed);
            }
        }
    }
}

/// 走査スレッドを `workers` 本立てる。格子は grid_w = 0 で自動 (送信側に合わせる)。
/// 前の計測が残っていれば捨てて数え直す。
#[no_mangle]
pub extern "system" fn Java_app_vloom_vloom_NativeProbe_start(
    _env: *mut JNIEnv,
    _class: jclass,
    workers: jint,
    grid_w: jint,
    grid_h: jint,
) {
    let workers = workers.clamp(1, 8) as usize;
    // 待ち行列は本数ぶんだけ。溜めても古い枚を読むだけで、遅れが積み上がる
    let (tx, rx) = sync_channel::<Job>(workers);
    let rx = Arc::new(Mutex::new(rx));
    let stats = Arc::new(Stats::default());
    for _ in 0..workers {
        let (rx, stats) = (rx.clone(), stats.clone());
        std::thread::spawn(move || worker(rx, stats, grid_w as u8, grid_h as u8));
    }
    *PROBE.lock().unwrap() = Some(Probe { tx, stats });
}

#[no_mangle]
pub extern "system" fn Java_app_vloom_vloom_NativeProbe_stop(_env: *mut JNIEnv, _class: jclass) {
    *PROBE.lock().unwrap() = None;
}

/// Y 面 (direct ByteBuffer) を 1 枚渡す。コピーしてすぐ返るので、呼び出し側は画像を閉じてよい。
/// 走査が追いつかず捨てたときは false。
///
/// # Safety
/// JNI から呼ばれる。`buf` は direct ByteBuffer であること。
#[no_mangle]
pub unsafe extern "system" fn Java_app_vloom_vloom_NativeProbe_submit(
    env: *mut JNIEnv,
    _class: jclass,
    buf: jobject,
    width: jint,
    height: jint,
    stride: jint,
    rot: jint,
) -> jint {
    let guard = PROBE.lock().unwrap();
    let Some(probe) = guard.as_ref() else { return 0 };
    let ptr = ((**env).GetDirectBufferAddress.unwrap())(env, buf) as *const u8;
    let cap = ((**env).GetDirectBufferCapacity.unwrap())(env, buf);
    if ptr.is_null() || cap <= 0 || width <= 0 || height <= 0 || stride < width {
        return 0;
    }
    probe.stats.submitted.fetch_add(1, Relaxed);
    // YUV_420_888 の Y 面は最終行が stride に満たないことがある。走査は stride * height を
    // 前提にするので、その長さで確保して足りないぶんは 0 のままにする
    let need = stride as usize * height as usize;
    let mut y = vec![0u8; need];
    let n = need.min(cap as usize);
    std::ptr::copy_nonoverlapping(ptr, y.as_mut_ptr(), n);
    let job = Job { y, w: width as u32, h: height as u32, stride: stride as u32, rot: rot as u32 };
    match probe.tx.try_send(job) {
        Ok(()) => 1,
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
            probe.stats.dropped.fetch_add(1, Relaxed);
            0
        }
    }
}

/// 累計をカンマ区切りで返す (並びは ProbeActivity の STAT_NAMES と揃える)。
///
/// # Safety
/// JNI から呼ばれる。
#[no_mangle]
pub unsafe extern "system" fn Java_app_vloom_vloom_NativeProbe_stats(
    env: *mut JNIEnv,
    _class: jclass,
) -> jstring {
    let line = match PROBE.lock().unwrap().as_ref() {
        None => String::new(),
        Some(p) => {
            let s = &p.stats;
            [
                s.submitted.load(Relaxed),
                s.dropped.load(Relaxed),
                s.scanned.load(Relaxed),
                s.detected.load(Relaxed),
                s.blocks_ok.load(Relaxed),
                s.blocks_total.load(Relaxed),
                s.full_frames.load(Relaxed),
                s.rotate_us.load(Relaxed),
                s.decode_us.load(Relaxed),
                s.scan_us.load(Relaxed),
                s.unique_bytes.load(Relaxed),
                s.packets.lock().unwrap().len() as u64,
                s.seqs.lock().unwrap().len() as u64,
            ]
            .map(|v| v.to_string())
            .join(",")
        }
    };
    let c = CString::new(line).unwrap();
    ((**env).NewStringUTF.unwrap())(env, c.as_ptr())
}

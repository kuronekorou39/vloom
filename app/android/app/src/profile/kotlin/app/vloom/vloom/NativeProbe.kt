package app.vloom.vloom

import java.nio.ByteBuffer

/// Rust 側 (app/rust/src/native_probe.rs) の窓口。走査のスレッドは Rust が持つ。
object NativeProbe {
    init {
        System.loadLibrary("rust_lib_vloom")
    }

    @JvmStatic external fun start(workers: Int, gridW: Int, gridH: Int)
    @JvmStatic external fun stop()
    /// Y 面を 1 枚渡す (Rust 側でコピーするので、戻ったら画像を閉じてよい)。捨てられたら 0
    @JvmStatic external fun submit(y: ByteBuffer, width: Int, height: Int, stride: Int, rot: Int): Int
    @JvmStatic external fun stats(): String
}

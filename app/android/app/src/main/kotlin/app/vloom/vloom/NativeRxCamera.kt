package app.vloom.vloom

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.pm.PackageManager
import android.graphics.ImageFormat
import android.graphics.Rect
import android.graphics.SurfaceTexture
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.TotalCaptureResult
import android.hardware.camera2.params.MeteringRectangle
import android.media.ImageReader
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import android.util.Range
import android.util.Size
import android.view.OrientationEventListener
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.EventChannel
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel
import io.flutter.view.TextureRegistry
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import kotlin.math.roundToInt

/// Rust 側 (app/rust/src/native_rx.rs) の走査ハンドル。1 本のスレッドが 1 つ持つ。
object NativeRx {
    init {
        System.loadLibrary("rust_lib_vloom")
    }

    @JvmStatic external fun create(): Long
    @JvmStatic external fun destroy(h: Long)
    @JvmStatic external fun setLayout(h: Long, gridW: Int, gridH: Int)
    @JvmStatic external fun seed(
        h: Long, rot: Int, gridW: Int, gridH: Int,
        x0: Float, y0: Float, x1: Float, y1: Float, x2: Float, y2: Float, x3: Float, y3: Float,
    )
    @JvmStatic external fun reset(h: Long)
    @JvmStatic external fun scan(h: Long, y: ByteBuffer, width: Int, height: Int, stride: Int, rot: Int): ByteArray?
    @JvmStatic external fun acquire(
        h: Long, y: ByteBuffer, width: Int, height: Int, stride: Int, rot: Int, thorough: Boolean,
    ): ByteArray?
}

/// 受信用のカメラ。Camera2 の画像を Flutter を通さずに Rust の走査へ渡し、Dart には
/// 走査の結果だけを流す (lib/receive/native_cam.dart が相手)。
///
/// Flutter の camera プラグインを使っていたときは、画像 (数 MB) を Dart に渡すだけで
/// UI isolate を 1 枚 20ms 使い、解像度も 1600x1200 までしか選べなかった。直結すると
/// 2560x1920 を毎秒 30 枚そのまま走査でき、同じ置き方で 2〜4 倍拾える
/// (docs/measurements/2026-11-26-native-camera-probe.md)。
///
/// プレビューは Flutter の Texture に直接描く。走査は [workers] 本のスレッドで回し、
/// 追従の状態はスレッドごとに持つ (Dart の ScanPool と同じ構成)。
class NativeRxCamera(
    private val activity: Activity,
    private val textures: TextureRegistry,
    messenger: BinaryMessenger,
) : MethodChannel.MethodCallHandler, EventChannel.StreamHandler {
    private val ui = Handler(Looper.getMainLooper())
    private val camThread = HandlerThread("vloom-cam").apply { start() }
    private val cam = Handler(camThread.looper)
    private val readThread = HandlerThread("vloom-read").apply { start() }
    private val read = Handler(readThread.looper)

    @Volatile private var sink: EventChannel.EventSink? = null

    // --- 開いている間だけ有効 (cam スレッドで触る) ---
    private var device: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var reader: ImageReader? = null
    private var producer: TextureRegistry.SurfaceProducer? = null
    private var pool: ExecutorService? = null
    private var handles = LongArray(0)
    private var freeHandles = ArrayBlockingQueue<Long>(1)
    private var chars: CameraCharacteristics? = null
    private var sensorRot = 90
    private var fps = 30

    /// 端末の傾き (0 = 縦持ち、90/270 = 横持ち、180 = 逆さ)。横長のコードは端末を横にして
    /// 読むので、走査の前に画像を起こす角度をこれに合わせる
    @Volatile private var deviceRot = 0
    private var orientation: OrientationEventListener? = null

    /// 走査の前に画像を起こす角度 (背面カメラ)
    private val scanRot get() = (sensorRot + deviceRot) % 360

    // --- 走査の間引きと位置検出 (読み出しスレッドと行き来する) ---
    private val camFrames = AtomicInteger()
    @Volatile private var scanIntervalMs = 0L
    @Volatile private var lastScanAt = 0L
    @Volatile private var pendingAcquire: Pair<Boolean, MethodChannel.Result>? = null
    private val acquireBusy = AtomicBoolean(false)

    /// 最後に Dart へ何か流した時刻。走査の結果が出ない間 (探索に 1 枚 1〜2 秒かかる、
    /// 間引いている) も、カメラが生きていることを知らせるのに使う
    @Volatile private var lastEventAt = 0L

    // --- カメラの設定 (変えるたびにリクエストを作り直す) ---
    private var evIndex = 0
    private var meterCenter = true
    private var aeLocked = false
    private var afLocked = false
    private var expUs = 0L
    private var iso = 0
    @Volatile private var lastFocusDist: Float? = null

    private var pendingStart: Pair<MethodCall, MethodChannel.Result>? = null

    init {
        MethodChannel(messenger, "app.vloom/nativecam").setMethodCallHandler(this)
        EventChannel(messenger, "app.vloom/nativecam/reports").setStreamHandler(this)
    }

    override fun onListen(arguments: Any?, events: EventChannel.EventSink?) {
        sink = events
    }

    override fun onCancel(arguments: Any?) {
        sink = null
    }

    override fun onMethodCall(call: MethodCall, result: MethodChannel.Result) {
        when (call.method) {
            "start" -> {
                if (activity.checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
                    launch(call, result)
                } else {
                    pendingStart = call to result
                    activity.requestPermissions(arrayOf(Manifest.permission.CAMERA), PERMISSION_REQUEST)
                }
            }
            "stop" -> cam.post {
                stop()
                ui.post { result.success(null) }
            }
            "setLayout" -> {
                val (gw, gh) = call.argument<Int>("gridW")!! to call.argument<Int>("gridH")!!
                cam.post {
                    handles.forEach { NativeRx.setLayout(it, gw, gh) }
                    ui.post { result.success(null) }
                }
            }
            "seed" -> {
                val c = call.argument<List<Double>>("corners")!!.map { it.toFloat() }
                val r = call.argument<Int>("rot")!!
                val (gw, gh) = call.argument<Int>("gridW")!! to call.argument<Int>("gridH")!!
                cam.post {
                    if (c.size >= 8) {
                        handles.forEach { NativeRx.seed(it, r, gw, gh, c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]) }
                    }
                    ui.post { result.success(null) }
                }
            }
            "reset" -> cam.post {
                handles.forEach { NativeRx.reset(it) }
                ui.post { result.success(null) }
            }
            // 次に届く 1 枚で位置検出を走らせ、結果を返す
            "acquire" -> {
                pendingAcquire?.second?.success(null) // 前の要求は取り消し扱い
                pendingAcquire = (call.argument<Boolean>("thorough") ?: false) to result
            }
            // 何も写っていない間の省電力: 走査をこの間隔 (ms) に間引く。0 で全部走査する
            "setScanInterval" -> {
                scanIntervalMs = (call.argument<Int>("ms") ?: 0).toLong()
                result.success(null)
            }
            "lockExposure" -> cam.post {
                aeLocked = call.argument<Boolean>("lock") ?: false
                applyRequest()
                ui.post { result.success(null) }
            }
            "lockFocus" -> cam.post {
                afLocked = call.argument<Boolean>("lock") ?: false
                applyRequest()
                ui.post { result.success(null) }
            }
            else -> result.notImplemented()
        }
    }

    /// MainActivity.onRequestPermissionsResult から呼ぶ。
    fun onPermissionResult(requestCode: Int, grantResults: IntArray) {
        if (requestCode != PERMISSION_REQUEST) return
        val (call, result) = pendingStart ?: return
        pendingStart = null
        if (grantResults.firstOrNull() == PackageManager.PERMISSION_GRANTED) {
            launch(call, result)
        } else {
            result.error("permission", "カメラの権限がありません", null)
        }
    }

    /// 前のカメラを片付けてから開く。片付けは cam スレッド、開く準備は UI スレッド
    /// (Flutter の Texture は UI スレッドでしか作れない)。
    private fun launch(call: MethodCall, result: MethodChannel.Result) {
        cam.post {
            stop()
            ui.post { start(call, result) }
        }
    }

    /// カメラを開く (UI スレッド)。以降のカメラの通知は cam スレッドに届く。
    private fun start(call: MethodCall, result: MethodChannel.Result) {
        try {
            val mgr = activity.getSystemService(Context.CAMERA_SERVICE) as CameraManager
            val id = mgr.cameraIdList.firstOrNull {
                mgr.getCameraCharacteristics(it).get(CameraCharacteristics.LENS_FACING) ==
                    CameraCharacteristics.LENS_FACING_BACK
            } ?: mgr.cameraIdList.first()
            val ch = mgr.getCameraCharacteristics(id)
            chars = ch
            sensorRot = ch.get(CameraCharacteristics.SENSOR_ORIENTATION) ?: 90
            deviceRot = 0
            orientation = object : OrientationEventListener(activity) {
                override fun onOrientationChanged(deg: Int) {
                    if (deg == ORIENTATION_UNKNOWN) return // 平置き。直前の向きのままにする
                    // 境目 (45 度) の近くでばたつかないよう、向きの中心に十分寄ってから切り替える
                    val nearest = ((deg + 45) / 90 % 4) * 90
                    val off = Math.floorMod(deg - nearest + 180, 360) - 180
                    if (nearest != deviceRot && kotlin.math.abs(off) <= ORIENTATION_SNAP_DEG) {
                        deviceRot = nearest
                    }
                }
            }.apply { enable() }
            fps = call.argument<Int>("fps") ?: 30
            val workers = (call.argument<Int>("workers") ?: DEFAULT_WORKERS).coerceIn(1, 4)
            val size = pickCaptureSize(ch, call.argument<Int>("width") ?: 0, call.argument<Int>("height") ?: 0)
            val preview = pickPreviewSize(ch, size)

            val step = ch.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_STEP)?.toDouble() ?: 0.0
            val evRange = ch.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_RANGE)
            val ev = call.argument<Double>("ev") ?: 0.0
            evIndex = if (step > 0 && evRange != null) {
                (ev / step).roundToInt().coerceIn(evRange.lower, evRange.upper)
            } else 0
            meterCenter = call.argument<Boolean>("meterCenter") ?: true
            expUs = (call.argument<Int>("exposureUs") ?: 0).toLong()
            iso = call.argument<Int>("iso") ?: 0
            aeLocked = false
            afLocked = false
            lastFocusDist = null
            scanIntervalMs = 0
            camFrames.set(0)

            handles = LongArray(workers) { NativeRx.create() }
            freeHandles = ArrayBlockingQueue<Long>(workers).apply { handles.forEach { add(it) } }
            pool = Executors.newFixedThreadPool(workers)

            val sp = textures.createSurfaceProducer()
            sp.setSize(preview.width, preview.height)
            producer = sp
            // 走査中の枚 (本数ぶん) と、受け取って配るまでの 2 枚
            val rd = ImageReader.newInstance(size.width, size.height, ImageFormat.YUV_420_888, workers + 2)
            reader = rd
            rd.setOnImageAvailableListener({ onImage(it) }, read)

            val reply = mapOf(
                "textureId" to sp.id(),
                "width" to size.width,
                "height" to size.height,
                "sensorOrientation" to sensorRot,
                // false のときは Dart 側でプレビューを sensorOrientation ぶん回す
                "textureRotated" to sp.handlesCropAndRotation(),
            )
            mgr.openCamera(id, object : CameraDevice.StateCallback() {
                override fun onOpened(dev: CameraDevice) {
                    device = dev
                    openSession(dev, rd, sp, reply, result)
                }

                override fun onDisconnected(dev: CameraDevice) {
                    dev.close()
                    if (device === dev) device = null
                }

                override fun onError(dev: CameraDevice, error: Int) {
                    dev.close()
                    if (device === dev) {
                        device = null
                    } else {
                        // まだ開けていなかった (ほかのアプリが掴んでいる等)
                        ui.post { result.error("camera", "カメラを開けません ($error)", null) }
                    }
                }
            }, cam)
        } catch (e: Throwable) {
            Log.e(TAG, "start failed", e)
            cam.post { stop() }
            result.error("camera", e.toString(), null)
        }
    }

    @Suppress("DEPRECATION")
    private fun openSession(
        dev: CameraDevice,
        rd: ImageReader,
        sp: TextureRegistry.SurfaceProducer,
        reply: Map<String, Any>,
        result: MethodChannel.Result,
    ) {
        try {
            dev.createCaptureSession(listOf(rd.surface, sp.surface), object : CameraCaptureSession.StateCallback() {
                override fun onConfigured(s: CameraCaptureSession) {
                    session = s
                    applyRequest()
                    ui.post { result.success(reply) }
                }

                override fun onConfigureFailed(s: CameraCaptureSession) {
                    ui.post { result.error("camera", "この解像度では撮れません", null) }
                }
            }, cam)
        } catch (e: Throwable) {
            Log.e(TAG, "session failed", e)
            ui.post { result.error("camera", e.toString(), null) }
        }
    }

    /// 走査に渡す解像度。指定が使えなければ、4:3 で毎秒 [fps] 枚出せるいちばん大きいものにする。
    private fun pickCaptureSize(ch: CameraCharacteristics, w: Int, h: Int): Size {
        val map = ch.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)!!
        val frameNs = 1_000_000_000L / fps
        // 端末の申告は 33.33ms ちょうどではないことがあるので 5% の余裕を見る
        val fast = map.getOutputSizes(ImageFormat.YUV_420_888).filter {
            map.getOutputMinFrameDuration(ImageFormat.YUV_420_888, it) <= frameNs * 105 / 100
        }
        fast.firstOrNull { it.width == w && it.height == h }?.let { return it }
        val fourThree = fast.filter { it.width * 3 == it.height * 4 && it.width <= MAX_CAPTURE_WIDTH }
        return (fourThree.ifEmpty { fast }).maxByOrNull { it.width.toLong() * it.height }
            ?: Size(1280, 720)
    }

    /// プレビューは走査の画像と同じ縦横比で、画面に足りる程度の大きさ (帯域を食わせない)。
    private fun pickPreviewSize(ch: CameraCharacteristics, capture: Size): Size {
        val map = ch.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)!!
        val same = map.getOutputSizes(SurfaceTexture::class.java)
            .filter { it.width * capture.height == it.height * capture.width }
        return same.filter { it.width >= PREVIEW_MIN_WIDTH }.minByOrNull { it.width }
            ?: same.maxByOrNull { it.width }
            ?: capture
    }

    /// いまの設定でリクエストを作り直して流す (cam スレッド)。
    private fun applyRequest() {
        val dev = device ?: return
        val s = session ?: return
        val rd = reader ?: return
        val sp = producer ?: return
        val ch = chars ?: return
        try {
            val b = dev.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW)
            b.addTarget(rd.surface)
            b.addTarget(sp.surface)
            // 暗いときに枚数を落として露光を伸ばされると、表示の切り替わりが 1 枚に混ざる
            val ranges = ch.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES)
            val range = ranges?.firstOrNull { it.lower == fps && it.upper == fps }
                ?: ranges?.filter { it.upper == fps }?.maxByOrNull { it.lower }
            if (range != null) b.set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, range)
            // 手ぶれ補正は画角を切って補間するので、セルの境目がぼける
            b.set(
                CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE,
                CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE_OFF,
            )
            // ピントの固定は「いまのレンズ位置のまま AF を切る」。AF のモードを切り替えて
            // 固定させると、合わせ直しでフレームの供給が 0.5〜5 秒止まることがあった
            val dist = lastFocusDist
            val canManualFocus = (ch.get(CameraCharacteristics.LENS_INFO_MINIMUM_FOCUS_DISTANCE) ?: 0f) > 0f
            if (afLocked && dist != null && canManualFocus) {
                b.set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_OFF)
                b.set(CaptureRequest.LENS_FOCUS_DISTANCE, dist)
            } else {
                b.set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE)
            }
            if (expUs > 0) {
                b.set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_OFF)
                b.set(CaptureRequest.SENSOR_EXPOSURE_TIME, expUs * 1000L)
                b.set(CaptureRequest.SENSOR_SENSITIVITY, iso)
                b.set(CaptureRequest.SENSOR_FRAME_DURATION, 1_000_000_000L / fps)
            } else {
                b.set(CaptureRequest.CONTROL_AE_EXPOSURE_COMPENSATION, evIndex)
                b.set(CaptureRequest.CONTROL_AE_LOCK, aeLocked)
            }
            if (meterCenter) {
                // 測光・測距は画角の中心 (= コード) で。周りが黒いと全体の平均に引かれて白が飛ぶ
                val a = ch.get(CameraCharacteristics.SENSOR_INFO_ACTIVE_ARRAY_SIZE)
                if (a != null) {
                    val (hw, hh) = a.width() / 6 to a.height() / 6
                    val box = Rect(a.centerX() - hw, a.centerY() - hh, a.centerX() + hw, a.centerY() + hh)
                    val region = arrayOf(MeteringRectangle(box, MeteringRectangle.METERING_WEIGHT_MAX))
                    if ((ch.get(CameraCharacteristics.CONTROL_MAX_REGIONS_AE) ?: 0) > 0) {
                        b.set(CaptureRequest.CONTROL_AE_REGIONS, region)
                    }
                    if ((ch.get(CameraCharacteristics.CONTROL_MAX_REGIONS_AF) ?: 0) > 0) {
                        b.set(CaptureRequest.CONTROL_AF_REGIONS, region)
                    }
                }
            }
            s.setRepeatingRequest(b.build(), object : CameraCaptureSession.CaptureCallback() {
                override fun onCaptureCompleted(
                    session: CameraCaptureSession,
                    request: CaptureRequest,
                    result: TotalCaptureResult,
                ) {
                    result.get(TotalCaptureResult.LENS_FOCUS_DISTANCE)?.let { lastFocusDist = it }
                }
            }, cam)
        } catch (e: Throwable) {
            Log.e(TAG, "applyRequest failed", e)
        }
    }

    /// カメラから 1 枚届いた (読み出しスレッド)。空いている走査スレッドがあれば渡し、
    /// なければ捨てる (溜めても古い枚を読むだけで、遅れが積み上がる)。
    private fun onImage(rd: ImageReader) {
        val img = try {
            rd.acquireNextImage()
        } catch (e: IllegalStateException) {
            null // 走査中の枚で上限いっぱい。次の枚を待つ
        } ?: return
        val frames = camFrames.incrementAndGet()
        // 走査の結果がしばらく出ていなければ、届いた枚数だけを知らせる。Dart 側の見張りは
        // 「2 秒何も来なければカメラを作り直す」ので、走査が遅いだけで作り直しが続いていた
        val tick = SystemClock.elapsedRealtime()
        if (tick - lastEventAt >= HEARTBEAT_MS) {
            lastEventAt = tick
            val beat = ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN).putInt(frames).array()
            ui.post { sink?.success(beat) }
        }
        val exec = pool
        if (exec == null) {
            img.close()
            return
        }
        val plane = img.planes[0]
        val (w, h, stride) = Triple(img.width, img.height, plane.rowStride)
        val rot = scanRot
        try {
            val acq = pendingAcquire
            if (acq != null && acquireBusy.compareAndSet(false, true)) {
                pendingAcquire = null
                exec.execute {
                    var bytes: ByteArray? = null
                    try {
                        val hd = freeHandles.take()
                        try {
                            bytes = NativeRx.acquire(hd, plane.buffer, w, h, stride, rot, acq.first)
                        } finally {
                            freeHandles.offer(hd)
                        }
                    } catch (e: Throwable) {
                        Log.e(TAG, "acquire failed", e)
                    } finally {
                        img.close()
                        acquireBusy.set(false)
                    }
                    ui.post { acq.second.success(bytes) }
                }
                return
            }
            val now = SystemClock.elapsedRealtime()
            val interval = scanIntervalMs
            val hd = if (interval > 0 && now - lastScanAt < interval) null else freeHandles.poll()
            if (hd == null) {
                img.close()
                return
            }
            lastScanAt = now
            exec.execute {
                val t0 = SystemClock.elapsedRealtimeNanos()
                val report = try {
                    NativeRx.scan(hd, plane.buffer, w, h, stride, rot)
                } catch (e: Throwable) {
                    Log.e(TAG, "scan failed", e)
                    null
                } finally {
                    img.close()
                    freeHandles.offer(hd)
                }
                if (report != null) {
                    val scanMs = ((SystemClock.elapsedRealtimeNanos() - t0) / 1_000_000).toInt()
                    // 先頭 12 バイト: カメラから届いた累計の枚数、この枚の走査にかかった ms、
                    // 画像を起こした角度
                    val out = ByteBuffer.allocate(12 + report.size).order(ByteOrder.LITTLE_ENDIAN)
                    out.putInt(frames).putInt(scanMs).putInt(rot).put(report)
                    lastEventAt = SystemClock.elapsedRealtime()
                    ui.post { sink?.success(out.array()) }
                }
            }
        } catch (e: RejectedExecutionException) {
            img.close() // 止めている最中
        }
    }

    /// カメラと走査を止めて全部手放す (cam スレッド)。
    private fun stop() {
        pendingAcquire?.second?.let { r -> ui.post { r.success(null) } }
        pendingAcquire = null
        orientation?.let { o -> ui.post { o.disable() } }
        orientation = null
        try {
            session?.close()
        } catch (_: Throwable) {
        }
        session = null
        device?.close()
        device = null
        // 走査中のスレッドが画像を読み終えるのを待ってから、画像とハンドルを手放す
        val exec = pool
        pool = null
        exec?.shutdown()
        val drained = exec?.awaitTermination(STOP_WAIT_MS, TimeUnit.MILLISECONDS) ?: true
        reader?.close()
        reader = null
        producer?.let { sp -> ui.post { sp.release() } } // Texture の解放も UI スレッドで
        producer = null
        // 止まりきらなかったら、使用中のハンドルを消すより漏らすほうを選ぶ (数百バイト)
        if (drained) handles.forEach { NativeRx.destroy(it) }
        handles = LongArray(0)
        acquireBusy.set(false)
    }

    companion object {
        private const val TAG = "VloomCam"
        private const val PERMISSION_REQUEST = 3901
        private const val DEFAULT_WORKERS = 3

        /// 自動で選ぶ解像度の上限。これより大きいと回転コピーと走査が毎秒 30 枚に間に合わない
        private const val MAX_CAPTURE_WIDTH = 2560

        /// プレビューに使う解像度の下限 (画面の幅に足りる程度)
        private const val PREVIEW_MIN_WIDTH = 1280

        /// 端末の向きを切り替える範囲 (向きの中心からの角度)。45 が境目
        private const val ORIENTATION_SNAP_DEG = 30

        /// 走査の結果が出ない間、カメラが生きていることを知らせる間隔
        private const val HEARTBEAT_MS = 500L

        /// 止めるとき、走査中のスレッドを待つ上限
        private const val STOP_WAIT_MS = 3000L
    }
}

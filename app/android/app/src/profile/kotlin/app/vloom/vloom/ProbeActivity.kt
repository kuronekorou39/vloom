package app.vloom.vloom

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.graphics.Color
import android.graphics.ImageFormat
import android.graphics.Typeface
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.media.ImageReader
import android.os.BatteryManager
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.util.Log
import android.util.Range
import android.util.Size
import android.view.Gravity
import android.view.Surface
import android.graphics.SurfaceTexture
import android.view.TextureView
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.TextView
import java.util.concurrent.atomic.AtomicLong

/// カメラ → 走査を Flutter を通さず直結したときの上限を測る画面 (実験用。profile ビルドだけに入る)。
///
/// Camera2 の画像を ImageReader で受け、Y 面をそのまま Rust (NativeProbe) に渡す。
/// 1 秒ごとに「届いた枚数・走査できた枚数・読めたブロック・重複を除いたバイト数」を
/// 画面と logcat (タグ VloomProbe) に出す。復元まではしない。
///
///   adb shell am start -n app.vloom.vloom.probe/app.vloom.vloom.ProbeActivity \
///       --ei w 1920 --ei h 1080 --ei fps 60 --ei workers 3 --es grid 15x20 --ei exp 2000 --ei iso 400
///
/// w/h: 撮る解像度、fps: 撮る枚数、workers: 走査のスレッド数、grid: 格子 (省略で自動)、
/// exp: 露光 (マイクロ秒。省略で自動露出)、iso: ゲイン、preview: 0 でプレビューなし
class ProbeActivity : Activity() {
    private lateinit var preview: TextureView
    private lateinit var text: TextView
    private val camThread = HandlerThread("probe-cam").apply { start() }
    private val camHandler = Handler(camThread.looper)
    private val readThread = HandlerThread("probe-read").apply { start() }
    private val readHandler = Handler(readThread.looper)
    private val ui = Handler(Looper.getMainLooper())

    private var camera: CameraDevice? = null
    private var reader: ImageReader? = null
    private var rot = 90

    private var capW = 1920
    private var capH = 1080
    private var capFps = 60
    private var workers = 3
    private var gridW = 0
    private var gridH = 0
    private var expUs = 0
    private var iso = 400
    private var withPreview = true

    // カメラから届いた枚数と、届く間隔の最大 (センサーの時刻で測る)
    private val camFrames = AtomicLong()
    private val maxGapUs = AtomicLong()
    private var lastSensorNs = 0L

    private var startMs = 0L
    private var prev = LongArray(STAT_COUNT)
    private var prevCam = 0L

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        capW = intent.getIntExtra("w", capW)
        capH = intent.getIntExtra("h", capH)
        capFps = intent.getIntExtra("fps", capFps)
        workers = intent.getIntExtra("workers", workers)
        expUs = intent.getIntExtra("exp", expUs)
        iso = intent.getIntExtra("iso", iso)
        withPreview = intent.getIntExtra("preview", 1) != 0
        intent.getStringExtra("grid")?.split("x")?.mapNotNull { it.toIntOrNull() }?.let {
            if (it.size == 2) { gridW = it[0]; gridH = it[1] }
        }

        preview = TextureView(this)
        text = TextView(this).apply {
            setTextColor(Color.WHITE)
            setBackgroundColor(0xB0000000.toInt())
            typeface = Typeface.MONOSPACE
            textSize = 13f
            setPadding(24, 24, 24, 24)
        }
        // プレビューは縦持ちで幅いっぱい。高さは撮る縦横比に合わせる (センサーは横長)
        val screenW = resources.displayMetrics.widthPixels
        val root = FrameLayout(this).apply {
            setBackgroundColor(Color.BLACK)
            fitsSystemWindows = true // ステータスバー・ナビゲーションバーに潜らせない
        }
        root.addView(preview, FrameLayout.LayoutParams(screenW, screenW * capW / capH, Gravity.TOP))
        root.addView(
            text,
            FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT,
                FrameLayout.LayoutParams.WRAP_CONTENT,
                Gravity.BOTTOM,
            ),
        )
        setContentView(root)

        // TextureView は縦持ちなら、ビューの縦横比を画像 (を 90 度回したもの) に合わせるだけで正しく映る
        preview.surfaceTextureListener = object : TextureView.SurfaceTextureListener {
            override fun onSurfaceTextureAvailable(st: SurfaceTexture, width: Int, height: Int) {
                if (checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
                    openCamera()
                } else {
                    requestPermissions(arrayOf(Manifest.permission.CAMERA), 1)
                }
            }
            override fun onSurfaceTextureSizeChanged(st: SurfaceTexture, width: Int, height: Int) {}
            override fun onSurfaceTextureDestroyed(st: SurfaceTexture): Boolean {
                closeCamera()
                return true
            }
            override fun onSurfaceTextureUpdated(st: SurfaceTexture) {}
        }
    }

    override fun onRequestPermissionsResult(code: Int, perms: Array<out String>, results: IntArray) {
        if (results.firstOrNull() == PackageManager.PERMISSION_GRANTED) openCamera()
        else text.text = "カメラの権限がありません"
    }

    private fun openCamera() {
        val mgr = getSystemService(Context.CAMERA_SERVICE) as CameraManager
        val id = mgr.cameraIdList.first {
            mgr.getCameraCharacteristics(it).get(CameraCharacteristics.LENS_FACING) ==
                CameraCharacteristics.LENS_FACING_BACK
        }
        val chars = mgr.getCameraCharacteristics(id)
        rot = chars.get(CameraCharacteristics.SENSOR_ORIENTATION) ?: 90
        val map = chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)!!
        val ranges = chars.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES)
        Log.i(TAG, "fps ranges: ${ranges?.joinToString()}")
        Log.i(
            TAG,
            "min frame duration ${capW}x$capH: " +
                "${map.getOutputMinFrameDuration(ImageFormat.YUV_420_888, Size(capW, capH)) / 1000} us",
        )

        // プレビューは撮る画像と同じ縦横比で、いちばん小さいもの (帯域を食わせない)
        val pv = map.getOutputSizes(SurfaceTexture::class.java)
            .filter { it.width * capH == it.height * capW && it.width >= 640 }
            .minByOrNull { it.width } ?: Size(capW, capH)
        preview.surfaceTexture?.setDefaultBufferSize(pv.width, pv.height)

        val r = ImageReader.newInstance(capW, capH, ImageFormat.YUV_420_888, 6)
        reader = r
        r.setOnImageAvailableListener({ rd ->
            val img = rd.acquireNextImage() ?: return@setOnImageAvailableListener
            val ts = img.timestamp
            if (lastSensorNs != 0L) {
                val gap = (ts - lastSensorNs) / 1000
                if (gap > maxGapUs.get()) maxGapUs.set(gap)
            }
            lastSensorNs = ts
            camFrames.incrementAndGet()
            val p = img.planes[0]
            NativeProbe.submit(p.buffer, img.width, img.height, p.rowStride, rot)
            img.close()
        }, readHandler)

        NativeProbe.start(workers, gridW, gridH)
        startMs = SystemClock.elapsedRealtime()
        prev = LongArray(STAT_COUNT)
        prevCam = 0
        camFrames.set(0)

        try {
            mgr.openCamera(id, object : CameraDevice.StateCallback() {
                override fun onOpened(dev: CameraDevice) {
                    camera = dev
                    startSession(dev, r)
                }
                override fun onDisconnected(dev: CameraDevice) = dev.close()
                override fun onError(dev: CameraDevice, error: Int) {
                    Log.e(TAG, "camera error $error")
                    dev.close()
                }
            }, camHandler)
        } catch (e: SecurityException) {
            Log.e(TAG, "no camera permission", e)
        }
        ui.postDelayed(tick, 1000)
    }

    @Suppress("DEPRECATION")
    private fun startSession(dev: CameraDevice, r: ImageReader) {
        val targets = mutableListOf<Surface>(r.surface)
        if (withPreview) preview.surfaceTexture?.let { targets.add(Surface(it)) }
        dev.createCaptureSession(targets, object : CameraCaptureSession.StateCallback() {
            override fun onConfigured(session: CameraCaptureSession) {
                val req = dev.createCaptureRequest(CameraDevice.TEMPLATE_RECORD)
                targets.forEach { req.addTarget(it) }
                req.set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, Range(capFps, capFps))
                req.set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO)
                // 手ぶれ補正は画角を切って補間するので、セルの境目がぼける
                req.set(
                    CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE,
                    CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE_OFF,
                )
                if (expUs > 0) {
                    req.set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_OFF)
                    req.set(CaptureRequest.SENSOR_EXPOSURE_TIME, expUs * 1000L)
                    req.set(CaptureRequest.SENSOR_SENSITIVITY, iso)
                    req.set(CaptureRequest.SENSOR_FRAME_DURATION, 1_000_000_000L / capFps)
                }
                session.setRepeatingRequest(req.build(), null, camHandler)
            }

            override fun onConfigureFailed(session: CameraCaptureSession) {
                Log.e(TAG, "session configure failed (${capW}x$capH @$capFps preview=$withPreview)")
                ui.post { text.text = "この組み合わせでは撮れません: ${capW}x$capH @$capFps" }
            }
        }, camHandler)
    }

    private fun closeCamera() {
        ui.removeCallbacks(tick)
        camera?.close()
        camera = null
        reader?.close()
        reader = null
        NativeProbe.stop()
    }

    private val tick = object : Runnable {
        override fun run() {
            val line = NativeProbe.stats()
            if (line.isNotEmpty()) report(line.split(",").map { it.toLong() }.toLongArray())
            ui.postDelayed(this, 1000)
        }
    }

    private fun report(cur: LongArray) {
        val d = LongArray(cur.size) { cur[it] - prev[it] }
        prev = cur
        val cam = camFrames.get()
        val dCam = cam - prevCam
        prevCam = cam
        val gap = maxGapUs.getAndSet(0)
        val secs = ((SystemClock.elapsedRealtime() - startMs) / 1000.0).coerceAtLeast(1.0)
        val battery = registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        val tempC = (battery?.getIntExtra(BatteryManager.EXTRA_TEMPERATURE, 0) ?: 0) / 10.0
        val thermal = (getSystemService(Context.POWER_SERVICE) as PowerManager).currentThermalStatus
        val scanned = d[SCANNED].coerceAtLeast(1)
        val det = d[DETECTED].coerceAtLeast(1)
        val readPct = if (d[BLOCKS_TOTAL] > 0) 100.0 * d[BLOCKS_OK] / d[BLOCKS_TOTAL] else 0.0
        val scanMs = d[SCAN_US] / 1000.0 / scanned
        val newKb = d[UNIQUE_BYTES] / 1024.0
        val avgKbs = cur[UNIQUE_BYTES] / 1024.0 / secs

        // 1 秒ぶんの値 (logcat は機械で読むので key=value)
        Log.i(
            TAG,
            ("t=%.0f cam=%d gapMaxUs=%d drop=%d scan=%d det=%d full=%d read=%.1f scanMs=%.1f " +
                "rotMs=%.1f decMs=%.1f newKB=%.1f newPk=%d newSeq=%d totKB=%.1f avgKBs=%.1f " +
                "tempC=%.1f thermal=%d").format(
                secs, dCam, gap, d[DROPPED], d[SCANNED], d[DETECTED], d[FULL], readPct, scanMs,
                d[ROTATE_US] / 1000.0 / det, d[DECODE_US] / 1000.0 / det, newKb, d[PACKETS], d[SEQS],
                cur[UNIQUE_BYTES] / 1024.0, avgKbs, tempC, thermal,
            ),
        )
        val grid = if (gridW > 0) "${gridW}x$gridH" else "自動"
        val exp = if (expUs > 0) "${expUs}us" else "自動"
        text.text = listOf(
            "${capW}x$capH @$capFps  走査 $workers 本  格子 $grid  露光 $exp",
            "カメラ  $dCam 枚/s  (最大間隔 ${gap / 1000} ms)",
            "走査    ${d[SCANNED]} 枚/s  捨て ${d[DROPPED]}  1 枚 %.1f ms".format(scanMs),
            "検出    ${d[DETECTED]} 枚/s  全読み ${d[FULL]}  読めた割合 %.0f%%".format(readPct),
            "新規    %.0f KB/s  (送信フレーム ${d[SEQS]} 種/s)".format(newKb),
            "平均    %.0f KB/s  (%.0f 秒)".format(avgKbs, secs),
            "温度    %.1f ℃  (thermal $thermal)".format(tempC),
        ).joinToString("\n")
    }

    override fun onDestroy() {
        closeCamera()
        camThread.quitSafely()
        readThread.quitSafely()
        super.onDestroy()
    }

    companion object {
        private const val TAG = "VloomProbe"

        // NativeProbe.stats() の並び (app/rust/src/native_probe.rs と揃える)。0 番は submitted
        private const val STAT_COUNT = 13
        private const val DROPPED = 1
        private const val SCANNED = 2
        private const val DETECTED = 3
        private const val BLOCKS_OK = 4
        private const val BLOCKS_TOTAL = 5
        private const val FULL = 6
        private const val ROTATE_US = 7
        private const val DECODE_US = 8
        private const val SCAN_US = 9
        private const val UNIQUE_BYTES = 10
        private const val PACKETS = 11
        private const val SEQS = 12
    }
}

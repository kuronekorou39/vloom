package app.vloom.vloom

import android.hardware.camera2.CaptureRequest
import androidx.camera.camera2.interop.Camera2CameraControl
import androidx.camera.camera2.interop.CaptureRequestOptions
import androidx.camera.camera2.interop.ExperimentalCamera2Interop
import androidx.camera.core.CameraControl
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import io.flutter.plugins.camerax.CameraAndroidCameraxPlugin
import io.flutter.plugins.camerax.ProxyApiRegistrar

/// 起動 Intent で「どのタブを開くか」「どのプリセットを使うか」を指定できるようにする。
///
/// 条件を振って自動計測するとき、画面のチップを座標決め打ちでタップするのは脆い。
/// プリセットが 1 つ増えただけで全部ずれるし、実際に外れて前の結果画面をそのまま
/// 撮ってしまい試行を無駄にしたことがある。Intent なら座標に依存せず、
/// 画面を辿る数秒も要らない。
///
///   adb shell am start -n app.vloom.vloom/.MainActivity --ei tab 1 --ei preset 4
///
/// tab: 0=送信 1=受信 2=履歴、preset: kPresets の添字、grid: "11x14" のような格子。
/// いずれも省略可。grid はプリセットにない格子を試すためのもので、preset より優先する。
class MainActivity : FlutterActivity() {
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, CHANNEL)
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    "args" -> result.success(
                        mapOf(
                            "tab" to intent.getIntExtra("tab", -1),
                            "preset" to intent.getIntExtra("preset", -1),
                            "grid" to intent.getStringExtra("grid"),
                            "camlock" to intent.getStringExtra("camlock"),
                            "ev" to intent.getStringExtra("ev"),
                            "aepoint" to intent.getStringExtra("aepoint"),
                            "dump" to intent.getIntExtra("dump", -1),
                            "exp" to intent.getIntExtra("exp", -1),
                            "iso" to intent.getIntExtra("iso", -1),
                        )
                    )
                    "setExposure" -> {
                        try {
                            setManualExposure(
                                flutterEngine,
                                (call.argument<Number>("control")!!).toLong(),
                                (call.argument<Number>("us")!!).toLong(),
                                (call.argument<Number>("iso")!!).toInt(),
                            )
                            result.success(true)
                        } catch (e: Throwable) {
                            result.error("exposure", e.toString(), null)
                        }
                    }
                    else -> result.notImplemented()
                }
            }
    }

    /// 露光時間とゲインを Camera2 で直接指定する (AE を切る)。
    ///
    /// Flutter の camera プラグインは EV 補正までしか出しておらず、Pixel 9a の AE は
    /// 「ゲイン最低・露光長め」を好んで 10ms 前後に張り付く。20fps の送信では露光中に
    /// 表示が切り替わって前後フレームが混ざる範囲がローリングシャッターの広い範囲に及ぶ。
    /// プラグインが内部に持つ CameraX の CameraControl を、Dart 側のインスタンス ID から
    /// 引き当てて Camera2Interop で上書きする。プラグインの非公開フィールドに触るので
    /// バージョンを上げたら要確認 (camera_android_camerax 0.7.1)。
    @OptIn(ExperimentalCamera2Interop::class)
    private fun setManualExposure(engine: FlutterEngine, controlId: Long, us: Long, iso: Int) {
        val plugin = engine.plugins.get(CameraAndroidCameraxPlugin::class.java)
            as? CameraAndroidCameraxPlugin ?: error("camerax plugin not found")
        val field = CameraAndroidCameraxPlugin::class.java.getDeclaredField("proxyApiRegistrar")
        field.isAccessible = true
        val registrar = field.get(plugin) as ProxyApiRegistrar
        val control = registrar.instanceManager.getInstance<Any>(controlId) as? CameraControl
            ?: error("CameraControl not found for id $controlId")
        val opts = CaptureRequestOptions.Builder()
            .setCaptureRequestOption(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_OFF)
            .setCaptureRequestOption(CaptureRequest.SENSOR_EXPOSURE_TIME, us * 1000L)
            .setCaptureRequestOption(CaptureRequest.SENSOR_SENSITIVITY, iso)
            .build()
        Camera2CameraControl.from(control).addCaptureRequestOptions(opts)
    }

    companion object {
        private const val CHANNEL = "app.vloom/launch"
    }
}

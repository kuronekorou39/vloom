package app.vloom.vloom

import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

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
                        )
                    )
                    else -> result.notImplemented()
                }
            }
    }

    companion object {
        private const val CHANNEL = "app.vloom/launch"
    }
}

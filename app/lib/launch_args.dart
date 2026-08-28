import 'dart:io' show Platform;

import 'package:flutter/services.dart';

/// 起動 Intent で渡された指定 (Android のみ)。条件を振って自動計測するためのもの。
///
/// 画面のチップを座標決め打ちでタップする方式は、項目が 1 つ増えただけで全部ずれる
/// (実際に外れて前の結果画面を撮り、試行を無駄にした)。Intent なら座標に依存せず、
/// 画面を辿る数秒も要らない。指定の受け口は MainActivity.kt にある。
///
///     adb shell am start -n app.vloom.vloom/.MainActivity --ei tab 1 --ei preset 4
///     adb shell am start -n app.vloom.vloom/.MainActivity --ei tab 1 --es grid 13x16
class LaunchArgs {
  const LaunchArgs({this.tab, this.preset, this.grid, this.camLock});

  /// 0=送信 1=受信 2=履歴。指定がなければ null
  final int? tab;

  /// kPresets の添字。指定がなければ null
  final int? preset;

  /// 格子の直接指定 ("11x14" 等)。プリセットに無い格子を測るためのもので、
  /// [preset] より優先する。指定がなければ null
  final String? grid;

  /// 追従が安定したときのカメラロック: "none" / "ae" / "both"。指定がなければ null
  /// (= 既定の ae)。ロック操作はフレーム供給を秒単位で止めることがあるので、
  /// 効果と代償を測るために切り替えられるようにしてある。
  final String? camLock;

  static const _ch = MethodChannel('app.vloom/launch');
  static LaunchArgs _cached = const LaunchArgs();

  /// 起動直後に読んだ値。main() の [load] 後は同期で参照できる
  /// (受信画面はカメラを作る前にプリセットを確定させたいので、非同期だと間に合わない)。
  static LaunchArgs get cached => _cached;

  /// main() から一度だけ呼ぶ。
  static Future<void> load() async {
    if (!Platform.isAndroid) return;
    try {
      final m = await _ch.invokeMapMethod<String, dynamic>('args');
      if (m == null) return;
      int? pick(String k) {
        final v = m[k];
        return (v is int && v >= 0) ? v : null;
      }

      final g = m['grid'];
      final cl = m['camlock'];
      _cached = LaunchArgs(
        tab: pick('tab'),
        preset: pick('preset'),
        grid: (g is String && g.isNotEmpty) ? g : null,
        camLock: (cl is String && cl.isNotEmpty) ? cl : null,
      );
    } catch (_) {
      // 指定なしで普通に起動する
    }
  }
}

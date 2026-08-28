import 'dart:async';
import 'dart:io' show Platform;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'src/rust/frb_generated.dart';
import 'history_store.dart';
import 'history_screen.dart';
import 'vcode_send_screen.dart';
import 'vcode_receive_screen.dart';
import 'calibration_screen.dart';
import 'launch_args.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  // 縦向き固定: vcode の受信はカメラ向きを前提に検出するため、端末回転で
  // 前提が崩れて読めなくなるのを防ぐ (スキャナ系アプリの定石)。
  await SystemChrome.setPreferredOrientations(
      [DeviceOrientation.portraitUp, DeviceOrientation.portraitDown]);
  // 計測の自動化用。カメラを作る前にプリセットを確定させたいので、
  // 画面が組み上がる前にここで読んでおく (以降は同期で参照できる)。
  await LaunchArgs.load();
  await RustLib.init();
  await HistoryStore.instance.init();
  _registerNativeLicenses();
  runApp(const VloomApp());
}

/// Rust 側の依存のライセンスを Flutter のライセンス画面に載せる。
/// pub パッケージの LICENSE は Flutter が自動収集するが、cargo の依存は拾われないため
/// 手で登録する (配布物に含む以上、表記が要る)。全文は THIRD-PARTY-NOTICES.md。
void _registerNativeLicenses() {
  LicenseRegistry.addLicense(() async* {
    yield const LicenseEntryWithLineBreaks(
      ['raptorq'],
      '''
Copyright Christopher Berner

Licensed under the Apache License, Version 2.0.
https://www.apache.org/licenses/LICENSE-2.0

RaptorQ (RFC 6330) の実装。Vloom の Fountain 符号化の中核。''',
    );
  });
}

class VloomApp extends StatelessWidget {
  const VloomApp({super.key});

  @override
  Widget build(BuildContext context) {
    final scheme = ColorScheme.fromSeed(
      seedColor: const Color(0xFF5B8CFF),
      brightness: Brightness.dark,
    );
    return MaterialApp(
      title: 'Vloom',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(colorScheme: scheme, useMaterial3: true),
      home: const HomeShell(),
    );
  }
}

class HomeShell extends StatefulWidget {
  const HomeShell({super.key});
  @override
  State<HomeShell> createState() => _HomeShellState();
}

class _HomeShellState extends State<HomeShell> {
  // 起動 Intent でタブを指定できる (計測の自動化用。既定は送信タブ)
  int _index = LaunchArgs.cached.tab ?? 0;
  // 校正画面 (カメラを使う) を上に重ねている間は、下の受信タブのカメラを止める。
  // IndexedStack は全子を生かし続けるため、明示的に active を切らないと
  // 背面カメラを複数の画面が奪い合ってフリーズする。
  bool _calOpen = false;

  /// カメラ受信はモバイルのみ。デスクトップ (Windows = PC 送信機として使用) では
  /// camera プラグインが動かないため差し替える (IndexedStack は全子を即ビルドする)。
  static final bool _hasCamera = Platform.isAndroid || Platform.isIOS;

  /// 受信系カメラは「そのタブが選択中」かつ「校正を開いていない」ときだけ動かす。
  List<Widget> _buildScreens() => <Widget>[
        const VcodeSendScreen(),
        _hasCamera
            ? VcodeReceiveScreen(active: _index == 1 && !_calOpen)
            : const Center(child: Text('この環境ではカメラ受信は使えません')),
        const HistoryScreen(),
      ];

  Future<void> _openCalibration() async {
    setState(() => _calOpen = true); // 先に受信タブのカメラを解放させる
    await Navigator.of(context).push(
      MaterialPageRoute(builder: (_) => const CalibrationScreen()),
    );
    // 校正カメラの解放 (非同期 dispose) が終わるのを少し待ってから受信を再開する。
    // 早すぎると同じ背面カメラを一瞬奪い合い、受信プレビューが灰色になる。
    await Future.delayed(const Duration(milliseconds: 500));
    if (mounted) setState(() => _calOpen = false); // 戻ったら受信カメラを再開
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Vloom'),
        actions: [
          if (_hasCamera)
            IconButton(
              icon: const Icon(Icons.tune),
              tooltip: '校正 (読み取り限界の確認)',
              onPressed: _openCalibration,
            ),
          IconButton(
            icon: const Icon(Icons.info_outline),
            tooltip: 'ライセンス',
            onPressed: () => showLicensePage(
              context: context,
              applicationName: 'Vloom',
              applicationLegalese: '© 2026 rou — MIT License',
            ),
          ),
        ],
      ),
      body: IndexedStack(index: _index, children: _buildScreens()),
      bottomNavigationBar: NavigationBar(
        selectedIndex: _index,
        onDestinationSelected: (i) => setState(() => _index = i),
        destinations: const [
          NavigationDestination(icon: Icon(Icons.grid_on), label: '送信'),
          NavigationDestination(icon: Icon(Icons.center_focus_strong), label: '受信'),
          NavigationDestination(icon: Icon(Icons.history), label: '履歴'),
        ],
      ),
    );
  }
}

import 'dart:typed_data';

/// 「さがす」モードの集計。
///
/// 送信側は格子と fps の組を数秒ずつ切り替えながら試験フレームを流し続ける
/// (Rust の vcode::probe)。受信側は読めたブロックを設定ごとに数え、実効スループットを
/// 出し続ける。ファイルは運ばないので Fountain code は通さない。
///
/// 速さは受信側の時計ではなく、ブロックが持っているフレーム番号から出す:
///
///     重複なしで読めたブロック数 × 1 ブロックのバイト数 ÷ (写っていたフレーム数 ÷ fps)
///
/// 格子が切り替わった直後は掴み直しに数枚かかるが、その間のフレームは「写っていた」
/// 範囲に入らないので、切り替えの手間は数字に乗らない (本番の転送には切り替えが無い)。
/// 範囲の内側で読めなかったフレームやブロックは、そのまま損失として効く。

/// 試験フレームの印 (ヘッダの OTI 欄の先頭)。Rust の PROBE_MAGIC と揃える
const _kProbeMagic = [0x56, 0x50, 0x52, 0x42]; // "VPRB"

/// 1 ブロックが本番で運ぶバイト数 (ペイロード 46 から RaptorQ の ID 4 を引いたもの)
const kProbeBlockBytes = 42;

/// これだけフレーム番号が飛んだら、同じ設定の次の巡回に入ったと見なす (秒)
const _kVisitGapSec = 1.0;

/// 振れ幅と順位に数える巡回の最短の長さ (秒)。受信を始めた瞬間や掴み直しの直後は
/// 巡回の端だけが写り、数枚ぶんの値は当てにならない
const _kMinVisitSec = 1.5;

/// ヘッダの OTI 欄から読む、いま流れている設定の情報。
class ProbeTag {
  const ProbeTag(this.setting, this.settings, this.fps, this.dwellSec);
  final int setting;
  final int settings;
  final int fps;
  final double dwellSec;

  /// 試験フレームでなければ null
  static ProbeTag? parse(Uint8List oti) {
    if (oti.length < 12) return null;
    for (var i = 0; i < 4; i++) {
      if (oti[i] != _kProbeMagic[i]) return null;
    }
    return ProbeTag(oti[4], oti[8], oti[9], oti[10] / 10);
  }
}

/// 設定 1 つぶんの集計。
class ProbeSetting {
  ProbeSetting(this.index, this.grid, this.fps);
  final int index;
  final String grid;
  final int fps;

  // 進行中の巡回
  final Set<int> _seen = {};
  int _minFrame = 0;
  int _maxFrame = -1;

  // 終わった巡回の合計
  int _doneBlocks = 0;
  int _doneFrames = 0;

  /// 巡回ごとの実効スループット (KB/s)。振れ幅を出すために全部持っておく
  final List<double> _visitKbps = [];

  /// 巡回ごとの値の「悪いほう」と「良いほう」(KB/s)。まだ無ければ null。
  ///
  /// 送信 fps がカメラの fps に近い設定 (30fps など) は、切り替わりと撮影の
  /// タイミングの噛み合わせ次第で巡回ごとに大きく振れる (実機: 13x18 / 30fps が
  /// 110〜219)。平均だけ見ると、たまたま当たった巡回で一番に見えてしまう。
  /// 最小・最大そのものではなく下から / 上から 2 割目を取る。長く流すと 1 回だけ
  /// 外れた巡回が最小を決めてしまい、どの設定も悪く見えるため
  double? get minKbps => _quantile(0.2);
  double? get maxKbps => _quantile(0.8);

  double? _quantile(double q) {
    if (_visitKbps.isEmpty) return null;
    final v = [..._visitKbps]..sort();
    return v[((v.length - 1) * q).round()];
  }

  /// 振れ幅に数えた巡回の数
  int get visits => _visitKbps.length;

  int get blocksPerFrame {
    final p = grid.split('x');
    return int.parse(p[0]) * int.parse(p[1]);
  }

  /// 取りこぼしが無いときの値 (KB/s)
  double get theoreticalKbps => blocksPerFrame * kProbeBlockBytes * fps / 1024;

  int get _curFrames => _maxFrame < _minFrame ? 0 : _maxFrame - _minFrame + 1;

  /// これまでの全巡回 (進行中を含む) を通した実効スループット (KB/s)
  double get kbps {
    final frames = _doneFrames + _curFrames;
    if (frames == 0) return 0;
    return (_doneBlocks + _seen.length) * kProbeBlockBytes * fps / frames / 1024;
  }

  /// 写っていたフレームのうち、読めたブロックの割合 (0〜1)
  double get recovery => theoreticalKbps == 0 ? 0 : kbps / theoreticalKbps;

  void _closeVisit() {
    final frames = _curFrames;
    if (frames > 0) {
      _doneBlocks += _seen.length;
      _doneFrames += frames;
      if (frames >= fps * _kMinVisitSec) {
        _visitKbps.add(_seen.length * kProbeBlockBytes * fps / frames / 1024);
      }
    }
    _seen.clear();
    _minFrame = 0;
    _maxFrame = -1;
  }

  void _add(int frame, int block) {
    if (_maxFrame >= _minFrame &&
        (frame > _maxFrame + fps * _kVisitGapSec || frame < _minFrame - fps)) {
      _closeVisit();
    }
    if (_maxFrame < _minFrame) {
      _minFrame = frame;
      _maxFrame = frame;
    } else {
      if (frame < _minFrame) _minFrame = frame;
      if (frame > _maxFrame) _maxFrame = frame;
    }
    _seen.add(frame * 65536 + block);
  }
}

class ProbeStats {
  final Map<int, ProbeSetting> _settings = {};

  /// いま写っている設定 (最後に読めたブロックのもの)
  ProbeSetting? current;

  /// 送信側が流している設定の数と、1 設定の長さ (ヘッダから)
  int settingCount = 0;
  double dwellSec = 0;

  /// 設定番号の順
  List<ProbeSetting> get settings =>
      _settings.values.toList()..sort((a, b) => a.index.compareTo(b.index));

  /// 一巡以上測れた設定の中で、いちばん当てにできるもの。まだ無ければ null。
  /// 平均ではなく「悪かった巡回の値」で比べる。本番の転送は噛み合わせを選べないので、
  /// 当たれば速い設定より、外れても速い設定のほうが実際に速い
  ProbeSetting? get best {
    ProbeSetting? b;
    for (final s in _settings.values) {
      if (s.visits == 0) continue;
      if (b == null || s.minKbps! > b.minKbps!) b = s;
    }
    return b;
  }

  /// 検出できた 1 枚ぶんを足す。[packets] は CRC が通ったブロックのペイロード。
  /// [gridW] / [gridH] はこの枚のヘッダの格子 (ブロック自身は格子を持たないため)。
  void add(ProbeTag tag, List<Uint8List> packets, int gridW, int gridH) {
    settingCount = tag.settings;
    dwellSec = tag.dwellSec;
    for (final p in packets) {
      if (p.length < 8) continue;
      final frame = p[0] | (p[1] << 8) | (p[2] << 16) | (p[3] << 24);
      final block = p[4] | (p[5] << 8);
      final setting = p[6];
      final fps = p[7];
      // 切り替わりをまたいだ 1 枚には前の設定のブロックも写りうる。格子が違うなら
      // ヘッダの格子は当てにならないので、ヘッダと同じ設定のブロックだけ数える
      if (setting != tag.setting || fps == 0) continue;
      final s = _settings.putIfAbsent(
        setting,
        () => ProbeSetting(setting, '${gridW}x$gridH', fps),
      );
      s._add(frame, block);
      if (!identical(current, s)) {
        // 別の設定に移った。前の設定の巡回はここで締める
        current?._closeVisit();
        current = s;
      }
    }
  }

  /// logcat 用の 1 行 (計測スクリプトが拾う)
  String summary() => settings
      .map((s) => '${s.grid}@${s.fps}=${s.kbps.toStringAsFixed(1)}'
          '[${s.minKbps?.toStringAsFixed(0) ?? "-"}-${s.maxKbps?.toStringAsFixed(0) ?? "-"}]'
          '(${(s.recovery * 100).toStringAsFixed(0)}%,${s.visits})')
      .join(' ');
}

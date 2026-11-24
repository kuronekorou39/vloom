// --- ピント (AF) の探り直しを見つけて、合っている瞬間に固定する ---
//
// 合成計測 (track_motion) では、手振れそのものより**ピント**が効く。13x18
// (3.4px/セル) で中心のボケ半径が 0.35 → 0.80px になるだけで回収ブロックが
// 8 割落ちた (露光を 2 倍にしたときの 4 割減より桁で大きい)。据え置きなら AF は
// 一度合えば動かないが、手持ちは距離が変わり続けて連続 AF が探り直す。
// 実機の「三脚 138 / 手持ち 60 KB/s」はこれで説明が付く。
//
// ただし AF ロックは無条件にやると損をする。三脚では既に合っているので、
// モード切替の停止 (0.5〜5 秒) だけが乗る (実測 both 49 / ae 76 KB/s)。
// そこで「探り直しが起きている」証拠を掴んでからにする: 満点率が高い状態から
// 低い状態へ落ちて戻る、を 2 度観測したら AF が探っていると見なし、
// 次に高い状態になった瞬間 (= 今ピントが合っている) に固定する。
// 据え置きでは満点率が落ちないのでこの経路には入らない。

/// 「ピントが合っている」と見なす満点ブロック率 (回収 / 全体)
const kFocusGoodRatio = 0.55;

/// 「ボケている」と見なす満点ブロック率
const kFocusBadRatio = 0.25;

/// 探り直しと判断するのに必要な、good → bad → good の往復回数
const kFocusHuntCycles = 2;

/// good / bad をこの枚数だけ「連続して」満たしたときだけ状態が変わったと見なす。
///
/// 回収率は 1〜2 枚おきに大きく上下する (ローリングシャッターで送信フレームの
/// 切り替わりをまたいだ枚は帯を失う。実機ログで 125/234 と 75/234 が交互に出た)。
/// 単発の上下を「探り直し」と数えると、実機では受信開始 240ms で 2 往復に達して
/// 誤爆した。AF の探り直しはボケが 0.5〜2 秒続くので、連続枚数で区別できる。
const kFocusStateFrames = 6;

/// 検出がこの枚数に達するまでは数え始めない。開始直後は AE/AF が動いている途中で、
/// 位置合わせも粗いため回収率が低く出る (それを探り直しと取り違えない)。
const kFocusSettleFrames = 30;

/// AF を固定した後、これだけ連続でボケていたら固定を解除して AF に戻す
/// (距離が変わって固定したピントが外れた場合)
const kFocusRelockBadFrames = 12;

/// 1 回の受信で AF 固定を試みる上限。切替のたびにフレームが止まるので粘らない
const kFocusLockAttempts = 3;

/// [FocusHunter.update] が返す、カメラに対して取るべき操作。
enum FocusAction { lock, unlock }

/// 満点ブロック率の推移から AF の探り直しを見つけ、固定・解除の判断だけを返す。
/// カメラは触らない (呼び出し側が [FocusAction] に従って FocusMode を切り替える)。
class FocusHunter {
  bool locked = false; // AF を固定済みか
  int hunts = 0; // good -> bad -> good の往復を観測した回数
  int attempts = 0; // AF 固定を試みた回数
  bool _wasGood = false; // 直近が「合っている」側だったか
  int _goodRun = 0; // 連続して「合っている」側だったフレーム数
  int _badRun = 0; // 連続して「ボケている」側だったフレーム数

  /// 検出フレーム 1 枚ぶんを判定する。[framesDetected] はこれまでの検出枚数 (今回を含む)。
  FocusAction? update(int ok, int total, {required int framesDetected}) {
    if (total <= 0) return null;
    final ratio = ok / total;
    if (locked) {
      // 固定したピントが外れた (距離が変わった) なら AF に戻す
      _badRun = ratio < kFocusBadRatio ? _badRun + 1 : 0;
      if (_badRun >= kFocusRelockBadFrames) {
        _badRun = 0;
        locked = false;
        _wasGood = false;
        return FocusAction.unlock;
      }
      return null;
    }
    // 開始直後は AE/AF が動いている途中なので数えない
    if (framesDetected < kFocusSettleFrames) return null;
    // 連続して同じ側にいる枚数を数える。中間の値はどちらの走も進めず、切りもしない
    // (1〜2 枚おきの上下で走が途切れると、続いているボケを見つけられない)
    if (ratio >= kFocusGoodRatio) {
      _goodRun++;
      _badRun = 0;
    } else if (ratio < kFocusBadRatio) {
      _badRun++;
      _goodRun = 0;
    }
    if (_wasGood && _badRun >= kFocusStateFrames) {
      // 合っていたのが続けてボケた = 探り直しが始まった
      _wasGood = false;
      hunts++;
      return null;
    }
    if (_goodRun < kFocusStateFrames) return null;
    _wasGood = true;
    if (hunts >= kFocusHuntCycles && attempts < kFocusLockAttempts) {
      // 探り直しを見たうえで、今まさに合っている。ここで固定すればボケを避けられる
      attempts++;
      locked = true;
      _badRun = 0;
      return FocusAction.lock;
    }
    return null;
  }

  /// 統計表の 1 行 (三脚では 0 回のままになるはず)
  String summary() => attempts > 0
      ? '$hunts 回検出 → $attempts 回固定${locked ? " (固定中)" : " (解除済み)"}'
      : '$hunts 回検出 (固定せず)';
}

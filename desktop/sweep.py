"""「さがす」モード: 格子と fps の組を数秒ずつ切り替えながら試験フレームを流す。

受信側は読めたブロックを設定ごとに数え、実効スループットを出し続ける
(アプリの受信画面は試験フレームだと自分で気付くので、切り替えの操作は要らない)。
カメラを据えたまま、その環境で一番速い設定を探すためのもの。ファイルは運ばない。

フレームの中身は Rust コア (vcode::probe) が作る。ここは「いまどの設定の何枚目か」を
決めるだけで、送信ウィンドウからは VcodeTx と同じ形で呼べるようにしてある。
"""

from __future__ import annotations

import re
from dataclasses import dataclass

import vloom_core

# 既定で回す格子。「引いても読める」ものから「据え置きで最速を狙う」ものまで
DEFAULT_GRIDS = ("11x14", "13x18", "15x20")
# 既定で試す fps の範囲と数。下は遅すぎて候補にならず、上はカメラ (毎秒 30 枚前後) が
# 1 枚ずつ拾えない (実機: 60Hz の画面で 60fps は 20fps より遅い)。
# 30fps はカメラの fps と近く、噛み合わせ次第で巡回ごとに大きく振れるが、当たれば
# 最速なので候補には入れる (受信側が振れ幅を見て判断する)
FPS_RANGE = (15, 40)
FPS_CHOICES = 3
# 画面の書き換え周期が取れないときに仮定する値
FALLBACK_REFRESH_HZ = 60.0
# 1 つの設定を流し続ける長さ (秒)。短いと切り替え直後の掴み直しが占める割合が増え、
# 長いと一巡が遅くなる。4 秒なら 20fps で 80 枚ぶん測れる
DEFAULT_DWELL_SEC = 4.0
# 一巡に入れられる設定の数 (ヘッダの欄が 1 バイト)
MAX_SETTINGS = 255
# 送信ウィンドウの「何枚で一巡か」に渡す値。番号は通しで増やし続けたいので、
# 現実には届かない大きさにしておく
ENDLESS_FRAMES = 2**31


@dataclass(frozen=True)
class Setting:
    grid_w: int
    grid_h: int
    fps: int

    @property
    def label(self) -> str:
        return f"{self.grid_w}x{self.grid_h} @{self.fps}fps"


def fps_candidates(refresh_hz: float) -> list[int]:
    """その画面で等間隔に出せる fps を、速い順に FPS_CHOICES 個まで返す。

    1 フレームを書き換え n 回ぶん表示するので、出せるのは 書き換え周期 ÷ n だけ。
    割り切れない fps を要求すると表示時間が 1 枚ごとに不揃いになり、受信側で
    混ざる枚が増える (実機: 60Hz の画面で 40fps は 30fps の半分の速さだった)。
    """
    hz = round(refresh_hz) if refresh_hz and refresh_hz > 1 else round(FALLBACK_REFRESH_HZ)
    lo, hi = FPS_RANGE
    out = [hz // n for n in range(1, hz + 1) if hz % n == 0 and lo <= hz // n <= hi]
    return out[:FPS_CHOICES] or [min(hi, max(lo, hz // 3 or lo))]


def default_sweep(refresh_hz: float) -> str:
    """既定の組。fps ごとに格子を一巡させる (遅い fps から)。"""
    return ",".join(f"{g}@{fps}" for fps in sorted(fps_candidates(refresh_hz))
                    for g in DEFAULT_GRIDS)


def parse_sweep(text: str, side_range: tuple[int, int]) -> list[Setting]:
    """"13x18@20,15x20@30" の形を設定の並びにする。"""
    lo, hi = side_range
    out: list[Setting] = []
    for part in text.split(","):
        m = re.fullmatch(r"\s*(\d{1,2})x(\d{1,2})@(\d{1,3})\s*", part)
        if not m:
            raise ValueError(f"設定の形式が不正: {part!r} (例: 13x18@20)")
        gw, gh, fps = (int(v) for v in m.groups())
        if not (lo <= gw <= hi and lo <= gh <= hi):
            raise ValueError(f"格子 {gw}x{gh} は範囲外です (縦横とも {lo}〜{hi})")
        if not 1 <= fps <= 240:
            raise ValueError(f"fps {fps} は範囲外です")
        out.append(Setting(gw, gh, fps))
    if not 1 <= len(out) <= MAX_SETTINGS:
        raise ValueError(f"設定は 1〜{MAX_SETTINGS} 個")
    return out


class SweepTx:
    """送信ウィンドウに VcodeTx の代わりに渡す。frame_gray(i) の i は通し番号。"""

    def __init__(self, settings: list[Setting], dwell_sec: float) -> None:
        self.settings = settings
        self.dwell_sec = dwell_sec
        # ヘッダには 0.1 秒単位の 1 バイトで載せる
        self._dwell_ds = max(1, min(255, round(dwell_sec * 10)))
        # 設定ごとの枚数と、一巡の中での開始位置
        self._counts = [max(1, round(dwell_sec * s.fps)) for s in settings]
        self._starts = [sum(self._counts[:k]) for k in range(len(settings))]
        self._cycle = sum(self._counts)
        self.frame_count = ENDLESS_FRAMES
        self.packet_count = 0
        self.current = 0
        self.frame_width, self.frame_height = self._cells(settings[0])

    @staticmethod
    def _cells(s: Setting) -> tuple[int, int]:
        w, h = vloom_core.layout_cells(s.grid_w, s.grid_h)
        return w, h

    def _setting_at(self, i: int) -> int:
        pos = i % self._cycle
        k = len(self._starts) - 1
        while self._starts[k] > pos:
            k -= 1
        return k

    @property
    def frame_interval(self) -> float:
        """直近に描いたフレームを表示し続ける長さ (秒)。設定ごとに fps が違う。"""
        return 1.0 / self.settings[self.current].fps

    @property
    def status(self) -> str:
        s = self.settings[self.current]
        return (f"さがす: {self.current + 1}/{len(self.settings)}  {s.label}  "
                f"(1 設定 {self.dwell_sec:g} 秒 · 一巡 {len(self.settings) * self.dwell_sec:g} 秒)")

    def frame_gray(self, i: int) -> bytes:
        k = self._setting_at(i)
        s = self.settings[k]
        self.current = k
        self.frame_width, self.frame_height = self._cells(s)
        return vloom_core.probe_frame_gray(
            s.grid_w, s.grid_h, k, len(self.settings), s.fps, self._dwell_ds, i)

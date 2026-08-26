"""コードを表示する送信ウィンドウ。

ブラウザ版と違い、フレームの表示間隔を自前で制御できるのがネイティブ版の利点。
ディスプレイのリフレッシュに対して表示が短すぎると受信側が中間状態を撮ってしまうため、
遅延を蓄積させないデッドライン方式で刻む (setInterval のようなドリフトが出ない)。
"""

from __future__ import annotations

import ctypes
import time

from PySide6.QtCore import QRect, Qt, QTimer, Signal
from PySide6.QtGui import QColor, QImage, QKeySequence, QPainter, QShortcut
from PySide6.QtWidgets import (
    QCheckBox,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QSlider,
    QVBoxLayout,
    QWidget,
)

# 送信中はディスプレイを消灯させない (Windows のみ。他 OS では何もしない)
_ES_CONTINUOUS = 0x80000000
_ES_DISPLAY_REQUIRED = 0x00000002


def _keep_display_awake(on: bool) -> None:
    try:
        flags = _ES_CONTINUOUS | (_ES_DISPLAY_REQUIRED if on else 0)
        ctypes.windll.kernel32.SetThreadExecutionState(flags)
    except (AttributeError, OSError):
        pass  # Windows 以外、または API が無い環境


class FrameView(QWidget):
    """コードのフレームだけを描く面。白背景にアスペクト維持でフィットさせる。"""

    def __init__(self) -> None:
        super().__init__()
        self._image: QImage | None = None
        self._buffer: bytes | None = None  # QImage は参照を持たないので手元で保持する
        self._smooth = False
        self.setAutoFillBackground(False)

    def set_smooth(self, on: bool) -> None:
        self._smooth = on
        self.update()

    def set_frame(self, data: bytes, w: int, h: int, table: list[int]) -> None:
        self._buffer = data
        img = QImage(data, w, h, w, QImage.Format.Format_Indexed8)
        # 輝度調整はカラーテーブルで行う。画素を触らないのでフレームごとのコストがゼロ。
        img.setColorTable(table)
        self._image = img
        self.update()

    def paintEvent(self, event) -> None:  # noqa: N802 (Qt の命名)
        p = QPainter(self)
        p.fillRect(self.rect(), QColor(0, 0, 0))
        if self._image is None:
            return
        # 既定は最近傍。セル境界をぼかさないほうが受信側の量子化に有利。
        p.setRenderHint(QPainter.RenderHint.SmoothPixmapTransform, self._smooth)
        iw, ih = self._image.width(), self._image.height()
        scale = min(self.width() / iw, self.height() / ih)
        dw, dh = int(iw * scale), int(ih * scale)
        target = QRect((self.width() - dw) // 2, (self.height() - dh) // 2, dw, dh)
        p.drawImage(target, self._image)


class SenderWindow(QWidget):
    """送信ウィンドウ。下部に輝度・スムージング・状態のバーを出す。

    通常ウィンドウなので、受信側を構えながらサイズや位置を動かせる。コードは
    アスペクト比を保って中央にフィットするので、窓を広げるほどセルが大きくなる。
    """

    closed = Signal()

    def __init__(self, tx, fps: int, payload_len: int, label: str) -> None:
        super().__init__()
        self.tx = tx
        self.payload_len = payload_len
        self.frame_count = tx.frame_count
        self.interval = 1.0 / max(1, fps)
        self.fps = fps

        self.setWindowTitle("Vloom 送信")
        self.view = FrameView()
        self.status = QLabel("")
        self.status.setStyleSheet("color:#e6e9ef; font-size:12px;")

        self.bright = QSlider(Qt.Orientation.Horizontal)
        self.bright.setRange(30, 100)
        self.bright.setValue(100)
        self.bright.setFixedWidth(140)
        self.bright.valueChanged.connect(self._rebuild_table)

        self.smooth = QCheckBox("スムージング")
        self.smooth.setStyleSheet("color:#e6e9ef;")
        self.smooth.toggled.connect(self.view.set_smooth)

        stop = QPushButton("停止 (Esc)")
        stop.clicked.connect(self.close)

        bar = QHBoxLayout()
        bar.setContentsMargins(12, 6, 12, 6)
        for w in (QLabel("輝度"), self.bright, self.smooth):
            if isinstance(w, QLabel):
                w.setStyleSheet("color:#e6e9ef; font-size:12px;")
            bar.addWidget(w)
        bar.addWidget(self.status, 1)
        bar.addWidget(stop)

        bar_widget = QWidget()
        bar_widget.setLayout(bar)
        bar_widget.setStyleSheet("background: rgba(20,22,28,235);")

        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        root.addWidget(self.view, 1)
        root.addWidget(bar_widget)

        QShortcut(QKeySequence(Qt.Key.Key_Escape), self, self.close)

        self.label = label
        self.index = 0
        self.pass_no = 1
        self.started = time.perf_counter()
        self._deadline = self.started
        self._recent: list[float] = []  # 実測 fps 用の直近の描画時刻
        # _rebuild_table() は 1 枚目を描くので、index 等を決めた後に呼ぶ
        self._table: list[int] = []
        self._rebuild_table()

        # PreciseTimer: 既定の CoarseTimer は Windows で数十 ms ずれることがあり、
        # 1 フレームの表示時間がリフレッシュ周期を割ってしまう。
        self.timer = QTimer(self)
        self.timer.setTimerType(Qt.TimerType.PreciseTimer)
        self.timer.setSingleShot(True)
        self.timer.timeout.connect(self._tick)

    def start(self) -> None:
        _keep_display_awake(True)
        self.show()
        self.raise_()
        self.activateWindow()
        self._deadline = time.perf_counter()
        self._tick()

    def _rebuild_table(self) -> None:
        # 輝度 = 白レベルを下げる。受信側の白飛び (露出オーバー) を抑えるための調整。
        b = self.bright.value() / 100.0
        self._table = [0xFF000000 | (int(v * b) * 0x010101) for v in range(256)]
        self._draw(self.index)

    def _frame_arg(self, slot: int) -> int:
        """スロット番号 → VcodeTx に渡すフレーム番号。

        パケットは frame_gray(i) の中で (i * ブロック数 + j) mod パケット総数 に
        割り当てられる。i をそのまま 0..frame_count-1 で回すと、毎巡まったく同じ
        パケットが同じスロットに並ぶ。送信 fps とカメラの取り込み位相がビートして
        特定スロットを構造的に取りこぼすと、そのパケットは何巡しても入らない。

        巡ごとに 1 つ余分に進めることで、同じスロットが毎巡ちがうパケット群を運ぶ。
        受信側は「ヘッダとブロックが読めれば何でもよい」ので、送信側だけの変更で
        互換性は保たれる。
        """
        return slot + self.pass_no - 1

    def _draw(self, slot: int) -> None:
        gray = self.tx.frame_gray(self._frame_arg(slot))
        self.view.set_frame(gray, self.tx.frame_width, self.tx.frame_height, self._table)

    def _tick(self) -> None:
        now = time.perf_counter()
        self._draw(self.index)
        # 実測 fps。要求値どおり出ているかは計測の前提になるので必ず出す
        # (コンポジタは垂直同期で刻むので、リフレッシュの整数分の 1 でない
        #  fps を要求するとフレームの表示時間が不揃いになる)。
        self._recent.append(now)
        if len(self._recent) > 40:
            del self._recent[:-40]
        span = self._recent[-1] - self._recent[0]
        measured = (len(self._recent) - 1) / span if span > 0 else 0.0

        elapsed = now - self.started
        loop_sec = self.frame_count / self.fps
        self.status.setText(
            f"{self.label} · {self.payload_len:,} B · "
            f"{self.fps}fps 要求 / 実測 {measured:.1f}fps · "
            f"frame {self.index + 1}/{self.frame_count} · {self.pass_no} 巡目 · "
            f"{elapsed:.0f} 秒経過 (1 巡 {loop_sec:.1f} 秒)"
        )
        self.index += 1
        if self.index >= self.frame_count:
            self.index = 0
            self.pass_no += 1

        # デッドラインを積み上げてドリフトを消す。描画が遅れて過去になった場合だけ
        # 基準を取り直す (取り戻そうとして連続発火し、表示が 1 リフレッシュを割るのを防ぐ)。
        self._deadline += self.interval
        delay = self._deadline - time.perf_counter()
        if delay < 0:
            self._deadline = time.perf_counter() + self.interval
            delay = self.interval
        self.timer.start(int(delay * 1000))

    def closeEvent(self, event) -> None:  # noqa: N802 (Qt の命名)
        self.timer.stop()
        _keep_display_awake(False)
        self.closed.emit()
        super().closeEvent(event)

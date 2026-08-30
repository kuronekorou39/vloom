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
    QSizePolicy,
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

    def __init__(self, margin_cells: int = 0, zoom: float = 1.0,
                 dx: float = 0.0, dy: float = 0.0) -> None:
        super().__init__()
        self._image: QImage | None = None
        self._buffer: bytes | None = None  # QImage は参照を持たないので手元で保持する
        self._smooth = False
        # 面の中でのコードの大きさ (フィット時を 1.0 とする倍率) と、中心からの
        # ずれ (ウィンドウの幅・高さに対する比)。窓ごと動かすのと違い、白い面は
        # 動かないままコードだけが動くので、カメラから見て周囲の環境が変わらない。
        self._zoom = zoom
        self._dx = dx
        self._dy = dy
        # 下端に取っておく余白 (px)。操作バーを重ねる領域で、コードが隠れないように
        # 描画領域から常に除外する (バーの表示/非表示でコードの大きさは変えない)
        self._bottom_inset = 0
        # コードの四辺に確保する白の余白 (セル数)。QR の quiet zone に相当する。
        # コードが窓いっぱいだと外縁がウィンドウ枠や背景と隣接し、コーナー
        # マーカーの外周 (黒) がどこで終わるか読み取りにくくなる。
        self._margin = max(0, margin_cells)
        self.setAutoFillBackground(False)

    def set_smooth(self, on: bool) -> None:
        self._smooth = on
        self.update()

    def set_placement(self, zoom: float | None = None,
                      dx: float | None = None, dy: float | None = None) -> None:
        """コードの大きさ・位置を変える (指定したものだけ)。値は範囲内に丸める。"""
        if zoom is not None:
            self._zoom = min(1.0, max(0.05, zoom))
        if dx is not None:
            self._dx = min(0.5, max(-0.5, dx))
        if dy is not None:
            self._dy = min(0.5, max(-0.5, dy))
        self.update()

    @property
    def placement(self) -> tuple[float, float, float]:
        return self._zoom, self._dx, self._dy

    def set_bottom_inset(self, px: int) -> None:
        self._bottom_inset = max(0, px)
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
        # 背景は白。コーナーマーカーは外周 1 セルが黒なので、黒背景だとコードの
        # 外縁が背景に溶けて境界が消える。ガイド枠起点の通常スキャンは耐えるが、
        # 位置の自動検出 (scan_frame_wide) は黒背景だと全 sweep 失敗することを
        # 合成画像で確認した。PWA・スマホアプリの送信も白で塗っている。
        p.fillRect(self.rect(), QColor(255, 255, 255))
        if self._image is None:
            return
        # 既定は最近傍。セル境界をぼかさないほうが受信側の量子化に有利。
        p.setRenderHint(QPainter.RenderHint.SmoothPixmapTransform, self._smooth)
        iw, ih = self._image.width(), self._image.height()
        # 余白ぶんセル数が増えたものとして縮尺を決めると、四辺に正確に
        # margin セル分の白が残る (縮尺に関わらず「何セル分」で効く)
        m = self._margin
        avail_h = max(1, self.height() - self._bottom_inset)
        fit = min(self.width() / (iw + 2 * m), avail_h / (ih + 2 * m))
        dw, dh = int(iw * fit * self._zoom), int(ih * fit * self._zoom)
        cx = self.width() * (0.5 + self._dx)
        cy = avail_h * (0.5 + self._dy)
        target = QRect(int(cx - dw / 2), int(cy - dh / 2), dw, dh)
        p.drawImage(target, self._image)


class SenderWindow(QWidget):
    """送信ウィンドウ。下部に輝度・スムージング・状態のバーを出す。

    通常ウィンドウなので、受信側を構えながらサイズや位置を動かせる。窓を広げる
    ほどセルが大きくなるほか、矢印キーと +/- で「窓の中での」コードの位置と
    大きさも変えられる (三脚の構図を崩さずに追い込むため)。
    """

    closed = Signal()

    def __init__(self, tx, fps: int, payload_len: int, label: str,
                 margin_cells: int = 0, zoom: float = 1.0,
                 dx: float = 0.0, dy: float = 0.0, hold: bool = False) -> None:
        super().__init__()
        self.tx = tx
        # 静止 (調整用): フレームを進めず 1 枚を出し続ける。構図合わせの間に
        # 受信が完走して画面が変わったり、切り替わりでちらついたりしないように。
        self.hold = hold
        self.payload_len = payload_len
        self.frame_count = tx.frame_count
        self.interval = 1.0 / max(1, fps)
        self.fps = fps

        self.setWindowTitle("Vloom 送信")
        self.view = FrameView(margin_cells, zoom, dx, dy)
        self.status = QLabel("")
        self.status.setStyleSheet("color:#e6e9ef; font-size:12px;")
        # 状態表示の長さでウィンドウの最小幅が決まってしまい、--geometry で指定した
        # 幅より広がることがあった (780 を指定して 1293 になった)。縦長ウィンドウを
        # 作れないと縦長のコードを大きく出せないので、この文字列は幅を要求しない。
        self.status.setSizePolicy(QSizePolicy.Policy.Ignored, QSizePolicy.Policy.Preferred)

        self.bright = QSlider(Qt.Orientation.Horizontal)
        self.bright.setRange(30, 100)
        self.bright.setValue(100)
        self.bright.setFixedWidth(140)
        self.bright.valueChanged.connect(self._rebuild_table)

        self.smooth = QCheckBox("スムージング")
        self.smooth.setStyleSheet("color:#e6e9ef;")
        self.smooth.toggled.connect(self.view.set_smooth)

        self.hold_box = QCheckBox("静止 (調整用)")
        self.hold_box.setStyleSheet("color:#e6e9ef;")
        self.hold_box.setChecked(hold)
        self.hold_box.toggled.connect(self._set_hold)

        stop = QPushButton("停止 (Esc)")
        stop.clicked.connect(self.close)

        bar = QHBoxLayout()
        bar.setContentsMargins(12, 6, 12, 6)
        for w in (QLabel("輝度"), self.bright, self.smooth, self.hold_box):
            if isinstance(w, QLabel):
                w.setStyleSheet("color:#e6e9ef; font-size:12px;")
            bar.addWidget(w)
        bar.addWidget(self.status, 1)
        bar.addWidget(stop)

        self.bar_widget = QWidget()
        self.bar_widget.setLayout(bar)
        self.bar_widget.setStyleSheet("background: rgba(20,22,28,235);")

        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)
        root.addWidget(self.view, 1)
        # 操作バーはレイアウトに入れず、映像の上に重ねる。レイアウトに入れると
        # 表示/非表示でビューの高さが変わり、コードの大きさが数 px 変わってしまう。
        # カメラの構図はコードの大きさに合わせてあるので、バーの出入りで動いてはいけない。
        self.bar_widget.setParent(self)
        self.bar_widget.raise_()
        self.view.set_bottom_inset(self.bar_widget.sizeHint().height())

        QShortcut(QKeySequence(Qt.Key.Key_Escape), self, self.close)

        # 操作バーは濃色なので、コードのすぐ下にあると外縁が暗い帯と隣接して
        # quiet zone の意味が薄れる。触っていない間は畳む (映像の上に重ねているので
        # 畳んでもコードの大きさは変わらない)。
        self.setMouseTracking(True)
        self.view.setMouseTracking(True)
        self._bar_timer = QTimer(self)
        self._bar_timer.setSingleShot(True)
        self._bar_timer.timeout.connect(lambda: self.bar_widget.setVisible(False))

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

    def _show_bar(self) -> None:
        self.bar_widget.setVisible(True)
        self._bar_timer.start(2500)

    def mouseMoveEvent(self, event) -> None:  # noqa: N802 (Qt の命名)
        self._show_bar()
        super().mouseMoveEvent(event)

    def resizeEvent(self, event) -> None:  # noqa: N802 (Qt の命名)
        # バーは常にウィンドウ下端に全幅で重ねる
        h = self.bar_widget.sizeHint().height()
        self.bar_widget.setGeometry(0, self.height() - h, self.width(), h)
        super().resizeEvent(event)

    def keyPressEvent(self, event) -> None:  # noqa: N802 (Qt の命名)
        """矢印でコードを動かし、+/- で拡縮する (Shift で粗く、0 で戻す、H で静止)。

        受信側を三脚に据えたまま構図を追い込めるようにする。窓ごと動かすと
        背景のデスクトップまで変わって検出の条件が動くので、白い面の中で
        コードだけを動かす。合わせた値はバーに出るので、そのまま --zoom /
        --dx / --dy に渡して同じ構図を再現できる。
        """
        shift = bool(event.modifiers() & Qt.KeyboardModifier.ShiftModifier)
        step = 0.02 if shift else 0.005
        zstep = 0.05 if shift else 0.01
        z, dx, dy = self.view.placement
        match event.key():
            case Qt.Key.Key_Left:
                self.view.set_placement(dx=dx - step)
            case Qt.Key.Key_Right:
                self.view.set_placement(dx=dx + step)
            case Qt.Key.Key_Up:
                self.view.set_placement(dy=dy - step)
            case Qt.Key.Key_Down:
                self.view.set_placement(dy=dy + step)
            case Qt.Key.Key_Plus | Qt.Key.Key_Equal:
                self.view.set_placement(zoom=z + zstep)
            case Qt.Key.Key_Minus | Qt.Key.Key_Underscore:
                self.view.set_placement(zoom=z - zstep)
            case Qt.Key.Key_0:
                self.view.set_placement(zoom=1.0, dx=0.0, dy=0.0)
            case Qt.Key.Key_H:
                self.hold_box.setChecked(not self.hold)
            case _:
                super().keyPressEvent(event)
                return
        self._show_bar()

    def start(self) -> None:
        _keep_display_awake(True)
        self._bar_timer.start(2500)
        self.show()
        self.raise_()
        self.activateWindow()
        self._deadline = time.perf_counter()
        self._tick()

    def _set_hold(self, on: bool) -> None:
        self.hold = on
        self._show_bar()

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
        z, dx, dy = self.view.placement
        self.status.setText(
            f"{self.label} · {self.payload_len:,} B · "
            f"{self.fps}fps 要求 / 実測 {measured:.1f}fps · "
            f"frame {self.index + 1}/{self.frame_count} · {self.pass_no} 巡目 · "
            f"{elapsed:.0f} 秒経過 (1 巡 {loop_sec:.1f} 秒) · "
            f"配置 --zoom {z:.2f} --dx {dx:+.3f} --dy {dy:+.3f}"
            + ("  [静止中]" if self.hold else "")
        )
        if not self.hold:
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

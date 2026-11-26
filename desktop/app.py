"""設定画面。ファイル/テキストを選び、格子・階調・fps を決めてコードを生成し、表示ウィンドウを開く。

符号化は Rust コア (vloom_core) をそのまま呼ぶので、吐くフレームは PWA・スマホアプリと
バイト単位で同一。受信側に手を入れる必要はない。

画面は「送るもの → コードの設定 → 生成」の上から下への 1 本道にしてある。普段触るのは
格子・fps・表示先だけで、bit/セル・リペア・余白・配置は計測で条件を振るときのための
もの (既定のままで使える) なので「詳細設定」に畳む。
"""

from __future__ import annotations

import math
import mimetypes
from datetime import datetime
import pathlib
import re

import vloom_core
from PySide6.QtCore import QRect, Qt, Signal
from PySide6.QtGui import QDragEnterEvent, QDropEvent, QFont, QGuiApplication, QMouseEvent
from PySide6.QtWidgets import (
    QComboBox,
    QDoubleSpinBox,
    QFileDialog,
    QFormLayout,
    QButtonGroup,
    QFrame,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPlainTextEdit,
    QPushButton,
    QSizePolicy,
    QSpinBox,
    QStackedWidget,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from .sender import SenderWindow
from .sweep import DEFAULT_DWELL_SEC, SweepTx, default_sweep, parse_sweep

# 受信側の総当たり候補に入っている格子だけを既定で出す。11×10 は候補外なので
# 受信側で格子を明示指定しない限り検出されない (選べるようにはしておく)。
GRIDS = ["7x6 (高密度)", "5x4 (標準)", "9x8 (超高密度)",
         "11x10 (最大)", "11x14 (縦長)", "13x18 (超密)", "15x20 (最密・寄せて使う)"]
# 受信側が「自動」で読める格子。CANDIDATES に加え、スキャナが写りから格子を見積もるので
# 速度テストの 3 格子 (11x14 / 13x18 / 15x20) も読める (2026-11-25 の速度テストで確認)
AUTO_DETECT_GRIDS = {(15, 20), (13, 18), (11, 14), (7, 6), (5, 4)}

# ソースパケットに対するリペアパケットの比率。PWA の REPAIR_RATE と同値。
REPAIR_RATE = 0.5
# コードの四辺に置く白余白 (セル数)。QR の quiet zone は 4 モジュールだが、
# こちらはウィンドウ枠や操作バーと隣接するぶん余裕を見て 8 セルにしている。
DEFAULT_MARGIN_CELLS = 8
# カメラの実効フレームレートが 23-25 fps なので、20 までは 1 枚ずつ拾える。
# 30 にすると混ざって回収率が落ちる (実測: 11x10 で 60 -> 33 KB/s)。
# 実際の初期値は、表示するモニタの書き換えを割り切れる、これ以下で最大の fps にする
# (64Hz のノート PC で 20fps を出すと表示時間が 3 回分と 4 回分で揃わず、取りこぼしが増えた)。
DEFAULT_FPS = 20


def fps_for_refresh(hz: float, cap: int = DEFAULT_FPS) -> int:
    """書き換え hz の画面で等間隔に出せる (hz / 整数)、cap 以下で最大の fps。"""
    r = round(hz) if hz and hz > 1 else 60
    return next((r // n for n in range(1, r + 1) if r % n == 0 and r // n <= cap), cap)

# 格子の一辺の上限。Rust 側 (VcodeTx) がこの範囲に丸めるので、超える指定をそのまま
# 渡すと「指定と違う格子が黙って送られる」ことになる (実際に 16x22 を指定して 16x20 が
# 出て、受信側の格子と食い違ったまま計測した)。ここで止める
GRID_SIDE_RANGE = (2, 32)

# 画面の配色 (ライト)。補足の文字・枠線・強調の 3 色だけを使う
MUTED = "#6b7280"
BORDER = "#d0d5dd"
ACCENT = "#2563eb"
WARN = "#b45309"

STYLE = f"""
QLabel[role="section"] {{ font-weight: 600; font-size: 11pt; }}
QLabel[role="muted"] {{ color: {MUTED}; }}
QLabel[role="warn"] {{ color: {WARN}; }}
QFrame[role="card"] {{
    border: 1px solid {BORDER}; border-radius: 8px; background: palette(base);
}}
QFrame#dropZone {{
    border: 1.5px dashed {BORDER}; border-radius: 8px; background: palette(base);
}}
QFrame#dropZone[hover="true"] {{ border-color: {ACCENT}; }}
QPushButton#primary {{
    background: {ACCENT}; color: white; border: none; border-radius: 6px;
    padding: 10px 16px; font-weight: 600; font-size: 11pt;
}}
QPushButton#primary:hover {{ background: #1d4ed8; }}
QPushButton#primary:pressed {{ background: #1e40af; }}
QToolButton#disclosure {{ border: none; font-weight: 600; padding: 2px 0; }}
QPushButton#segLeft, QPushButton#segRight {{
    border: 1px solid {BORDER}; background: palette(base); padding: 5px 18px; min-width: 64px;
}}
QPushButton#segLeft {{ border-top-left-radius: 6px; border-bottom-left-radius: 6px; }}
QPushButton#segRight {{
    border-left: none; border-top-right-radius: 6px; border-bottom-right-radius: 6px;
}}
QPushButton#segLeft:checked, QPushButton#segRight:checked {{
    background: {ACCENT}; border-color: {ACCENT}; color: white;
}}
"""


def parse_grid(text: str) -> tuple[int, int]:
    gw, gh = (int(v) for v in text.split(" ")[0].split("x"))
    lo, hi = GRID_SIDE_RANGE
    if not (lo <= gw <= hi and lo <= gh <= hi):
        raise ValueError(f"格子 {gw}x{gh} は範囲外です (縦横とも {lo}〜{hi})")
    return gw, gh


def _fmt_size(n: int) -> str:
    if n >= 1 << 20:
        return f"{n / (1 << 20):.1f} MB"
    if n >= 1 << 10:
        return f"{n / (1 << 10):.0f} KB"
    return f"{n} B"


def _fmt_sec(sec: float) -> str:
    if sec >= 60:
        return f"{int(sec // 60)} 分 {int(sec % 60)} 秒"
    return f"{sec:.0f} 秒" if sec >= 10 else f"{sec:.1f} 秒"


def _label(text: str, role: str | None = None, wrap: bool = False) -> QLabel:
    w = QLabel(text)
    if role:
        w.setProperty("role", role)
    w.setWordWrap(wrap)
    return w


class DropZone(QFrame):
    """ファイルを落とす / クリックして選ぶ領域。選ばれたら fileChosen を出す。"""

    fileChosen = Signal(pathlib.Path)

    def __init__(self) -> None:
        super().__init__()
        self.setObjectName("dropZone")
        self.setAcceptDrops(True)
        self.setCursor(Qt.CursorShape.PointingHandCursor)
        self.setMinimumHeight(96)
        self.title = QLabel("ここにファイルをドロップ")
        self.title.setAlignment(Qt.AlignmentFlag.AlignCenter)
        f = self.title.font()
        f.setPointSizeF(f.pointSizeF() + 1)
        f.setWeight(QFont.Weight.DemiBold)
        self.title.setFont(f)
        self.sub = _label("またはクリックして選ぶ", "muted")
        self.sub.setAlignment(Qt.AlignmentFlag.AlignCenter)
        lay = QVBoxLayout(self)
        lay.addStretch(1)
        lay.addWidget(self.title)
        lay.addWidget(self.sub)
        lay.addStretch(1)

    def show_file(self, path: pathlib.Path | None) -> None:
        if path is None:
            self.title.setText("ここにファイルをドロップ")
            self.sub.setText("またはクリックして選ぶ")
        else:
            self.title.setText(path.name)
            self.sub.setText(f"{_fmt_size(path.stat().st_size)} · クリックで選び直す")

    def _set_hover(self, on: bool) -> None:
        self.setProperty("hover", "true" if on else "false")
        self.style().unpolish(self)
        self.style().polish(self)

    def mouseReleaseEvent(self, e: QMouseEvent) -> None:
        if e.button() == Qt.MouseButton.LeftButton:
            path, _ = QFileDialog.getOpenFileName(self, "送るファイルを選ぶ")
            if path:
                self.fileChosen.emit(pathlib.Path(path))

    def dragEnterEvent(self, e: QDragEnterEvent) -> None:
        urls = e.mimeData().urls()
        if len(urls) == 1 and urls[0].isLocalFile() and pathlib.Path(urls[0].toLocalFile()).is_file():
            e.acceptProposedAction()
            self._set_hover(True)

    def dragLeaveEvent(self, e) -> None:  # noqa: ANN001 (QDragLeaveEvent)
        self._set_hover(False)

    def dropEvent(self, e: QDropEvent) -> None:
        self._set_hover(False)
        self.fileChosen.emit(pathlib.Path(e.mimeData().urls()[0].toLocalFile()))


class MainWindow(QWidget):
    TAB_FILE, TAB_TEXT = 0, 1

    def __init__(self) -> None:
        super().__init__()
        self.setWindowTitle("Vloom")
        self.setStyleSheet(STYLE)
        self.setMinimumWidth(520)
        self.path: pathlib.Path | None = None
        self.stage: SenderWindow | None = None
        # 一度開いた送信ウィンドウの位置とサイズを覚えておく。三脚でカメラを固定して
        # 条件を振るとき、送信のたびに位置が既定値へ戻ると毎回構図を取り直すことになる。
        self.stage_geometry: QRect | None = None
        # 起動直後から静止させるか (--hold)。構図合わせのときに使う
        self.hold_at_start = False
        # 1 セルを整数画素で描くか (--snap)。効くかは画面の解像度と格子によるので、
        # 既定は従来どおり切り下げなし。比べて決めるための切り替え。
        self.snap_cells = False

        root = QVBoxLayout(self)
        root.setContentsMargins(20, 18, 20, 18)
        root.setSpacing(10)

        # ---- 送るもの ----
        root.addWidget(_label("送るもの", "section"))
        self.drop = DropZone()
        self.drop.fileChosen.connect(self._set_file)
        self.file_label = _label("", "muted")  # 互換用 (計測スクリプトのログに残る)
        # ファイル / テキストの切り替え。タブ部品 (QTabWidget) はタブ以外の帯が白く
        # 描かれて浮くので、つながった 2 つのボタンにする
        self.seg_file = QPushButton("ファイル")
        self.seg_file.setObjectName("segLeft")
        self.seg_text = QPushButton("テキスト")
        self.seg_text.setObjectName("segRight")
        self.source_group = QButtonGroup(self)
        for k, b in enumerate((self.seg_file, self.seg_text)):
            b.setCheckable(True)
            b.setCursor(Qt.CursorShape.PointingHandCursor)
            self.source_group.addButton(b, k)
        self.seg_file.setChecked(True)
        # 解除は切り替えの右端に置き、ファイルを選んだときだけ出す
        self.clear_btn = QToolButton()
        self.clear_btn.setText("選択を解除")
        self.clear_btn.setAutoRaise(True)
        self.clear_btn.setVisible(False)
        self.clear_btn.clicked.connect(self._clear_file)
        seg_row = QHBoxLayout()
        seg_row.setSpacing(0)
        seg_row.addWidget(self.seg_file)
        seg_row.addWidget(self.seg_text)
        seg_row.addStretch(1)
        seg_row.addWidget(self.clear_btn)
        root.addLayout(seg_row)

        self.text = QPlainTextEdit()
        self.text.setPlaceholderText("送るテキストを入力")
        self.text.setFixedHeight(self.drop.minimumHeight())
        self.source_stack = QStackedWidget()
        self.source_stack.addWidget(self.drop)
        self.source_stack.addWidget(self.text)
        self.source_stack.setFixedHeight(self.drop.minimumHeight())
        self.source_group.idClicked.connect(self.source_stack.setCurrentIndex)
        self.source_stack.currentChanged.connect(self._sync_clear_btn)
        root.addWidget(self.source_stack)

        self.name_override = QLineEdit()
        self.name_override.setPlaceholderText("省略すると元のファイル名 (テキストは日時入りの名前)")
        name_form = QFormLayout()
        name_form.setLabelAlignment(Qt.AlignmentFlag.AlignLeft)
        name_form.addRow("受信側での名前", self.name_override)
        root.addLayout(name_form)

        root.addSpacing(6)

        # ---- 送信の設定 ----
        root.addWidget(_label("コードの設定", "section"))
        self.grid = QComboBox()
        self.grid.addItems(GRIDS)
        self.grid.setCurrentText("13x18 (超密)")  # 既定は縦長・超密 (実測最速)
        self.fps = QSpinBox()
        self.fps.setRange(2, 60)
        self.fps.setValue(DEFAULT_FPS)  # モニタが決まったら _apply_default_fps で合わせ直す
        # 手で fps を変えたら、モニタを切り替えても上書きしない
        self._fps_touched = False
        self.fps.editingFinished.connect(lambda: setattr(self, "_fps_touched", True))
        self.fps.setSuffix(" fps")
        self.screen = QComboBox()
        for i, s in enumerate(QGuiApplication.screens()):
            g = s.geometry()
            self.screen.addItem(f"{i + 1}: {s.name()} ({g.width()}×{g.height()}, "
                                f"{s.refreshRate():.0f}Hz)")
        main_form = QFormLayout()
        main_form.setLabelAlignment(Qt.AlignmentFlag.AlignLeft)
        main_form.setHorizontalSpacing(16)
        main_form.addRow("格子", self.grid)
        main_form.addRow("フレームレート", self.fps)
        main_form.addRow("表示するモニタ", self.screen)
        root.addLayout(main_form)

        # 目安 (理論速度・所要時間・注意) をカードにまとめる
        card = QFrame()
        card.setProperty("role", "card")
        cl = QVBoxLayout(card)
        cl.setContentsMargins(12, 10, 12, 10)
        cl.setSpacing(2)
        self.theory = QLabel("")
        f = self.theory.font()
        f.setWeight(QFont.Weight.DemiBold)
        self.theory.setFont(f)
        self.estimate = _label("", "muted", wrap=True)
        self.warn = _label("", "warn", wrap=True)
        cl.addWidget(self.theory)
        cl.addWidget(self.estimate)
        cl.addWidget(self.warn)
        root.addWidget(card)

        # ---- 詳細設定 (計測用。既定のままで使える) ----
        self.adv_toggle = QToolButton()
        self.adv_toggle.setObjectName("disclosure")
        self.adv_toggle.setText("詳細設定")
        self.adv_toggle.setCheckable(True)
        self.adv_toggle.setToolButtonStyle(Qt.ToolButtonStyle.ToolButtonTextBesideIcon)
        self.adv_toggle.setArrowType(Qt.ArrowType.RightArrow)
        self.adv_toggle.toggled.connect(self._toggle_advanced)
        root.addWidget(self.adv_toggle)

        self.bpc = QComboBox()
        # 既定は 1bit (白黒)。輝度 4 値は復号できるが、同じ格子の 1bit より遅い
        # (docs/experiments.md 2026-08-28)。既定を 4 値にしていたせいで、
        # --bpc を省いたコマンドが黙って 4 値で送り、13x18 で回収 0/234 になった。
        # PWA・スマホアプリの既定も 1bit なので、そちらに揃える。
        self.bpc.addItems(["1 (白黒)", "2 (輝度4値)"])
        self.repair = QSpinBox()
        # 捕捉率が低いほど高いリペア率が効くので、振り幅を広く取っておく
        self.repair.setRange(0, 400)
        self.repair.setValue(int(REPAIR_RATE * 100))
        self.repair.setSuffix(" %")
        # コードの四辺に置く白余白 (セル数)。QR の quiet zone 相当。
        self.margin = QSpinBox()
        self.margin.setRange(0, 20)
        self.margin.setValue(DEFAULT_MARGIN_CELLS)
        self.margin.setSuffix(" セル")
        # 窓の中でのコードの大きさと位置。窓ごと動かすと背景のデスクトップまで
        # 変わって検出の条件が動くうえ、setGeometry はクライアント矩形を指すので
        # OS のフレーム分ずれる。白い面は据えたまま、中のコードだけを動かせると
        # 三脚の構図を崩さずに追い込める。送信中は矢印キーと +/- でも動かせる。
        self.zoom = QDoubleSpinBox()
        self.zoom.setRange(0.05, 1.00)
        self.zoom.setSingleStep(0.05)
        self.zoom.setDecimals(2)
        self.zoom.setValue(1.00)
        self.zoom.setPrefix("× ")
        self.dx = QDoubleSpinBox()
        self.dy = QDoubleSpinBox()
        for w, p in ((self.dx, "横 "), (self.dy, "縦 ")):
            w.setRange(-0.50, 0.50)
            w.setSingleStep(0.01)
            w.setDecimals(3)
            w.setValue(0.0)
            w.setPrefix(p)
        place = QHBoxLayout()
        place.setContentsMargins(0, 0, 0, 0)
        for w in (self.zoom, self.dx, self.dy):
            w.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
            place.addWidget(w)
        place_row = QWidget()
        place_row.setLayout(place)

        self.advanced = QWidget()
        adv_form = QFormLayout(self.advanced)
        adv_form.setContentsMargins(16, 0, 0, 0)
        adv_form.setLabelAlignment(Qt.AlignmentFlag.AlignLeft)
        adv_form.setHorizontalSpacing(16)
        adv_form.addRow("bit/セル", self.bpc)
        adv_form.addRow("リペア", self.repair)
        adv_form.addRow("余白", self.margin)
        adv_form.addRow("コードの配置", place_row)
        adv_form.addRow("", _label("表示中も矢印キーで位置、+/- で大きさ、H で静止を切り替えられます",
                                   "muted", wrap=True))
        self.advanced.setVisible(False)
        root.addWidget(self.advanced)

        root.addStretch(1)

        # ---- 送信 ----
        start = QPushButton("生成")
        start.setObjectName("primary")
        start.setDefault(True)
        start.clicked.connect(self._start)
        # ファイルは送らず、格子と fps を自動で切り替えながら試験フレームを流す。
        # 受信側に設定ごとの速さが出続けるので、その環境で一番速い組が分かる
        sweep = QPushButton("速度テスト")
        sweep.setToolTip("格子と fps を数秒ずつ切り替えながら試験用のコードを表示します。"
                         "受信側に設定ごとの速さが出るので、一番速い組が分かります")
        sweep.clicked.connect(lambda: self.start_sweep())
        actions = QHBoxLayout()
        actions.addWidget(start, 1)
        actions.addWidget(sweep)
        root.addLayout(actions)
        # 生成中・速度テスト中の状態だけを短く出す (説明文は置かない)
        self.info = _label("", "muted", wrap=True)
        root.addWidget(self.info)

        self.screen.currentIndexChanged.connect(self._apply_default_fps)
        self._apply_default_fps()
        for w in (self.grid, self.bpc, self.screen):
            w.currentIndexChanged.connect(self._update_theory)
        for w in (self.fps, self.repair):
            w.valueChanged.connect(self._update_theory)
        self.text.textChanged.connect(self._update_theory)
        self.source_stack.currentChanged.connect(self._update_theory)
        self._update_theory()

    # ---- 入力 ----

    def _set_file(self, path: pathlib.Path) -> None:
        self.path = path
        self.drop.show_file(path)
        self.file_label.setText(f"{path.name} ({path.stat().st_size:,} B)")
        self.source_stack.setCurrentIndex(self.TAB_FILE)
        self.seg_file.setChecked(True)
        self._sync_clear_btn()
        self._update_theory()

    def _apply_default_fps(self, *_: object) -> None:
        """選んだモニタの書き換えに合う fps を初期値にする (手で変えていなければ)。"""
        if self._fps_touched:
            return
        screens = QGuiApplication.screens()
        hz = screens[min(self.screen.currentIndex(), len(screens) - 1)].refreshRate()
        self.fps.setValue(fps_for_refresh(hz))

    def _sync_clear_btn(self, *_: object) -> None:
        """「選択を解除」はファイル側を開いていて、ファイルを選んであるときだけ出す。"""
        self.clear_btn.setVisible(
            self.source_stack.currentIndex() == self.TAB_FILE and self.path is not None)

    def _pick_file(self) -> None:
        path, _ = QFileDialog.getOpenFileName(self, "送るファイルを選ぶ")
        if path:
            self._set_file(pathlib.Path(path))

    def _clear_file(self) -> None:
        self.path = None
        self.drop.show_file(None)
        self._sync_clear_btn()
        self.file_label.setText("")
        self._update_theory()

    def _toggle_advanced(self, on: bool) -> None:
        self.advanced.setVisible(on)
        self.adv_toggle.setArrowType(Qt.ArrowType.DownArrow if on else Qt.ArrowType.RightArrow)
        self.adjustSize()

    def _payload(self) -> tuple[bytes, str, str] | None:
        """送るバイト列と、受信側に伝える名前・MIME。いま開いているタブのものを送る。"""
        if self.source_stack.currentIndex() == self.TAB_FILE:
            if self.path is None:
                return None
            data = self.path.read_bytes()
            name = self.name_override.text().strip() or self.path.name
            mime = mimetypes.guess_type(name)[0] or "application/octet-stream"
            return data, name, mime
        text = self.text.toPlainText()
        if not text:
            return None
        # 毎回 message.txt だと受信側で同名ファイルが積み重なるので、送信時刻を付ける
        # (PWA / Android アプリと同じ挙動)
        stamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        name = self.name_override.text().strip() or f"message_{stamp}.txt"
        return text.encode("utf-8"), name, "text/plain;charset=utf-8"

    def _payload_size(self) -> int | None:
        """目安の計算用。読み込まずにサイズだけ見る。"""
        if self.source_stack.currentIndex() == self.TAB_FILE:
            return self.path.stat().st_size if self.path is not None else None
        n = len(self.text.toPlainText().encode("utf-8"))
        return n or None

    # ---- 表示 ----

    def _refresh_note(self, fps: int) -> str:
        """要求 fps とモニタのリフレッシュレートの噛み合わせを見る。問題なければ空文字。

        コンポジタは垂直同期でしか画を出さないので、実際の表示時間はリフレッシュ
        周期の整数倍に丸められる。リフレッシュの整数分の 1 でない fps を要求すると
        フレームごとに表示時間が変わり、計測の条件が揃わない (例: 60Hz で 8fps を
        要求すると 7 周期と 8 周期が交互になる)。
        """
        screens = QGuiApplication.screens()
        hz = screens[min(self.screen.currentIndex(), len(screens) - 1)].refreshRate()
        if hz <= 0:
            return ""
        periods = hz / fps
        nearest = round(periods)
        if nearest < 2:
            return f"{hz:.0f}Hz のモニタでは 1 フレームが 2 リフレッシュ未満になります。fps を下げてください。"
        if abs(periods - nearest) > 0.02:
            # hz を割り切る fps だけを挙げる (hz/n が整数になる n)
            clean = sorted(
                {round(hz / n) for n in range(2, 31) if abs(hz / n - round(hz / n)) < 0.02},
                reverse=True,
            )
            return (f"{hz:.0f}Hz の整数分の 1 でないため表示時間が不揃いになります。"
                    f"割り切れる fps: {', '.join(str(c) for c in clean)}")
        return ""

    def _update_theory(self) -> None:
        gw, gh = parse_grid(self.grid.currentText())
        bpc = 1 if self.bpc.currentIndex() == 0 else 2
        fps = self.fps.value()
        ps = vloom_core.packet_size(bpc)
        per_frame = gw * gh
        kbps = per_frame * ps * fps / 1024
        self.theory.setText(f"理論 {kbps:.0f} KB/s   ({gw}×{gh} · {bpc}bit · {fps}fps)")

        size = self._payload_size()
        self.estimate.setVisible(size is not None)
        if size is not None:
            # 最短 = ソースパケットを 1 枚も落とさず受け取れた場合。1 巡はリペア込みの全フレーム
            source = math.ceil(size / ps)
            best = math.ceil(source / per_frame) / fps
            loop = math.ceil(source * (1 + self.repair.value() / 100) / per_frame) / fps
            self.estimate.setText(f"{_fmt_size(size)} · 最短 {_fmt_sec(best)} · 1 巡 {_fmt_sec(loop)}")

        warns = []
        if (gw, gh) not in AUTO_DETECT_GRIDS:
            warns.append("この格子は受信側で格子の指定が必要です。")
        if note := self._refresh_note(fps):
            warns.append(note)
        self.warn.setText("\n".join(warns))
        self.warn.setVisible(bool(warns))

    def _start(self) -> None:
        payload = self._payload()
        if payload is None:
            QMessageBox.information(
                self, "Vloom", "ファイルを選択するか、テキストを入力してください。")
            return
        data, name, mime = payload
        gw, gh = parse_grid(self.grid.currentText())
        bpc = 1 if self.bpc.currentIndex() == 0 else 2
        fps = self.fps.value()

        # 元のファイル名/MIME をヘッダに埋めて送る (受信側で元名・種別をそのまま復元)
        source = vloom_core.wrap_file(name, mime, data)
        source_packets = math.ceil(len(source) / vloom_core.packet_size(bpc))
        extra_repair = math.ceil(source_packets * self.repair.value() / 100)
        tx = vloom_core.VcodeTx(source, extra_repair, gw, gh, bpc)

        self.stage = SenderWindow(tx, fps, len(data), name, self.margin.value(),
                                  self.zoom.value(), self.snap_cells,
                                  self.dx.value(), self.dy.value(),
                                  hold=self.hold_at_start)
        self.stage.closed.connect(self._on_stage_closed)
        self.stage.setGeometry(self.stage_geometry or self._stage_rect())
        self.stage.start()
        self.info.setText(
            f"生成中: {tx.frame_count} フレーム · 1 巡 {tx.frame_count / fps:.1f} 秒")

    def start_sweep(self, spec: str | None = None,
                    dwell_sec: float = DEFAULT_DWELL_SEC) -> None:
        """速度テスト (旧称「さがす」) で表示ウィンドウを開く。spec は "13x18@20,15x20@30" の形。

        spec を省くと、送信ウィンドウが乗る画面の書き換え周期から fps の候補を決める。
        """
        rect = self.stage_geometry or self._stage_rect()
        screen = QGuiApplication.screenAt(rect.center()) or QGuiApplication.primaryScreen()
        refresh_hz = screen.refreshRate() if screen is not None else 0.0
        try:
            settings = parse_sweep(spec or default_sweep(refresh_hz), GRID_SIDE_RANGE)
        except ValueError as e:
            QMessageBox.warning(self, "Vloom", str(e))
            return
        tx = SweepTx(settings, dwell_sec)
        # 配置 (余白・倍率・位置) は通常の送信と同じものを使う。格子ごとに窓いっぱいへ
        # 合わせ直されるので、どの格子もほぼ同じ大きさで写る (= 同じ距離での比較になる)
        self.stage = SenderWindow(tx, settings[0].fps, 0, "速度テスト", self.margin.value(),
                                  self.zoom.value(), self.snap_cells,
                                  self.dx.value(), self.dy.value(),
                                  hold=self.hold_at_start)
        self.stage.closed.connect(self._on_stage_closed)
        self.stage.setGeometry(rect)
        self.stage.start()
        print(f"速度テスト: 画面 {refresh_hz:.0f}Hz / " + ", ".join(s.label for s in settings),
              flush=True)
        self.info.setText(
            "速度テスト中: " + " → ".join(s.label for s in settings)
            + f"  (1 設定 {dwell_sec:g} 秒)")

    def _stage_rect(self) -> QRect:
        """送信ウィンドウの初期位置。選んだモニタの作業領域に収まる正方形を中央に置く。

        コードは縦横比がほぼ 1:1 なので、正方形にしておくと最初から無駄な余白が出ない。
        """
        screens = QGuiApplication.screens()
        area = screens[min(self.screen.currentIndex(), len(screens) - 1)].availableGeometry()
        side = int(min(area.width(), area.height()) * 0.85)
        return QRect(
            area.x() + (area.width() - side) // 2,
            area.y() + (area.height() - side) // 2,
            side,
            side,
        )

    def _on_stage_closed(self) -> None:
        # 次回も同じ構図で出せるよう、閉じる直前の位置を覚える
        if self.stage is not None:
            self.stage_geometry = self.stage.geometry()
        self.stage = None
        self.info.setText("")

    # ---- 外部からの制御 (計測の自動化用) ----

    def apply_settings(self, *, file: str | None = None, grid: str | None = None,
                       bpc: int | None = None, fps: int | None = None,
                       repair: int | None = None, margin: int | None = None,
                       zoom: float | None = None, dx: float | None = None,
                       dy: float | None = None, hold: bool = False, snap: bool = False,
                       geometry: tuple[int, int, int, int] | None = None) -> None:
        """コマンドラインから条件を流し込む。指定のないものは UI の値のまま。"""
        if file:
            self._set_file(pathlib.Path(file))
        if grid:
            i = next((k for k in range(self.grid.count())
                      if self.grid.itemText(k).split(" ")[0] == grid), None)
            if i is None:
                # リストに無い格子は選択肢に足して選ぶ。計測で格子を振るときに、
                # 候補を先に登録しないと送れないのでは探索が広がらない。
                if not re.fullmatch(r"\d{1,2}x\d{1,2}", grid):
                    raise SystemExit(f"格子の形式が不正: {grid} (例: 11x14)")
                self.grid.addItem(f"{grid} (指定)")
                i = self.grid.count() - 1
            self.grid.setCurrentIndex(i)
        if bpc is not None:
            self.bpc.setCurrentIndex(0 if bpc == 1 else 1)
        if fps is not None:
            self._fps_touched = True
            self.fps.setValue(fps)
        if repair is not None:
            self.repair.setValue(repair)
        if margin is not None:
            self.margin.setValue(margin)
        if zoom is not None:
            self.zoom.setValue(zoom)
        if dx is not None:
            self.dx.setValue(dx)
        if dy is not None:
            self.dy.setValue(dy)
        self.hold_at_start = hold
        self.snap_cells = snap
        if geometry is not None:
            self.stage_geometry = QRect(*geometry)
        self._update_theory()

    def start_now(self) -> None:
        """UI 操作なしで送信を始める。"""
        self._start()
        if self.stage is not None:
            g = self.stage.geometry()
            print(f"送信ウィンドウ: --geometry {g.x()},{g.y()},{g.width()},{g.height()}",
                  flush=True)

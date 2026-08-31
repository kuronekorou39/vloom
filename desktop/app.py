"""送信の設定画面。ファイル/テキストを選び、格子・階調・fps を決めて送信ウィンドウを開く。

符号化は Rust コア (vloom_core) をそのまま呼ぶので、吐くフレームは PWA・スマホアプリと
バイト単位で同一。受信側に手を入れる必要はない。
"""

from __future__ import annotations

import math
import mimetypes
from datetime import datetime
import pathlib
import re

import vloom_core
from PySide6.QtCore import QRect, Qt
from PySide6.QtGui import QGuiApplication
from PySide6.QtWidgets import (
    QComboBox,
    QDoubleSpinBox,
    QFileDialog,
    QFormLayout,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPlainTextEdit,
    QPushButton,
    QSpinBox,
    QVBoxLayout,
    QWidget,
)

from .sender import SenderWindow

# 受信側の総当たり候補に入っている格子だけを既定で出す。11×10 は候補外なので
# 受信側で格子を明示指定しない限り検出されない (選べるようにはしておく)。
GRIDS = ["7x6 (高密度)", "5x4 (標準)", "9x8 (超高密度)",
         "11x10 (最大)", "11x14 (縦長)", "13x18 (超密)"]
AUTO_DETECT_GRIDS = {(5, 4), (7, 6), (9, 8)}

# ソースパケットに対するリペアパケットの比率。PWA の REPAIR_RATE と同値。
REPAIR_RATE = 0.5
# コードの四辺に置く白余白 (セル数)。QR の quiet zone は 4 モジュールだが、
# こちらはウィンドウ枠や操作バーと隣接するぶん余裕を見て 8 セルにしている。
DEFAULT_MARGIN_CELLS = 8
# カメラの実効フレームレートが 23-25 fps なので、20 までは 1 枚ずつ拾える。
# 30 にすると混ざって回収率が落ちる (実測: 11x10 で 60 -> 33 KB/s)。
DEFAULT_FPS = 20


def parse_grid(text: str) -> tuple[int, int]:
    gw, gh = text.split(" ")[0].split("x")
    return int(gw), int(gh)


class MainWindow(QWidget):
    def __init__(self) -> None:
        super().__init__()
        self.setWindowTitle("Vloom — 送信")
        self.setMinimumWidth(560)
        self.path: pathlib.Path | None = None
        self.stage: SenderWindow | None = None
        # 一度開いた送信ウィンドウの位置とサイズを覚えておく。三脚でカメラを固定して
        # 条件を振るとき、送信のたびに位置が既定値へ戻ると毎回構図を取り直すことになる。
        self.stage_geometry: QRect | None = None
        # 起動直後から静止させるか (--hold)。構図合わせのときに使う
        self.hold_at_start = False

        self.file_label = QLabel("未選択")
        self.file_label.setWordWrap(True)
        pick = QPushButton("ファイルを選択...")
        pick.clicked.connect(self._pick_file)
        clear = QPushButton("解除")
        clear.clicked.connect(self._clear_file)

        file_row = QHBoxLayout()
        file_row.addWidget(pick)
        file_row.addWidget(clear)
        file_row.addWidget(self.file_label, 1)

        self.text = QPlainTextEdit()
        self.text.setPlaceholderText("または、ここにテキストを入力 (ファイル未選択時に送信)")
        self.text.setFixedHeight(64)

        self.grid = QComboBox()
        self.grid.addItems(GRIDS)
        self.bpc = QComboBox()
        self.bpc.addItems(["2 (輝度4値)", "1 (白黒)"])
        self.fps = QSpinBox()
        self.fps.setRange(2, 60)
        self.fps.setValue(DEFAULT_FPS)
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
        self.dx = QDoubleSpinBox()
        self.dy = QDoubleSpinBox()
        for w in (self.dx, self.dy):
            w.setRange(-0.50, 0.50)
            w.setSingleStep(0.01)
            w.setDecimals(3)
            w.setValue(0.0)

        self.screen = QComboBox()
        for i, s in enumerate(QGuiApplication.screens()):
            g = s.geometry()
            self.screen.addItem(f"{i + 1}: {s.name()} ({g.width()}x{g.height()})")

        self.name_override = QLineEdit()
        self.name_override.setPlaceholderText("受信側に見せるファイル名 (省略可)")

        form = QFormLayout()
        form.addRow("格子:", self.grid)
        form.addRow("bit/セル:", self.bpc)
        form.addRow("FPS:", self.fps)
        form.addRow("リペア:", self.repair)
        form.addRow("余白:", self.margin)
        place = QHBoxLayout()
        place.setContentsMargins(0, 0, 0, 0)
        place.addWidget(QLabel("倍率"))
        place.addWidget(self.zoom)
        place.addWidget(QLabel("横"))
        place.addWidget(self.dx)
        place.addWidget(QLabel("縦"))
        place.addWidget(self.dy)
        place_row = QWidget()
        place_row.setLayout(place)
        form.addRow("配置:", place_row)
        form.addRow("表示先:", self.screen)
        form.addRow("ファイル名:", self.name_override)

        self.theory = QLabel("")
        self.theory.setStyleSheet("color:#8a919c;")
        self.info = QLabel("スマホアプリ / PWA の「受信」をこの画面に向けてください。")
        self.info.setWordWrap(True)
        self.info.setStyleSheet("color:#8a919c;")

        start = QPushButton("送信開始")
        start.setDefault(True)
        start.clicked.connect(self._start)

        root = QVBoxLayout(self)
        root.addLayout(file_row)
        root.addWidget(self.text)
        root.addLayout(form)
        root.addWidget(self.theory)
        root.addWidget(start)
        root.addWidget(self.info)

        for w in (self.grid, self.bpc):
            w.currentIndexChanged.connect(self._update_theory)
        for w in (self.fps, self.repair):
            w.valueChanged.connect(self._update_theory)
        self._update_theory()

    # ---- 入力 ----

    def _pick_file(self) -> None:
        path, _ = QFileDialog.getOpenFileName(self, "送るファイルを選ぶ")
        if path:
            self.path = pathlib.Path(path)
            size = self.path.stat().st_size
            self.file_label.setText(f"{self.path.name} ({size:,} B)")
            self._update_theory()

    def _clear_file(self) -> None:
        self.path = None
        self.file_label.setText("未選択")
        self._update_theory()

    def _payload(self) -> tuple[bytes, str, str] | None:
        """送るバイト列と、受信側に伝える名前・MIME。"""
        if self.path is not None:
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

    # ---- 表示 ----

    def _refresh_note(self, fps: int) -> str:
        """要求 fps とモニタのリフレッシュレートの噛み合わせを見る。

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
            return f"   ※{hz:.0f}Hz では 1 フレームが 2 リフレッシュ未満 (fps 下げを推奨)"
        if abs(periods - nearest) > 0.02:
            # hz を割り切る fps だけを挙げる (hz/n が整数になる n)
            clean = sorted(
                {round(hz / n) for n in range(2, 31) if abs(hz / n - round(hz / n)) < 0.02},
                reverse=True,
            )
            return (f"   ※{hz:.0f}Hz の整数分の 1 でない (表示時間が不揃いになる)。"
                    f"割り切れる fps: {', '.join(str(c) for c in clean)}")
        return f"   ({hz:.0f}Hz の {nearest} リフレッシュ分を表示)"

    def _update_theory(self) -> None:
        gw, gh = parse_grid(self.grid.currentText())
        bpc = 2 if self.bpc.currentIndex() == 0 else 1
        fps = self.fps.value()
        kbps = gw * gh * vloom_core.packet_size(bpc) * fps / 1024
        note = "" if (gw, gh) in AUTO_DETECT_GRIDS else "   ※受信側で格子の明示指定が要る"
        self.theory.setText(
            f"理論 {kbps:.0f} KB/s  ({gw}x{gh} · {bpc}bit · {fps}fps){note}"
            f"\n{self._refresh_note(fps).strip()}")

    def _start(self) -> None:
        payload = self._payload()
        if payload is None:
            QMessageBox.information(
                self, "Vloom", "ファイルを選択するか、テキストを入力してください。")
            return
        data, name, mime = payload
        gw, gh = parse_grid(self.grid.currentText())
        bpc = 2 if self.bpc.currentIndex() == 0 else 1
        fps = self.fps.value()

        # 元のファイル名/MIME をヘッダに埋めて送る (受信側で元名・種別をそのまま復元)
        source = vloom_core.wrap_file(name, mime, data)
        source_packets = math.ceil(len(source) / vloom_core.packet_size(bpc))
        extra_repair = math.ceil(source_packets * self.repair.value() / 100)
        tx = vloom_core.VcodeTx(source, extra_repair, gw, gh, bpc)

        self.stage = SenderWindow(tx, fps, len(data), name, self.margin.value(),
                                  self.zoom.value(), self.dx.value(), self.dy.value(),
                                  hold=self.hold_at_start)
        self.stage.closed.connect(self._on_stage_closed)
        self.stage.setGeometry(self.stage_geometry or self._stage_rect())
        self.stage.start()
        self.info.setText(
            f"送信中: {tx.frame_count} フレーム / {tx.packet_count} パケット "
            f"(うちリペア {extra_repair})  ·  1 巡 {tx.frame_count / fps:.1f} 秒")

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
        self.info.setText("停止しました。")

    # ---- 外部からの制御 (計測の自動化用) ----

    def apply_settings(self, *, file: str | None = None, grid: str | None = None,
                       bpc: int | None = None, fps: int | None = None,
                       repair: int | None = None, margin: int | None = None,
                       zoom: float | None = None, dx: float | None = None,
                       dy: float | None = None, hold: bool = False,
                       geometry: tuple[int, int, int, int] | None = None) -> None:
        """コマンドラインから条件を流し込む。指定のないものは UI の値のまま。"""
        if file:
            self.path = pathlib.Path(file)
            self.file_label.setText(f"{self.path.name} ({self.path.stat().st_size:,} B)")
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
            self.bpc.setCurrentIndex(0 if bpc == 2 else 1)
        if fps is not None:
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

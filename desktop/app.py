"""送信の設定画面。ファイル/テキストを選び、格子・階調・fps を決めて送信ウィンドウを開く。

符号化は Rust コア (vloom_core) をそのまま呼ぶので、吐くフレームは PWA・スマホアプリと
バイト単位で同一。受信側に手を入れる必要はない。
"""

from __future__ import annotations

import math
import mimetypes
import pathlib

import vloom_core
from PySide6.QtCore import QRect, Qt
from PySide6.QtGui import QGuiApplication
from PySide6.QtWidgets import (
    QComboBox,
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
GRIDS = ["7x6 (高密度)", "5x4 (標準)", "9x8 (超高密度)", "11x10 (最大・受信側で要指定)"]
AUTO_DETECT_GRIDS = {(5, 4), (7, 6), (9, 8)}

# ソースパケットに対するリペアパケットの比率。PWA の REPAIR_RATE と同値。
REPAIR_RATE = 0.5
# 1 フレームは 2 リフレッシュ周期以上表示する必要がある。60Hz 画面での上限の目安。
REFRESH_SAFE_FPS = 30


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
        self.fps.setValue(12)
        self.repair = QSpinBox()
        self.repair.setRange(0, 200)
        self.repair.setValue(int(REPAIR_RATE * 100))
        self.repair.setSuffix(" %")

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
        name = self.name_override.text().strip() or "message.txt"
        return text.encode("utf-8"), name, "text/plain;charset=utf-8"

    # ---- 表示 ----

    def _update_theory(self) -> None:
        gw, gh = parse_grid(self.grid.currentText())
        bpc = 2 if self.bpc.currentIndex() == 0 else 1
        fps = self.fps.value()
        kbps = gw * gh * vloom_core.packet_size(bpc) * fps / 1024
        warn = f"   ※{REFRESH_SAFE_FPS}fps 超は 120Hz 画面向け" if fps > REFRESH_SAFE_FPS else ""
        note = "" if (gw, gh) in AUTO_DETECT_GRIDS else "   ※受信側で格子の明示指定が要る"
        self.theory.setText(f"理論 {kbps:.0f} KB/s  ({gw}x{gh} · {bpc}bit · {fps}fps){warn}{note}")

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

        self.stage = SenderWindow(tx, fps, len(data), name)
        self.stage.closed.connect(self._on_stage_closed)
        self.stage.setGeometry(self._stage_rect())
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
        self.stage = None
        self.info.setText("停止しました。")

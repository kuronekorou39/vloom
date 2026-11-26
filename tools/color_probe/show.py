"""模様を 1 枚、指定のモニタに全画面で出す (Esc で閉じる)。

    uv run --group desktop --group dev python tools/color_probe/show.py rgb8_c2 --screen 2
"""
from __future__ import annotations

import argparse
import sys

from PySide6.QtCore import Qt
from PySide6.QtGui import QGuiApplication, QImage, QKeySequence, QPainter, QShortcut
from PySide6.QtWidgets import QApplication, QWidget

from patterns import PATTERNS, SCREEN_H, SCREEN_W


class Stage(QWidget):
    def __init__(self, image) -> None:
        super().__init__()
        self._data = image.tobytes()
        self._img = QImage(self._data, SCREEN_W, SCREEN_H, SCREEN_W * 3, QImage.Format.Format_RGB888)
        QShortcut(QKeySequence(Qt.Key.Key_Escape), self, self.close)

    def paintEvent(self, _event) -> None:  # noqa: N802 (Qt の命名)
        p = QPainter(self)
        # 画面の画素と 1 対 1 で描く (拡大縮小するとセルの境目がにじむ)
        p.drawImage(0, 0, self._img)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("pattern", choices=sorted(PATTERNS))
    ap.add_argument("--screen", type=int, default=1, help="表示するモニタ (1 始まり)")
    args = ap.parse_args()

    app = QApplication(sys.argv[:1])
    screens = QGuiApplication.screens()
    geo = screens[max(0, min(args.screen - 1, len(screens) - 1))].geometry()
    if (geo.width(), geo.height()) != (SCREEN_W, SCREEN_H):
        print(f"警告: モニタが {geo.width()}x{geo.height()} で、模様の {SCREEN_W}x{SCREEN_H} と合いません",
              flush=True)
    stage = Stage(PATTERNS[args.pattern].image())
    stage.setWindowFlag(Qt.WindowType.WindowStaysOnTopHint, True)
    stage.setGeometry(geo)
    stage.showFullScreen()
    return app.exec()


if __name__ == "__main__":
    sys.exit(main())

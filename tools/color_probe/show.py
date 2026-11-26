"""模様を 1 枚、指定のモニタに全画面で出す (Esc で閉じる)。

    uv run --group desktop --group dev python tools/color_probe/show.py rgb8_c2 --screen 2
"""
from __future__ import annotations

import argparse
import sys

from PySide6.QtCore import Qt
from PySide6.QtGui import QGuiApplication, QImage, QKeySequence, QPainter, QShortcut
from PySide6.QtWidgets import QApplication, QWidget

from patterns import PATTERNS, SCREEN_H, SCREEN_W, stripes_image


class Stage(QWidget):
    def __init__(self, image) -> None:
        super().__init__()
        self._data = image.tobytes()
        self._img = QImage(self._data, SCREEN_W, SCREEN_H, SCREEN_W * 3, QImage.Format.Format_RGB888)
        QShortcut(QKeySequence(Qt.Key.Key_Escape), self, self.close)

    def paintEvent(self, _event) -> None:  # noqa: N802 (Qt の命名)
        p = QPainter(self)
        # 画面の画素と 1 対 1 で描く (拡大縮小するとセルの境目がにじむ)。模様より大きい画面
        # (2560x1440 など) では、白地の中央に置く
        p.fillRect(self.rect(), Qt.GlobalColor.white)
        p.drawImage((self.width() - SCREEN_W) // 2, (self.height() - SCREEN_H) // 2, self._img)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("pattern", choices=[*sorted(PATTERNS), "stripes"])
    ap.add_argument("--screen", type=int, default=1, help="表示するモニタ (1 始まり)")
    args = ap.parse_args()

    app = QApplication(sys.argv[:1])
    screens = QGuiApplication.screens()
    geo = screens[max(0, min(args.screen - 1, len(screens) - 1))].geometry()
    if (geo.width(), geo.height()) != (SCREEN_W, SCREEN_H):
        print(f"モニタは {geo.width()}x{geo.height()}。模様 ({SCREEN_W}x{SCREEN_H}) は中央に等倍で出します",
              flush=True)
    stage = Stage(stripes_image() if args.pattern == "stripes" else PATTERNS[args.pattern].image())
    stage.setWindowFlag(Qt.WindowType.WindowStaysOnTopHint, True)
    stage.setGeometry(geo)
    stage.showFullScreen()
    return app.exec()


if __name__ == "__main__":
    sys.exit(main())

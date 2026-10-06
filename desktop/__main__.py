"""エントリポイント: python -m desktop

計測で条件を振るときのために、コマンドラインから設定を流し込んで即送信できる。
三脚でカメラを固定していると構図を作り直すのが高くつくので、送信ウィンドウの
位置とサイズを --geometry で固定できるようにしてある (省略時は前回の位置を再利用)。

    # 手で操作する
    python -m desktop

    # 条件を指定して即送信 (位置も固定)
    python -m desktop --file web/pwa/testdata/test-100KB.jpg \
        --grid 7x6 --bpc 1 --fps 10 --repair 50 \
        --geometry 2917,-244,1199,1222 --start

    # 窓は動かさず、中のコードだけ小さく右下に寄せる
    python -m desktop --zoom 0.8 --dx 0.05 --dy 0.03 --start

    # さがす: 格子と fps を 4 秒ずつ切り替えながら試験フレームを流す (ファイルは送らない)。
    # 受信側に設定ごとの KB/s が出続ける。fps の候補は画面の書き換え周期から決まる
    # (60Hz なら 15 / 20 / 30)。組を自分で決めるなら --sweep に並べる
    python -m desktop --sweep
    python -m desktop --sweep 13x18@20,15x20@20,15x20@30 --dwell 5
"""

from __future__ import annotations

import argparse
import sys

from PySide6.QtWidgets import QApplication

from .app import MainWindow
from .sweep import DEFAULT_DWELL_SEC


def _geometry(text: str) -> tuple[int, int, int, int]:
    parts = text.split(",")
    if len(parts) != 4:
        raise argparse.ArgumentTypeError("--geometry は X,Y,幅,高 の 4 つ")
    return tuple(int(v) for v in parts)  # type: ignore[return-value]


def main() -> int:
    p = argparse.ArgumentParser(prog="desktop", description="Vloom 送信")
    p.add_argument("--file", help="送るファイル")
    p.add_argument("--grid", help="格子 (7x6 / 5x4 / 9x8 / 11x10 / 11x14 / 13x12)")
    p.add_argument("--bpc", type=int, choices=(1, 2), help="1 セルあたりのビット数")
    p.add_argument("--fps", type=int, help="送信フレームレート")
    p.add_argument("--repair", type=int, help="リペア率 (%%)")
    p.add_argument("--margin", type=int, help="コード四辺の白余白 (セル数)")
    # 窓の中でのコードの置き方。三脚の構図を崩さずに大きさ・位置だけ変えられる
    # (窓ごと動かすと背景も変わり、検出の条件が動いてしまう)。
    p.add_argument("--zoom", type=float, help="窓に対するコードの倍率 (0.05〜1.0)")
    p.add_argument("--dx", type=float, help="窓の中心からの横ずれ (-0.5〜0.5)")
    p.add_argument("--dy", type=float, help="窓の中心からの縦ずれ (-0.5〜0.5)")
    p.add_argument("--geometry", type=_geometry,
                   help="送信ウィンドウの位置とサイズ X,Y,幅,高 (省略時は前回位置)")
    p.add_argument("--hold", action="store_true",
                   help="静止 (調整用): フレームを進めず 1 枚を出し続ける。送信中は H で切替")
    p.add_argument("--start", action="store_true", help="起動と同時に送信を始める")
    p.add_argument("--sweep", nargs="?", const="auto", metavar="格子@fps,...",
                   help="さがす: 設定を切り替えながら試験フレームを流す "
                        "(組を省くと、画面の書き換え周期から fps の候補を決める)")
    p.add_argument("--dwell", type=float, default=DEFAULT_DWELL_SEC,
                   help="さがす: 1 つの設定を流し続ける秒数")
    p.add_argument("--snap", action="store_true",
                   help="1 セルを整数個の物理画素で描く (端数の乱れは消えるがコードは小さくなる)")
    args = p.parse_args()

    app = QApplication(sys.argv[:1])
    app.setApplicationName("Vloom")
    win = MainWindow()
    win.apply_settings(file=args.file, grid=args.grid, bpc=args.bpc,
                       fps=args.fps, repair=args.repair, margin=args.margin,
                       zoom=args.zoom, dx=args.dx, dy=args.dy, hold=args.hold,
                       snap=args.snap, geometry=args.geometry)
    win.show()
    if args.sweep:
        win.start_sweep(None if args.sweep == "auto" else args.sweep, args.dwell)
    elif args.start:
        win.start_now()
    return app.exec()


if __name__ == "__main__":
    sys.exit(main())

"""受信アプリを adb で繰り返し起動し、条件ごとの実効スループットを集計する。

送信側は先に流しっぱなしにしておく (デスクトップ送信アプリか PWA を全画面で表示)。
このスクリプトは受信側だけを自動で回す: 条件を Intent で渡して起動し、
完了時の `[vloom-stats]` を logcat から拾い、条件ごとに中央値と範囲を出す。

    # 手持ちのピントが効いているかの A/B (2026-09-06 の合成計測の裏取り)
    uv run --group dev python tools/measure_rx.py --runs 4 \\
        --arm "AF固定あり:--ei afhunt 1" --arm "AF固定なし:--ei afhunt 0"

    # 密度を落としたほうが速いかの A/B。格子は送受で揃っていないと読めないので、
    # --sender でその条件に入る前に送信側を起動し直す (同じままなら触らない)
    uv run --group dev python tools/measure_rx.py --runs 4 \\
        --arm "13x18:--es grid 13x18" --arm "11x14:--es grid 11x14" \\
        --sender "13x18=uv run python -m desktop --file web/pwa/testdata/test-1MB.jpg --grid 13x18 --fps 20 --start" \\
        --sender "11x14=uv run python -m desktop --file web/pwa/testdata/test-1MB.jpg --grid 11x14 --fps 20 --start"

注意 (docs/experiments.md の運用の教訓より):
  - 充電しながらの連続計測は熱制限で復号が 2〜3 倍遅くなる。充電を抜いて、
    冷えた状態から始めること。--cool で試行間に待ちを入れられる
  - 1MB 以上を送ること。100KB は 1 秒未満で終わり、定常が測れない
  - 受信アプリのカメラ権限は先に通しておくこと。権限ダイアログが出ると自動起動が
    止まる (adb shell pm grant <パッケージ> android.permission.CAMERA)
"""

from __future__ import annotations

import argparse
import re
import statistics
import subprocess
import time

# 計測用の lab ビルド (docs/development.md) が入っていればそちらを、
# 無ければ通常のパッケージを測る。どちらの運用でも --pkg を指定せずに済む
PKG_CANDIDATES = ("app.vloom.vloom.lab", "app.vloom.vloom")
STATS_RE = re.compile(r"\[vloom-stats\] (.*)")


def adb(*args: str, timeout: float = 30) -> str:
    return subprocess.run(
        ["adb", *args], capture_output=True, text=True, timeout=timeout, encoding="utf-8",
        errors="replace",
    ).stdout


def detect_pkg() -> str | None:
    """端末に入っている Vloom を探す (lab があればそちらを優先)。"""
    installed = {
        line.strip().removeprefix("package:")
        for line in adb("shell", "pm", "list", "packages").splitlines()
    }
    return next((c for c in PKG_CANDIDATES if c in installed), None)


def parse_stats(line: str) -> dict[str, str]:
    """`key=value | key=value` を辞書にする (アプリの _statsRows と同じ並び)。"""
    out: dict[str, str] = {}
    for part in line.split("|"):
        if "=" in part:
            k, v = part.split("=", 1)
            out[k.strip()] = v.strip()
    return out


def one_run(pkg: str, extras: list[str], timeout: float) -> dict[str, str] | None:
    """1 回ぶん起動して、完了の統計行を待つ。時間切れなら None。"""
    adb("logcat", "-c")
    adb("shell", "am", "force-stop", pkg)
    time.sleep(1.0)
    adb("shell", "am", "start", "-n", f"{pkg}/app.vloom.vloom.MainActivity",
        "--ei", "tab", "1", *extras)
    proc = subprocess.Popen(
        ["adb", "logcat", "-s", "flutter:V"],
        stdout=subprocess.PIPE, text=True, encoding="utf-8", errors="replace",
    )
    deadline = time.time() + timeout
    try:
        assert proc.stdout is not None
        for line in proc.stdout:
            m = STATS_RE.search(line)
            if m:
                return parse_stats(m.group(1))
            if time.time() > deadline:
                return None
    finally:
        proc.kill()
    return None


def kbps_of(stats: dict[str, str]) -> float | None:
    v = stats.get("実効スループット", "")
    m = re.match(r"([\d.]+)", v)
    return float(m.group(1)) if m else None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--arm", action="append", required=True,
                    help='"表示名:--ei afhunt 1" の形で条件を並べる (複数可)')
    ap.add_argument("--runs", type=int, default=3, help="条件あたりの試行回数")
    ap.add_argument("--timeout", type=float, default=120, help="1 試行の待ち時間 (秒)")
    ap.add_argument("--cool", type=float, default=5, help="試行の間に待つ秒数 (発熱対策)")
    ap.add_argument("--pkg", help="受信アプリのパッケージ名 (既定は端末から自動判別)")
    ap.add_argument("--sender", action="append", default=[],
                    help='"条件名=コマンド" 。格子を振るときは送信側も変えないと'
                         '成立しないので、その条件に入る前に送信側を起動し直す')
    ap.add_argument("--sender-warmup", type=float, default=6,
                    help="送信側を起動してから受信を始めるまでの待ち (秒)")
    args = ap.parse_args()

    senders: dict[str, str] = {}
    for sp in args.sender:
        name, _, cmd = sp.partition("=")
        senders[name.strip()] = cmd.strip()

    arms: list[tuple[str, list[str]]] = []
    for a in args.arm:
        name, _, rest = a.partition(":")
        arms.append((name.strip(), rest.split()))

    if not adb("devices").strip().splitlines()[1:]:
        print("端末が見つかりません (adb devices)")
        return 1

    pkg = args.pkg or detect_pkg()
    if pkg is None:
        print(f"Vloom が入っていません (探したもの: {', '.join(PKG_CANDIDATES)})")
        return 1
    print(f"受信アプリ: {pkg}")

    results: dict[str, list[float]] = {n: [] for n, _ in arms}
    extra_cols: dict[str, list[str]] = {n: [] for n, _ in arms}
    sender_proc: subprocess.Popen | None = None
    sender_cmd = None
    for i in range(args.runs):
        # 条件を交互に回す。まとめて回すと発熱や環境光の変化が片方に偏る
        for name, extras in arms:
            want = senders.get(name)
            if want and want != sender_cmd:
                # 送信側の条件が変わったので開き直す (同じままなら触らない)
                if sender_proc is not None:
                    sender_proc.terminate()
                    sender_proc.wait(timeout=10)
                print(f"送信側を起動: {want}")
                sender_proc = subprocess.Popen(want, shell=True)
                sender_cmd = want
                time.sleep(args.sender_warmup)
            print(f"[{i + 1}/{args.runs}] {name} …", end=" ", flush=True)
            stats = one_run(pkg, extras, args.timeout)
            if stats is None:
                print("時間切れ")
                continue
            k = kbps_of(stats)
            if k is None:
                print("統計を読めず")
                continue
            results[name].append(k)
            af = stats.get("AF 探り直し", "-")
            extra_cols[name].append(af)
            print(f"{k:.1f} KB/s (AF {af})")
            time.sleep(args.cool)

    if sender_proc is not None:
        sender_proc.terminate()
    print()
    print(f"{'条件':<16} {'試行':>4} {'中央値':>9} {'最小':>8} {'最大':>8}  AF 探り直し")
    for name, _ in arms:
        v = results[name]
        if not v:
            print(f"{name:<16} {'0':>4}  (結果なし)")
            continue
        af = extra_cols[name][-1] if extra_cols[name] else "-"
        print(f"{name:<16} {len(v):>4} {statistics.median(v):>9.1f} "
              f"{min(v):>8.1f} {max(v):>8.1f}  {af}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

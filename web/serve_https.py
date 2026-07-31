"""Vloom web/ ディレクトリを HTTPS で配信する開発用サーバー。

スマホからのカメラアクセスは getUserMedia の都合上 HTTPS or localhost が必須。
自己署名証明書を一度だけ生成し、http.server ベースで配信する。

Usage:
    python web/serve_https.py [--port 8443] [--ip 192.168.11.52]

スマホ側:
    1. PC と同じ Wi-Fi に接続
    2. https://<PC の IP>:8443/index.html を開く
    3. 証明書警告 → 「詳細設定」→「<host> にアクセスする (安全ではありません)」
    4. カメラ許可を求められたら許可
"""

from __future__ import annotations

import argparse
import http.server
import ipaddress
import json
import socket
import ssl
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

CERT_DIR = Path(__file__).parent / "certs"
CERT_FILE = CERT_DIR / "cert.pem"
KEY_FILE = CERT_DIR / "key.pem"


def get_local_ips() -> list[str]:
    """ローカルネットワーク IP のリストを返す (LAN 内アクセス用)。"""
    ips = set()
    try:
        for info in socket.getaddrinfo(socket.gethostname(), None):
            ip = info[4][0]
            if ":" in ip:
                continue
            ips.add(ip)
    except Exception:
        pass
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
            s.connect(("8.8.8.8", 80))
            ips.add(s.getsockname()[0])
    except Exception:
        pass
    return sorted(ips)


def make_self_signed_cert(ips: list[str]) -> None:
    """ローカル IP を SAN に入れた自己署名証明書を生成して PEM 保存する。"""
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import rsa
    from cryptography.x509.oid import NameOID

    CERT_DIR.mkdir(exist_ok=True)
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)

    subject = issuer = x509.Name(
        [
            x509.NameAttribute(NameOID.COUNTRY_NAME, "JP"),
            x509.NameAttribute(NameOID.ORGANIZATION_NAME, "Vloom dev"),
            x509.NameAttribute(NameOID.COMMON_NAME, "Vloom dev"),
        ]
    )

    san_entries: list[x509.GeneralName] = [
        x509.DNSName("localhost"),
        x509.IPAddress(ipaddress.IPv4Address("127.0.0.1")),
    ]
    for ip in ips:
        try:
            san_entries.append(x509.IPAddress(ipaddress.IPv4Address(ip)))
        except ValueError:
            san_entries.append(x509.DNSName(ip))

    cert = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(issuer)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(datetime.now(timezone.utc) - timedelta(minutes=1))
        .not_valid_after(datetime.now(timezone.utc) + timedelta(days=365))
        .add_extension(x509.SubjectAlternativeName(san_entries), critical=False)
        .sign(key, hashes.SHA256())
    )

    KEY_FILE.write_bytes(
        key.private_bytes(
            encoding=serialization.Encoding.PEM,
            format=serialization.PrivateFormat.PKCS8,
            encryption_algorithm=serialization.NoEncryption(),
        )
    )
    CERT_FILE.write_bytes(cert.public_bytes(serialization.Encoding.PEM))
    print(f"[cert] generated {CERT_FILE} (SAN: {', '.join(ips)})")


LOG_DIR = Path(__file__).parent / "client_logs"

# 送受信間の簡易バックチャネル (同一 LAN 前提の開発用ブラックボード)。
# 受信側が読み取り状況を POST /state/receiver、送信側が設定を POST /state/sender、
# 双方が GET /state で互いの状態を取得し、表示設定の助言・適応調整に使う。
# サーバはスレッド化 (ThreadingHTTPSServer) だが、_STATE はキー固定 ("sender"/"receiver") で
# 値まるごとの差し替えしかせず、GET も {**_STATE} でスナップショットを取るため GIL 下で安全。
_STATE: dict[str, dict] = {"sender": {}, "receiver": {}}
MAX_BODY = 16 * 1024 * 1024  # POST ボディ上限 (capture の PNG を許容しつつ暴走を防ぐ)


class Handler(http.server.SimpleHTTPRequestHandler):
    """no-cache + POST /log,/capture,/state/* + GET /state エンドポイントを提供する。"""

    def end_headers(self) -> None:
        # 開発用: ブラウザキャッシュ無効化
        self.send_header("Cache-Control", "no-store, no-cache, must-revalidate")
        # CORS (LAN 内で別 origin から呼ばれても OK にする)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        super().end_headers()

    def do_OPTIONS(self) -> None:  # noqa: N802
        self.send_response(204)
        self.end_headers()

    def log_message(self, fmt, *args) -> None:  # noqa: A003
        # 高頻度な /state ポーリングはログに出さない (本当に見たい log/capture/cert が埋もれるのを防ぐ)
        if self.path.startswith("/state"):
            return
        super().log_message(fmt, *args)

    def _content_length(self) -> int:
        try:
            n = int(self.headers.get("Content-Length") or 0)
        except (TypeError, ValueError):
            return 0
        return max(0, min(n, MAX_BODY))

    def do_GET(self) -> None:  # noqa: N802
        if self.path == "/state":
            body = json.dumps({**_STATE, "now": time.time()}).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(body)
            return
        super().do_GET()

    def do_POST(self) -> None:  # noqa: N802
        if self.path == "/log":
            self._handle_log()
        elif self.path == "/capture":
            self._handle_capture()
        elif self.path == "/state/sender":
            self._handle_state("sender")
        elif self.path == "/state/receiver":
            self._handle_state("receiver")
        else:
            self.send_error(404, "endpoint not found")

    def _handle_state(self, which: str) -> None:
        raw = self.rfile.read(self._content_length()).decode("utf-8", errors="replace")
        try:
            data = json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            data = {}
        if not isinstance(data, dict):  # 配列/スカラを POST されても落ちない
            data = {}
        data["_ts"] = time.time()
        _STATE[which] = data
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.end_headers()
        self.wfile.write(b"OK")

    def _handle_log(self) -> None:
        length = self._content_length()
        body = self.rfile.read(length).decode("utf-8", errors="replace")
        LOG_DIR.mkdir(exist_ok=True)
        timestamp = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S")
        log_path = LOG_DIR / f"log_{timestamp}_{self.client_address[0].replace('.', '_')}.txt"
        log_path.write_text(body, encoding="utf-8")
        print(f"[log] saved {log_path.name} ({len(body)} chars) from {self.client_address[0]}")
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.end_headers()
        self.wfile.write(b"OK")

    def _handle_capture(self) -> None:
        """キャプチャ画像 (PNG base64) を受け取って保存。クライアントが decode 失敗時に診断用に送る。"""
        length = self._content_length()
        body = self.rfile.read(length).decode("utf-8", errors="replace")
        LOG_DIR.mkdir(exist_ok=True)
        timestamp = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S")
        # body は data URL ("data:image/png;base64,...") を想定
        import base64
        try:
            prefix, b64 = body.split(",", 1)
            data = base64.b64decode(b64)
        except Exception as e:  # noqa: BLE001
            self.send_response(400)
            self.end_headers()
            self.wfile.write(f"bad data: {e}".encode())
            return
        out_path = LOG_DIR / f"capture_{timestamp}_{self.client_address[0].replace('.', '_')}.png"
        out_path.write_bytes(data)
        print(f"[capture] saved {out_path.name} ({len(data)} bytes) from {self.client_address[0]}")
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.end_headers()
        self.wfile.write(b"OK")


class ThreadingHTTPSServer(http.server.ThreadingHTTPServer):
    """スレッド化 HTTPS サーバ。

    スマホは HTTPS 接続を途中でブツッと切ることが日常的にあり (画面ロック・タブ離脱・
    Wi-Fi 瞬断)、そのたびに ConnectionResetError / SSLError が上がる。単一スレッドだと
    1 クライアントの切断待ちで他がブロックされ、切断トレースバックでコンソールも埋まる。
    スレッド化して並行処理し、切断系の例外はトレースバックを出さず握りつぶす。"""

    daemon_threads = True

    def handle_error(self, request, client_address) -> None:  # noqa: ANN001
        exc = sys.exc_info()[1]
        if isinstance(
            exc,
            (ConnectionResetError, ConnectionAbortedError, BrokenPipeError, ssl.SSLError, TimeoutError),
        ):
            return  # モバイルの接続切断は無視 (サーバは落とさない)
        super().handle_error(request, client_address)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8443)
    parser.add_argument(
        "--ip",
        action="append",
        default=[],
        help="証明書 SAN に追加する IP (複数指定可)。指定しなければ自動検出。",
    )
    args = parser.parse_args()

    detected_ips = get_local_ips()
    all_ips = list(set(args.ip + detected_ips))

    if not CERT_FILE.exists() or not KEY_FILE.exists():
        make_self_signed_cert(all_ips)
    else:
        print(f"[cert] reusing {CERT_FILE}")

    web_dir = Path(__file__).parent
    import os
    os.chdir(web_dir)
    print(f"[serve] root={web_dir}")

    server = ThreadingHTTPSServer(("0.0.0.0", args.port), Handler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(certfile=str(CERT_FILE), keyfile=str(KEY_FILE))
    server.socket = context.wrap_socket(server.socket, server_side=True)

    print(f"[serve] HTTPS listening on port {args.port}")
    for ip in all_ips:
        print(f"        https://{ip}:{args.port}/")
    print("        https://localhost:" + str(args.port) + "/")
    print("Press Ctrl+C to stop.")

    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\n[serve] shutting down")


if __name__ == "__main__":
    main()

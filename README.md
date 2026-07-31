# Vloom

**光だけでファイルを渡す。** ネットワークもペアリングも要らない。

片方の画面にコードの動画ループを表示し、もう片方のカメラで撮り続けるだけで、通信経路を
一切介さずにファイルが渡る。ペイロードを Rust の Fountain code (RaptorQ) でパケット列に
符号化し、各パケットをコードとして循環表示する。受信側は十分なパケットが集まった時点で
元データを復元するので、**1 枚も取りこぼさずに撮る必要はない**。フレーム落ちやカメラ位置に強い。

コード形式は 2 系統:

- **QR** — 標準形式。既存デコーダが使え、どの端末でも動く
- **vcode** — 独自の輝度 4 値ブロック格子。QR のファインダ/EC 領域を持たず格子全体をデータに
  使い、1 セルに 2bit を載せる。同じセル数で QR (v40/EC=L) の約 2.3 倍の情報量

名前の由来: bloom の b→v で **Vloom**。同時に *v-loom* = 織機 とも読め、光の断片を織り上げて
元のファイルに戻す動きを表す。

## ディレクトリ構成

```
fountain/    RaptorQ ラッパー (Fountain Encoder/Decoder)。符号化の中核
vcode/       独自コード形式 (輝度4値ブロック格子) のエンコーダとスキャナ
core-wasm/   Fountain + vcode を wasm-bindgen でブラウザへ公開 (→ web/pwa/pkg)
app/         Flutter アプリ (送受信兼用)。Rust を FFI ブリッジで共有
web/pwa/     PC 用 PWA。GitHub Pages で配信
web/         旧 Web 版 (sender/receiver.html) + 開発用 HTTPS サーバ
qr_bench/    QR 検出率ベンチ (Python / pyzbar)
```

実行時生成物 (いずれも git 管理外・再生成可): `target/` `web/pkg/` `web/certs/` `web/client_logs/`

## 必要環境

- **Rust** (stable) + `wasm-pack` … WASM ビルド用
  ```powershell
  cargo install wasm-pack
  ```
- **Python 3.10+** … 開発用 HTTPS サーバ (`web/serve_https.py`) と QR ベンチ用

## セットアップ

### 1. Python 仮想環境

```powershell
python -m venv .venv
.venv\Scripts\Activate.ps1
pip install cryptography          # HTTPS サーバの自己署名証明書生成に必須
pip install pyzbar qrcode pillow numpy opencv-python   # qr_bench を回す場合のみ
```

### 2. WASM ビルド (初回 / Rust 変更時)

```powershell
cd core-wasm
wasm-pack build --target web --release --out-dir ..\web\pkg
```

`web/pkg/` は git 管理外。クローン直後は必ずこのビルドが必要。

## 実機テスト (PC → スマホ)

PC とスマホを同じ Wi-Fi に接続し、HTTPS サーバを起動する:

```powershell
python web\serve_https.py
```

- PC ブラウザ: `https://localhost:8443/sender.html` で画像/テキストを送信
- スマホ Chrome: `https://<PC の IP>:8443/receiver.html` でカメラ受信
  (IP は起動時にコンソール表示。証明書警告は「詳細設定 → アクセスする」で許容)

カメラ不要の往復テストは `https://localhost:8443/test_phase1.html`。
手順・トラブルシューティングの詳細は **[web/README.md](web/README.md)** を参照。

## ビルド / テスト

```powershell
cargo test --manifest-path fountain/Cargo.toml   # Fountain コアの単体テスト
```

## 備考

- `serve_https.py` は開発専用 (自己署名証明書・no-cache・LAN 内限定)。本番用途には使わない。
  受信状況を送受信間で共有する `/state` バックチャネルや、診断用 `/log` `/capture`
  エンドポイントを持つ。
- 経緯: Phase 0 の 8 色カラーセル方式は実機のモアレ + 色 ISP で破綻し、Phase 1 で B/W の
  QR + Fountain 動画ループにピボットして実機転送に成功した。その後 QR の構造的な
  オーバーヘッドを外すため、独自形式 vcode (輝度 4 値ブロック格子) に主軸を移している。
- 旧称 `beyond-qr`。QR を前提とした名前を脱したため Vloom に改称した。

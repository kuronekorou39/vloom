# Vloom web (Phase 1: QR + Fountain code 動画ループ)

PC 画面に **QR の動画ループ** を表示し、**スマホ Chrome / Safari** のカメラで連続スキャンして
ファイル/テキストを復元する Web アプリ。Rust の Fountain code (RaptorQ) を WASM 化し、
QR 生成は qrcode-generator、QR 読み取りは jsQR (いずれも CDN) で行う。

仕組み: 送信側はペイロードを Fountain パケット列に符号化し、各パケットを
`[12 byte OTI][packet]` の QR にして循環表示する。受信側はカメラで QR を読み続け、
OTI でデコーダを初期化し、重複を除きつつパケットが十分集まった時点で元データを復元する。
カメラ位置やフレーム落ちに強く、1 枚も取りこぼさずに撮る必要はない。

## ファイル構成

```
web/
├── sender.html        # PC 側送信 UI (ペイロード → Fountain → QR 動画ループ)
├── receiver.html      # スマホ側受信 UI (カメラ連続スキャン → jsQR → Fountain 復元)
├── test_phase1.html   # PC 単体の往復テスト (カメラ不要: encode→QR→jsQR→decode)
├── pkg/               # wasm-pack 出力 (core-wasm。要ビルド、git 管理外)
├── certs/             # 自己署名証明書 (初回起動時に自動生成、git 管理外)
├── serve_https.py     # 開発用 HTTPS サーバー
├── manifest.json      # PWA マニフェスト (start_url = receiver.html)
└── README.md          # 本ファイル
```

## セットアップ手順

### 1. WASM ビルド (初回 / Rust 変更時)

```powershell
cd core-wasm   # リポジトリルートから
wasm-pack build --target web --release --out-dir ..\web\pkg
```

### 2. ファイアウォール許可 (初回のみ、管理者 PowerShell)

```powershell
New-NetFirewallRule -DisplayName "Vloom HTTPS 8443" -Direction Inbound -Protocol TCP -LocalPort 8443 -Action Allow -Profile Private
```

(または Python の最初の起動時に Windows のポップアップで「アクセスを許可」を選ぶ)

### 3. HTTPS サーバー起動

```powershell
python web\serve_https.py
```

初回起動時に `web/certs/` 配下へ自己署名証明書を自動生成する (LAN IP を SAN に注入)。
カメラ API (getUserMedia) は HTTPS が必須のため、平文 HTTP では動かない。

### 4. PC 側で送信

PC ブラウザで `https://localhost:8443/sender.html` を開く:

1. テキストを入力、またはファイルを選択
2. (任意) FPS / EC レベル / QR バージョンを調整
3. 「送信開始」→ QR が動画ループ表示される
4. F11 で全画面化 (白背景なので画像周りの quiet zone が自動確保される)

### 5. スマホで受信

スマホを **PC と同じ Wi-Fi に接続**し、スマホ Chrome で開く:

```
https://<PC の IP>:8443/receiver.html
```

(IP はサーバー起動時にコンソールに表示される。例: `https://192.168.11.52:8443/`)

**証明書警告**: 「詳細設定」→「<host> にアクセスする (安全ではありません)」を選択。
自己署名証明書なので警告が出るのは正常。

1. 「カメラ開始」をタップ → カメラ許可 (背面カメラ)
2. PC 画面の QR 動画にスマホをかざす
3. パケットが集まると自動で復元され、画像/テキスト/hex プレビューと
   ダウンロードリンクが表示される

## 動作確認 (カメラ不要)

`https://localhost:8443/test_phase1.html` を PC ブラウザで開くと、
encode→QR→jsQR→decode の往復を画面内で検証できる。

## トラブルシューティング

- **カメラ起動エラー** → HTTPS 経由か確認 (HTTP では getUserMedia 不可)
- **WASM 読み込み失敗** → `pkg/` が web/ 下に正しくビルドされているか確認 (手順 1)
- **なかなか復元しない** → QR バージョンを下げる / FPS を下げる / 撮影距離・手ぶれ・反射を調整、画面のオートブライトネスを切る
- **接続できない** → ファイアウォール許可、PC と同じ Wi-Fi、IP アドレスが合っているか

## 補足

- `serve_https.py` は開発専用 (CORS 全開放・0.0.0.0 待受・no-cache)。本番用途には使わない。
- `POST /log`・`POST /capture` の診断エンドポイントを持ち、受信側のログ/キャプチャを
  `web/client_logs/` (git 管理外) に保存できる。

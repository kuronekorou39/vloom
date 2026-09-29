# Vloom — Claude Code 向けガイド

## このリポジトリは何か

**光だけでファイルを渡す**アプリ。ネットワークもペアリングも使わず、送信側の画面に
コードの動画ループを表示し、受信側のカメラで撮り続けるだけでファイルを転送する。

- 符号化: Rust の Fountain code (RaptorQ) でパケット化 → 独自コード形式 **vcode**
  (輝度ブロック格子) として循環表示
- Rust コアは **WASM (PWA) と FFI (Flutter) の両方から同じ実装を共有**する。
  ブラウザ版・ネイティブ版・送受信で符号化/復号のロジックを分岐させない設計
- 詳しい仕組み・実測値は [README.md](README.md)、技術選定の経緯は
  [docs/tech_stack.md](docs/tech_stack.md)

## ディレクトリ構成

```
fountain/    RaptorQ ラッパー (Fountain Encoder/Decoder)。符号化の中核
vcode/       独自コード形式のエンコーダとスキャナ (ホモグラフィ推定・部分回収)
core-wasm/   Fountain + vcode を wasm-bindgen でブラウザへ公開
app/         Flutter アプリ (送受信兼用)。Rust を FFI ブリッジで共有
py/          Rust コアの Python バインディング (PyO3 / maturin)
desktop/     Windows 送信アプリ (PySide6)。py/ 経由で Rust コアを呼ぶ
web/pwa/     ブラウザ版 + 開発用 HTTPS サーバ
tools/       vcode エンコーダ (Python 標準ライブラリのみ) 等の補助スクリプト
docs/        技術スタック・形式仕様・開発ガイド・効果検証の記録
licenses/    サードパーティのライセンス全文
```

`app/` (Flutter, `cargo` workspace 除外) と `py/` (PyO3, 同じく除外) は
`Cargo.toml` の `[workspace]` に含まれない。理由はそれぞれの `Cargo.toml` の
コメントを参照。

## よく使うコマンド

```bash
cargo test --workspace          # Rust コア (fountain / vcode) のテスト

cd core-wasm                    # PWA 用 WASM ビルド (web/pwa/pkg/ は git 管理外)
wasm-pack build --target web --release --out-dir ../web/pwa/pkg

cd app && flutter pub get && flutter build apk --release   # Android

uv sync                                   # Python: 素の環境 (tools/vcode_encode.py 用)
uv sync --group desktop --reinstall-package vloom-core     # デスクトップアプリ (要 Rust)
uv sync --group dev                       # 開発スクリプト (計測・PWA自動テスト等)

uv run --group dev python web/pwa/serve_https.py            # PWA を実機で試す (HTTPS 必須)
uv run --group dev python tools/test_pwa_receive.py          # PWA 受信の自動回帰テスト
```

手順の詳細・実機検証・トラブルシュートは [docs/development.md](docs/development.md)
に一次情報がある。特に以下は踏みやすい罠として明記されているので、該当作業の前に読むこと。

- Rust コア (`vcode/` / `fountain/`) を変更したら `py/` 側は
  `uv sync --group desktop --reinstall-package vloom-core` を **必ず** 実行する
  (`uv run --reinstall-package` では不十分。キャッシュが無効化されず古いホイールが残る)
- PWA は Service Worker でキャッシュ優先なので、配信物を更新したら
  `web/pwa/sw.js` の `CACHE` バージョンを上げる
- `web/pwa/app.js` 等の構文チェックは `node --check` の前に `.mjs` へコピーする
  (CommonJS 判定だと壊れた文字列を素通りさせることがある)

## 作業のルール

- ドキュメント・コミットメッセージ・コード内コメントは日本語 (既存の README /
  docs/*.md の慣習に合わせる)
- Rust コアの実装は WASM 版・ネイティブ版で分岐させない。プラットフォーム差分が
  必要な場合は呼び出し側 (core-wasm の bindings 層 / Flutter 側) で吸収する
- 実験・計測 (格子サイズ、fps、露出などのパラメータ探索) を行ったら、結果を
  [docs/experiments.md](docs/experiments.md) や `docs/measurements/` に倣った形で
  残す。単発の思いつきで既定値を変えない
- ライセンス: PySide6 (LGPL-3.0, `desktop/` のみ) 以外はコピーレフト依存を持ち込まない。
  新規依存を足すときは [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) との整合を確認する
- `assets/`, `samples/`, `scratch/` 配下や生成画像 (`Gemini_Generated_Image_*.png` など)
  はスクラッチ/検証用。恒久的な成果物は該当ディレクトリ (`docs/images/` 等) に置く

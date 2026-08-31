# Vloom

**光だけでファイルを渡す。** ネットワークもペアリングも要らない。

片方の画面にコードの動画ループを表示し、もう片方のカメラで撮り続けるだけで、通信経路を
一切介さずにファイルが渡ります。Wi-Fi も Bluetooth も、相手の連絡先も不要です。

## 試す

| 方法 | 対象 | 備考 |
|---|---|---|
| [**Web で開く**](https://kuronekorou39.github.io/vloom/) | どの端末でも | インストール不要。まずここから |
| [Releases の APK](../../releases) | Android | ネイティブの方が高速。カメラ制御が細かい |
| ソースからビルド | iOS | Mac + Xcode が必要 ([手順](docs/development.md#ios-について)) |
| `desktop/` を実行 | Windows (送信のみ) | フレーム間隔を正確に刻める ([手順](docs/development.md#デスクトップ送信アプリ-windows)) |

**使い方**: 送信側の画面いっぱいにコードを表示し (輝度は最大)、受信側のカメラを向けて
ガイド枠に収めるだけ。格子 (プリセット) は送受信で同じものを選びます。受信画面が
充填率・余白・px/セル を読み上げ、「近づけて / 離して」も誘導します。

## 仕組み

ペイロードを Rust の Fountain code (RaptorQ) でパケット列に符号化し、各パケットをコードとして
循環表示します。受信側は十分なパケットが集まった時点で復元するので、**1 枚も取りこぼさずに
撮る必要がありません**。フレーム落ち・手ブレ・位置ズレに強く、送受信の fps を合わせる必要も
ありません。

コード形式は **vcode** — 動画ネイティブな独自の輝度ブロック格子です。QR のファインダ・
フォーマット情報・EC 領域を持たず、格子全体をデータに使います。1 セルあたりの情報量は
QR とほぼ同じですが、**ブロック単位の CRC による部分回収 + fountain 符号**により、
フレームの一部が潰れても読めたブロックだけを積み上げられます。QR は 1 枚が完結した
単位なので、読めなければその 1 枚が丸ごと無駄になる — 実効スループットの差はここから
出ます。形式の仕様は [docs/vcode_format.md](docs/vcode_format.md)。

**実測 (1MB 転送、Pixel 9a 受信、13×18 / 1bit)**:

| 送信側 | 実効 |
|---|---|
| BenQ 平面モニタ (LCD) @20fps | 131〜147 KB/s |
| iPhone 12 Pro (OLED) @30fps | **201〜207 KB/s** |

OLED は表示の切り替わりが速く、ローリングシャッターと混ざらないぶん fps を上げられます
(60Hz パネルの上限は 30fps)。何を試してこの数字に至ったかの通史は
[docs/experiments.md](docs/experiments.md)、全試行の生ログは
[docs/measurements/](docs/measurements/) にあります。

制約と目安:

- **送信面は平面ディスプレイ**であること (曲面は四隅からの射影変換が内側で外れて壊滅)
- 密な格子ほど速いが、カメラ解像度が要る (安定の目安 3px/セル以上、2bit は 6px/セル)
- 1 フレームは画面の 2 リフレッシュ周期以上表示する (60Hz → 30fps まで)
- 短いファイルの計測値は瞬間風速。定常は 1MB 以上・端末が冷えた状態で測る

## 開発

```bash
cargo test --workspace                                       # Rust コア
cd core-wasm && wasm-pack build --target web --release --out-dir ../web/pwa/pkg   # PWA
cd app && flutter build apk --release                        # Android
```

ビルド詳細・uv の注意点・実機検証 (HTTPS サーバー / 計測用 Intent / 調査ツール) は
[docs/development.md](docs/development.md) にまとめてあります。

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

Rust コアは WASM と FFI の両方から使うので、ブラウザ版・ネイティブ版・送受信で符号化/復号の
実装が分岐しません。構成の全体像と選定理由は [docs/tech_stack.md](docs/tech_stack.md)。

## ドキュメント

- [docs/development.md](docs/development.md) — ビルド・実行・実機検証の手順
- [docs/vcode_format.md](docs/vcode_format.md) — vcode フレームフォーマット仕様
- [docs/tech_stack.md](docs/tech_stack.md) — 技術選定の理由と経緯 (QR 経路の削除など)
- [docs/experiments.md](docs/experiments.md) — 効果検証の時系列まとめ
- [docs/measurements/](docs/measurements/) — 日別の全試行ログ

## ライセンス

[MIT](LICENSE)。同梱するサードパーティの表記は
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) にあります。コピーレフトの依存は
デスクトップアプリの PySide6 (LGPL-3.0) のみで、PWA・モバイルアプリ・Rust コアには
ありません。

# Vloom

**光だけでファイルを渡す。** ネットワークもペアリングも要らない。

片方の画面にコードの動画ループを表示し、もう片方のカメラで撮り続けるだけで、通信経路を
一切介さずにファイルが渡ります。Wi-Fi も Bluetooth も、相手の連絡先も不要です。

> 名前の由来: bloom の b→v で **Vloom**。同時に *v-loom* = 織機 とも読め、光の断片を
> 織り上げて元のファイルに戻す動きを表しています。

## 試す

| 方法 | 対象 | 備考 |
|---|---|---|
| [**Web で開く**](https://kuronekorou39.github.io/vloom/) | どの端末でも | インストール不要。まずここから |
| [Releases の APK](../../releases) | Android | ネイティブの方が高速。カメラ制御が細かい |
| ソースからビルド | iOS | Mac + Xcode が必要 (後述) |

## 仕組み

ペイロードを Rust の Fountain code (RaptorQ) でパケット列に符号化し、各パケットをコードとして
循環表示します。受信側は十分なパケットが集まった時点で元データを復元するので、**1 枚も
取りこぼさずに撮る必要がありません**。フレーム落ち・手ブレ・カメラ位置のズレに強く、
送受信のフレームレートを合わせる必要もありません。

一方向の光チャネルには再送要求ができません。順番に送って取りこぼしを待つ方式だと、1 枚
落とすたびに一巡分待つことになります。Fountain code はこれを構造的に解決します。

コード形式は 2 系統あります。

**QR** — 標準形式。既存のデコーダが使え、どの端末でも動きます。

**vcode** — 独自の輝度 4 値ブロック格子。QR のファインダパターン・アライメント・フォーマット
情報・誤り訂正領域を持たず、格子全体をデータに使い、1 セルに 2bit を載せます。

| | セル数 | データ量 | 効率 |
|---|---|---|---|
| QR v40 / EC=L | 177×177 = 31,329 | 2,953 B | 0.75 bit/セル |
| vcode 9×8 / 2bit | 180×172 = 30,960 | 6,624 B | **1.71 bit/セル** |

ほぼ同じセル数で **2.27 倍**の情報量です。この差は QR の構造に由来するので、QR を使う限り
埋まりません。加えて vcode はブロック単位の CRC で**部分回収**ができ、フレームの一部が
潰れても読めたブロックだけを回収します。

実測は QR 経路で 200KB を約 45 秒 (4.4 KB/s)。vcode の高密度格子は理論値のみで、実機計測は
これからです (9×8 / 30fps で理論 194 KB/s)。

## 使い方

送信側の画面いっぱいにコードを表示し、受信側のカメラを向けます。**受信側のガイド枠に
コードを収める**のがコツです。送信側の画面輝度は最大にしてください。

vcode は格子 (5×4 / 7×6 / 9×8 / 11×10) と階調 (1bit / 2bit)、fps を選べます。密な格子ほど
速い代わりに高いカメラ解像度が要ります。**6 px/セル** が安定の目安で、4 px を切ると輝度 4 値は
まず復号できません。受信画面に実測の px/セル が出るので、それを見て調整してください。

1 フレームはディスプレイの 2 リフレッシュ周期以上表示する必要があります。60Hz 画面なら
30fps、120Hz 画面なら 60fps が上限の目安です。

## 開発

### 必要環境

- **Rust** (stable) + `wasm-pack` — `cargo install wasm-pack`
- **Flutter** 3.41+ — ネイティブアプリをビルドする場合
- **Python 3.10+** — 開発用 HTTPS サーバと QR ベンチ

### ビルドとテスト

```bash
cargo test --workspace          # Rust コア (fountain / vcode スキャナ)

cd core-wasm                    # PWA 用の WASM
wasm-pack build --target web --release --out-dir ../web/pwa/pkg

cd app                          # ネイティブアプリ
flutter pub get
flutter build apk --release     # Android
```

`web/pwa/pkg/` は git 管理外なので、クローン直後は必ず WASM ビルドが要ります。

### PWA を実機で試す

カメラ API (`getUserMedia`) は HTTPS または localhost でしか動きません。スマホから開くには
HTTPS が必要なので、自己署名証明書付きの開発サーバーを同梱しています。

```bash
pip install cryptography
python web/pwa/serve_https.py
```

PC とスマホを同じ Wi-Fi に繋ぎ、表示された `https://<PC の IP>:8443/` を開きます。証明書警告は
「詳細設定 → アクセスする」で許容してください。開発専用なので本番用途には使わないでください。

### iOS について

**iOS 版のバイナリは配布していません。** Apple の制約により、署名なしの IPA は受け取った側で
自分の Apple ID による再署名が必要で、無料アカウントでは 7 日で失効するためです。

Mac をお持ちなら、自分でビルドしてインストールできます。

```bash
cd app
open ios/Runner.xcworkspace
```

Xcode の Signing & Capabilities で自分の Apple ID (Personal Team) を選び、Bundle Identifier を
自分固有の値に変更してから Run してください。Rust の iOS ターゲットが必要です
(`rustup target add aarch64-apple-ios`)。

無料の Apple ID では証明書が 7 日で失効するため、常用するなら Apple Developer Program
($99/年) が要ります。**常用目的なら PWA をお勧めします。**

## ディレクトリ構成

```
fountain/    RaptorQ ラッパー (Fountain Encoder/Decoder)。符号化の中核
vcode/       独自コード形式のエンコーダとスキャナ (ホモグラフィ推定・部分回収)
core-wasm/   Fountain + vcode を wasm-bindgen でブラウザへ公開
app/         Flutter アプリ (送受信兼用)。Rust を FFI ブリッジで共有
web/pwa/     ブラウザ版 + 開発用 HTTPS サーバ
qr_bench/    QR 検出率ベンチ (Python / pyzbar)
docs/        形式仕様と関連研究の調査
```

Rust コアは WASM と FFI の両方から使うので、**ブラウザ版とネイティブ版で符号化・復号の実装が
分岐しません**。送受信で実装差が出ないことは、この種のプロジェクトでは効きます。

## 経緯

Phase 0 では 8 色のカラーセルで情報密度を稼ごうとしましたが、実機のモアレとカメラの色 ISP に
よる補正で破綻しました。Phase 1 で白黒の QR + Fountain code + 動画ループにピボットし、実機での
ファイル転送に成功。その後、QR の構造的なオーバーヘッド (ファインダ・EC 領域) を外すため、
独自形式 vcode に主軸を移しています。

旧称 `beyond-qr`。QR を前提とした名前を脱したため改称しました。

## ライセンス

[MIT](LICENSE)

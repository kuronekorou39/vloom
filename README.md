# Vloom

**光だけでファイルを渡す。** ネットワークもペアリングも要らない。

片方の画面にコードの動画ループを表示し、もう片方のカメラで撮り続けるだけで、通信経路を
一切介さずにファイルが渡ります。Wi-Fi も Bluetooth も、相手の連絡先も不要です。

## 試す

| 方法 | 対象 | 備考 |
|---|---|---|
| [**Web で開く**](https://kuronekorou39.github.io/vloom/) | どの端末でも | インストール不要。まずここから |
| [Releases の APK](../../releases) | Android | ネイティブの方が高速。カメラ制御が細かい |
| ソースからビルド | iOS | Mac + Xcode が必要 (後述) |
| `desktop/` を実行 | Windows (送信のみ) | フレーム間隔を正確に刻める。後述 |

## 仕組み

ペイロードを Rust の Fountain code (RaptorQ) でパケット列に符号化し、各パケットをコードとして
循環表示します。受信側は十分なパケットが集まった時点で復元するので、**1 枚も取りこぼさずに
撮る必要がありません**。フレーム落ち・手ブレ・位置ズレに強く、送受信の fps を合わせる必要も
ありません。一方向の光チャネルは再送要求ができないため、順送り方式では 1 枚落とすたびに一巡
待つことになりますが、Fountain code はこれを構造的に解決します。

コード形式は **vcode** — 動画ネイティブな独自の輝度ブロック格子です。QR のファインダ・
アライメント・フォーマット情報・EC 領域を持たず、格子全体をデータに使い、1 セルに 2bit を
載せます。

| | セル数 | データ量 | 効率 |
|---|---|---|---|
| QR v40 / EC=L | 177×177 = 31,329 | 2,953 B | 0.75 bit/セル |
| vcode 9×8 / 1bit | 180×172 = 30,960 | 3,024 B | 0.78 bit/セル |
| vcode 9×8 / 2bit | 180×172 = 30,960 | 6,624 B | 1.71 bit/セル |

1 セルあたりの情報量そのものは、実運用している 1bit では QR とほぼ同じです
(2bit なら 2.27 倍ですが、実機では 1bit のほうが速い — 後述)。vcode が効くのは
1 枚あたりの容量ではなく**枚数の使い方**で、ブロック単位の CRC による部分回収と
fountain 符号により、フレームの一部が潰れても読めたブロックだけを積み上げられます。
QR は 1 枚が完結した単位なので、読めなければその 1 枚が丸ごと無駄になります。
実効スループットの差はここから出ます。

実機実測は **約 60 KB/s** (11×10 / 1bit / 20fps / 受信 1600×1200、BenQ 平面モニタを
三脚から約 50cm で撮影、Pixel 9a)。格子を落とすと素直に下がり、9×8 で約 40、7×6 で
約 22、5×4 で約 10 KB/s です。端末が温まるとスキャン速度が落ちて 2 倍以上変わるので、
その日の最初の 1 回を代表値にしないこと。詳細は
[docs/vcode_format.md](docs/vcode_format.md)。

**送信側は平面ディスプレイを使ってください。** 位置合わせは四隅から射影変換を起こす
ので、面が曲がっていると内側のサンプリングがずれ、コードは検出できるのにデータが
まったく取れません (曲面では 5 KB/s まで落ちました)。斜めから撮るのは問題ありません。

送信 fps の上限は受信側のカメラで決まります。実効フレームレートが 23〜25fps なので、
送信 20fps までは 1 枚ずつ拾えますが、30fps にすると 1 枚の写真に 2 フレームが混ざり
(ローリングシャッター + LCD の残像)、回収率が落ちて逆に遅くなります。容量は fps
ではなくセル数 (格子) で稼ぐ方針です。

輝度 4 値 (2bit) は 1 枚あたりの容量こそ 2.2 倍ですが、実機では同じ格子の 1bit より
遅くなります。レベル間のマージンが 1/3 になり、400 セルすべてが合って初めて回収できる
ブロックの成功率が落ちるためです (4 値の満点ブロック率 0〜10% に対し 1bit は 50〜95%)。
復号自体は通ります。

v0.3 まで併走させていた QR 経路 (6.7 KB/s) は、比較対象としての役目を終えたので v0.4 で
削除しました。経緯と得失は [docs/tech_stack.md](docs/tech_stack.md) に残しています。

## 使い方

送信側の画面いっぱいにコードを表示し、受信側のカメラを向けます。**受信側のガイド枠にコードを
収める**のがコツです。送信側の画面輝度は最大にしてください。

vcode は格子 (5×4 / 7×6 / 9×8 / 11×10) と階調 (1bit / 2bit)、fps を選べます。密な格子ほど速い
代わりに高いカメラ解像度が要ります。**6 px/セル** が安定の目安で、4 px を切ると輝度 4 値はまず
復号できません。受信画面に実測の px/セルが出るので、それを見て調整してください。

1 フレームはディスプレイの 2 リフレッシュ周期以上表示する必要があります。60Hz 画面なら 30fps、
120Hz 画面なら 60fps が上限の目安です。

## 開発

### 必要環境

- **Rust** (stable) + `wasm-pack` — `cargo install wasm-pack`
- **Flutter** 3.41+ — ネイティブアプリをビルドする場合
- **[uv](https://docs.astral.sh/uv/)** — Python 側 (デスクトップアプリ・開発スクリプト) の環境管理

### ビルドとテスト

```bash
cargo test --workspace          # Rust コア (fountain / vcode スキャナ)

cd core-wasm                    # PWA 用の WASM
wasm-pack build --target web --release --out-dir ../web/pwa/pkg

cd app                          # ネイティブアプリ
flutter pub get
flutter build apk --release     # Android
```

リリース鍵 (`app/android/key.properties`) を持たない環境では debug 鍵にフォールバックするため、
配布版が入っている端末には署名不一致で上書きインストールできません。計測用に横へ入れたい
ときは、別パッケージになるサフィックスを付けてください。

```bash
flutter build apk --release -PappIdSuffix=.lab   # app.vloom.vloom.lab / ランチャー名 "Vloom (lab)"
```

配布版と受信履歴を残したまま併存します。サフィックス未指定なら通常どおりです。

`web/pwa/pkg/` は git 管理外なので、クローン直後は必ず WASM ビルドが要ります。

### Python 環境 (uv)

Python 側は uv で管理しています。用途ごとに依存グループを分けてあり、既定では何も
入りません (`tools/vcode_encode.py` は標準ライブラリだけで動くため)。

```bash
uv sync                                   # 素の環境 (CLI エンコーダ用)
uv sync --group desktop                   # デスクトップ送信アプリ (要 Rust)
uv sync --group dev                       # 開発スクリプト (Pillow / cryptography)
uv sync --group desktop --group dev       # 両方
```

`--group desktop` は `py/` の PyO3 クレートを maturin でビルドするので Rust が要ります。
**Rust コア (`vcode/` や `fountain/`) を変更したときは `--reinstall-package vloom-core` を
付けてください。** uv のキャッシュキーは `py/` の内容だけを見るため、path 依存の先が
変わってもキャッシュが無効化されず、古いホイールがそのまま入ります。

```bash
uv sync --group desktop --reinstall-package vloom-core
```

`uv run --reinstall-package ...` では足りません。site-packages から消しただけでも
uv はビルドキャッシュから古いホイールを復元するので、**`uv sync` に付ける**こと
(実際にこれで、格子の上限を変えた `.pyd` が反映されず、指定した格子と違うものが
出力される状態にしばらく気づけませんでした)。

`uv sync` は宣言どおりの状態に**揃える**ので、グループを指定し直すと前のグループの
パッケージは消えます。両方要るときは同時に指定してください。

既に `.venv` がある場合、`uv sync` はそこに対して同期します。宣言に無いパッケージは
削除されるので、別環境で試すなら `UV_PROJECT_ENVIRONMENT` を指定してください。

### PWA を実機で試す

カメラ API (`getUserMedia`) は HTTPS または localhost でしか動きません。スマホから開くには HTTPS が
必要なので、自己署名証明書付きの開発サーバーを同梱しています。

```bash
uv run --group dev python web/pwa/serve_https.py
```

PC とスマホを同じ Wi-Fi に繋ぎ、表示された `https://<PC の IP>:8443/` を開きます。証明書警告は
「詳細設定 → アクセスする」で許容してください。開発専用です。

### デスクトップ送信アプリ (Windows)

PC を送信機として使うだけなら、ブラウザより表示タイミングを正確に刻める PySide6 版が
あります。符号化は Rust コアをそのまま呼ぶので、出るフレームは PWA と同一です。

```bash
uv sync --group desktop     # PySide6 + Rust コア (py/) をビルドして入れる
uv run python -m desktop
```

uv を使わない場合は `pip install pyside6 maturin && maturin develop --release -m py/Cargo.toml`
でも同じ状態になります。

ファイルを選んで「送信開始」で送信ウィンドウが開きます。通常のウィンドウなので、
受信側を構えながらサイズや位置を動かせます。コードはアスペクト比を保ってフィット
するので、窓を広げるほどセルが大きくなります。輝度とスムージングは表示中にその場で
調整でき、Esc で閉じます。

**窓の中でのコードの位置と大きさも変えられます。** 送信中に矢印キーで動かし、
`+` / `-` で拡縮、`0` で戻ります (Shift で粗く動きます)。カメラを三脚に据えている
とき、窓ごと動かすと背景のデスクトップまで変わって検出の条件が動いてしまうので、
白い面は据えたままコードだけを動かせるようにしてあります。現在値は下部バーに
`--zoom 0.80 --dx +0.050 --dy -0.030` の形で出るので、そのままコマンドラインに
渡せば同じ構図を再現できます。

受信は入れていません (PC の内蔵カメラは 720p 級が多く、vcode の目安 6px/セル に
届かないため)。スマホの「受信」を向けてください。

**PySide6 は LGPL-3.0** です。Vloom で唯一のコピーレフト依存で、このアプリにのみ
関係します。exe 化する場合の注意は [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) に
書いています。

### コマンドラインから vcode を作る

ブラウザやアプリを開かずに、ファイルを vcode のフレーム列へ変換できます。Python の標準
ライブラリだけで動くので追加インストールは要りません。

```bash
uv run python tools/vcode_encode.py photo.jpg --grid 9x8 --bpc 2 --fps 15
```

標準ライブラリだけで動くので、uv を通さず `python tools/vcode_encode.py ...` でも構いません。

`photo_vcode/` に PNG のフレーム列と `index.html` が出ます。HTML をブラウザで開き、クリックで
全画面にすればそのまま「受信」で受け取れます。送信側の `VcodeTx` とバイト単位で同じ
フレームを吐くので、受信側の変更は要りません。

RaptorQ は source パケットのみを生成します (リペア無し)。RFC 6330 の中間シンボル生成は純
Python には重すぎるためで、取りこぼしたブロックは次の周回で拾うことになります。リペアの
損失耐性が要る場面では PWA かアプリを使ってください。

### iOS について

iOS 版のバイナリは配布していません。署名なしの IPA は受け取った側での再署名が必要で、無料
アカウントでは 7 日で失効するためです。Mac があれば自分でビルドできます。

```bash
cd app
open ios/Runner.xcworkspace
```

Signing & Capabilities で自分の Apple ID (Personal Team) を選び、Bundle Identifier を固有値に
変更してから Run してください。Rust の iOS ターゲットが必要です
(`rustup target add aarch64-apple-ios`)。無料アカウントは 7 日で失効するため、常用なら PWA を
お勧めします。

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
docs/        技術スタック・形式仕様・関連研究の調査
licenses/    サードパーティのライセンス全文
```

Rust コアは WASM と FFI の両方から使うので、ブラウザ版・ネイティブ版・送受信で符号化/復号の
実装が分岐しません。構成の全体像と選定理由は [docs/tech_stack.md](docs/tech_stack.md)。

## ライセンス

[MIT](LICENSE)。同梱するサードパーティの表記は
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) にあります。コピーレフトの依存は
デスクトップアプリの PySide6 (LGPL-3.0) のみで、PWA・モバイルアプリ・Rust コアには
ありません。

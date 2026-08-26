# 技術スタック

Vloom は「画面 → カメラ」の一方向の光路だけでファイルを渡す。ネットワークもペアリングも
介さない。この制約が構成のほぼすべてを決めている。

- 符号化・復号のロジックは **Rust に一本化**し、WASM (ブラウザ) と FFI (Flutter) の両方から
  同じコードを呼ぶ。ブラウザ版とネイティブ版で実装が分岐すると、片方だけ復号できない
  フレームが生まれる。ここは分岐させられない。
- 送受信の形式は **vcode 一本**。QR 経路は v0.4 で削除した (経緯は末尾)。
- 依存は最小限に保つ。増えるほど「通信しない」ことの検証コストが上がる。

## 全体像

```
                 ┌──────────────────────────────────────────┐
                 │  Rust コア (符号化/復号のすべて)              │
                 │                                          │
                 │  fountain/  RaptorQ ラッパー               │
                 │  vcode/     フレーム形式 + カメラ用スキャナ    │
                 └───────┬──────────────────────┬───────────┘
                         │ wasm-bindgen         │ flutter_rust_bridge
                 ┌───────┴────────┐     ┌───────┴────────┐
                 │ core-wasm/     │     │ app/rust/      │
                 │ → web/pwa/pkg  │     │ → cdylib       │
                 └───────┬────────┘     └───────┬────────┘
                         │                      │
                 ┌───────┴────────┐     ┌───────┴────────┐
                 │ web/pwa/       │     │ app/ (Flutter) │
                 │ 素の ES modules │     │ Android / iOS  │
                 └────────────────┘     └────────────────┘

                         │ PyO3 (maturin)
                 ┌───────┴────────┐
                 │ py/            │
                 │ → vloom_core   │
                 └───────┬────────┘
                         │
                 ┌───────┴────────┐
                 │ desktop/       │
                 │ PySide6 (送信)  │
                 └────────────────┘

                 tools/vcode_encode.py  Python 標準ライブラリのみの
                                        オフライン送信側エンコーダ (開発用)
```

## Rust コア

符号化・復号の全体がここにある。`cargo test --workspace` がこのプロジェクトの一次防衛線。

| クレート | 役割 |
|---|---|
| `fountain/` | RaptorQ (RFC 6330) の薄いラッパー。パケット列の生成と復元 |
| `vcode/` | vcode のフレーム形式 (`lib.rs`) と実カメラ画像用スキャナ (`scan.rs`) |
| `core-wasm/` | 上 2 つを wasm-bindgen でブラウザへ公開 |
| `app/rust/` | 同じく flutter_rust_bridge で Flutter へ公開 |
| `py/` | 同じく PyO3 で Python へ公開 (`vloom_core`)。デスクトップ送信アプリが使う |

`app/` と `py/` はワークスペースから `exclude` している。前者は cargokit、後者は maturin が
独立にビルドするため。どちらも path 依存でコアを参照するので実装は分岐しない。

### なぜ Fountain code (RaptorQ) か

光路は一方向で、受信側から「そのフレームをもう一度」と頼めない。順送りだと 1 枚
取りこぼすたびに一巡待つことになる。Fountain code は「十分な数のパケットが集まれば
順不同で復元できる」ので、この待ちが構造的に消える。詳細は [vcode_format.md](vcode_format.md)。

`raptorq` クレートは default-features off (`std` 無し) で使っている。

### なぜ独自コード形式 (vcode) か

QR はファインダ・アライメント・フォーマット情報・EC 領域に面積を取られ、さらに
「1 フレーム = 成功か全損か」になる。vcode は格子全体をデータに使い、ブロック単位の
CRC-32 で**部分回収**する。同程度のセル数で実効 2.27 倍、実機で QR 経路の 4 倍。

## PWA (`web/pwa/`)

ビルドツールなし。素の ES modules を GitHub Pages がそのまま配信する。バンドラを入れると
依存とビルド手順が増えるが、このサイズでは見合わない。

| ファイル | 役割 |
|---|---|
| `app.js` | UI の結線のみ |
| `vcode.js` | 送信 (フレーム循環表示) / 受信 (カメラ → 輝度 → スキャン) |
| `camera.js` | カメラ選択・露出制御・スキャン統計 |
| `calibration.js` | 校正モード (どの密度まで読めるか確認) |
| `sw.js` | Service Worker。初回以降はオフラインで動く |
| `pkg/` | `wasm-pack` の出力 (git 管理外。CI でビルド) |

## アプリ (`app/`, Flutter)

| パッケージ | 用途 |
|---|---|
| `camera` | 受信。生 YUV フレームをそのまま Rust スキャナへ渡す |
| `wakelock_plus` / `screen_brightness` | 送信中の画面消灯防止・輝度最大化 |
| `image_picker` / `file_selector` | 送るファイルの選択 |
| `path_provider` / `flutter_file_dialog` / `share_plus` / `open_filex` | 受信ファイルの保存・共有・表示 |
| `flutter_rust_bridge` | Rust コアの FFI ブリッジ |

## デスクトップ送信アプリ (`desktop/`, PySide6)

Windows で PC を送信機として使うためのネイティブアプリ。ブラウザ版に対する利点は
**フレーム表示間隔を自分で制御できる**こと。1 フレームはディスプレイの 2 リフレッシュ周期
以上表示する必要があるが、ブラウザの `setTimeout` は数十 ms 単位でぶれる。ここでは
Qt の `PreciseTimer` を使い、デッドラインを積み上げてドリフトを消している。

符号化は `vloom_core` (= Rust コア) を呼ぶだけで、Python 側にエンコーダを持たない。
リペアパケットも PWA と同じ 50% が既定で、吐くフレームは PWA・モバイルアプリと
バイト単位で一致する。

| ファイル | 役割 |
|---|---|
| `app.py` | 設定画面 (ファイル/テキスト・格子・階調・fps・リペア率・表示先モニタ) |
| `sender.py` | 送信ウィンドウ。フレームループ、輝度調整、スムージング切替 |

起動は `uv sync --group desktop` の後 `uv run python -m desktop`。

送信ウィンドウは全画面にしていない。受信側を構えながら大きさや位置を調整したいのと、
全画面は止めるのが面倒なため。初期サイズは選んだモニタの作業領域に収まる正方形
(短辺の 85%) で、コードは中央にアスペクト比を保ってフィットする。

輝度調整は画素を触らず `QImage` のカラーテーブル (256 エントリ) を差し替えるだけなので、
フレームごとの追加コストがない。送信中は `SetThreadExecutionState` でディスプレイの
消灯を止める (Windows のみ)。

受信は入れていない。PC の内蔵カメラは 720p 級が多く、vcode の目安である 6px/セル を
満たせないため、受信はスマホの方が現実的なため。

## Python 環境 (uv)

Python 側は uv で一元管理する。ルートの `pyproject.toml` に依存グループを定義し、
`uv.lock` で固定している。`package = false` にしてあるのでリポジトリ自体はインストール
されない (ライブラリとして配布しないため)。スクリプトは `uv run python ...` で直接叩く。

| グループ | 中身 | 用途 |
|---|---|---|
| (既定) | 依存なし | `tools/vcode_encode.py` は標準ライブラリだけで動く |
| `desktop` | `vloom-core` (= `py/`) + PySide6 | デスクトップ送信アプリ。**Rust が要る** |
| `dev` | Pillow, cryptography | `tools/make_testdata.py`, `web/pwa/serve_https.py` |

`vloom-core` は `[tool.uv.sources]` で `py/` を editable 指定しているので、`uv sync` が
maturin を呼んで Rust 拡張をビルドする。手で `maturin develop` を打つ必要はない。

`uv sync` は宣言どおりの状態に揃える (宣言外のパッケージは消す) ので、複数グループが
要るときは `uv sync --group desktop --group dev` のように同時指定する。

| スクリプト | 用途 | 必要グループ |
|---|---|---|
| `tools/vcode_encode.py` | ファイル → vcode フレーム列 (PNG + 再生 HTML) | なし |
| `tools/make_testdata.py` | 計測用テスト画像の生成 | `dev` |
| `web/pwa/serve_https.py` | 開発用 HTTPS サーバ (実機は HTTPS でないとカメラが開かない) | `dev` |

`tools/vcode_encode.py` だけは Rust に依存しない独立実装で、RaptorQ の source パケットを
純 Python で組み立てる (リペアパケットは持たない)。Rust の `encode_frame` とバイト単位で
一致することをテストで確認しているが、**配布物には含めない**。実装の分岐を配布物に
持ち込まないための線引き。

## ライセンス

Vloom 本体は [MIT](../LICENSE)。配布物に含まれるものの著作権表示は
[THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md)、Apache-2.0 の全文は
[licenses/Apache-2.0.txt](../licenses/Apache-2.0.txt) にある。

**コピーレフトの依存はデスクトップアプリの PySide6 (LGPL-3.0) だけ。** PWA・モバイル
アプリ・Rust コア・`tools/` は MIT / Apache-2.0 / BSD のみで構成されている。exe 化する
場合の LGPL の扱いは THIRD-PARTY-NOTICES.md に書いた。

| コンポーネント | ライセンス |
|---|---|
| Vloom (`fountain` / `vcode` / `core-wasm` / `app` / `tools`) | MIT |
| raptorq 2.x | **Apache-2.0** |
| wasm-bindgen 0.2 | MIT OR Apache-2.0 |
| flutter_rust_bridge 2.12.0 | MIT |
| Flutter SDK + camera / image_picker / file_selector / path_provider / share_plus / wakelock_plus / flutter_file_dialog / open_filex | BSD-3-Clause |
| screen_brightness / cupertino_icons | MIT |
| pyo3 0.24 | MIT OR Apache-2.0 |
| **PySide6 / shiboken6 6.11 (desktop のみ)** | **LGPL-3.0 OR GPL-2.0 OR GPL-3.0** |
| png (dev のみ) | MIT OR Apache-2.0 |
| cryptography (dev のみ) | Apache-2.0 OR BSD-3-Clause |
| Pillow (dev のみ) | MIT-CMU |

アプリ内では AppBar の ⓘ から Flutter 標準のライセンス画面を開ける。pub パッケージの
LICENSE は Flutter が自動収集し、cargo 側 (raptorq) は `main.dart` の
`_registerNativeLicenses()` で登録している。PWA はフッタから
`THIRD-PARTY-NOTICES.md` へリンクしている (Pages のデプロイ時に成果物へコピーされる)。

## 通信について

**転送そのものは通信を一切使わない。** ファイルは画面とカメラの間しか通らない。

コード側も同じで、Rust (`std::net` 等)・Dart (`HttpClient` / `Socket`)・PWA の JS
(`XMLHttpRequest` / `WebSocket` / `sendBeacon`) のいずれにも通信の呼び出しはない。
Dart の `dart:io` は File / Directory / Platform にしか使っていない。PWA には外部ホストへの
URL が 1 つも無く、`fetch` は Service Worker の自アセットキャッシュと、同梱の計測用画像の
読み込み (同一オリジン) の 2 箇所だけ。Firebase・Crashlytics・アナリティクスの類は無い。

Android の権限は `CAMERA` のみを宣言している。ただしマージ後のマニフェストには
プラグイン由来のものが加わる。v0.3 までは QR 受信の `mobile_scanner` が ML Kit を引き込み、
その推移的依存 `com.google.android.datatransport:transport-backend-cct` が `INTERNET` と
`ACCESS_NETWORK_STATE`、および送信ジョブ (`JobInfoSchedulerService` /
`AlarmManagerSchedulerBroadcastReceiver`) をマージしていた。

**v0.4 のリリースビルドで、マージ後の権限が以下だけになったことを確認済み** (2026-08-26):

```
CAMERA
READ_MEDIA_AUDIO / READ_MEDIA_IMAGES / READ_MEDIA_VIDEO   (open_filex 由来)
app.vloom.vloom.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION  (Flutter 由来)
```

`INTERNET` / `ACCESS_NETWORK_STATE` / `RECORD_AUDIO` はいずれも消えた。マージ後の
service / receiver / provider にも `datatransport` と `mlkit` は 1 つも残っていない。
**これで「アプリは通信しない」が権限レベルで担保される。** 依存を足したときは
`app/build/app/intermediates/merged_manifests/release/` で再確認すること。

`RECORD_AUDIO` は `camera_android_camerax` がマージしてくるが、Vloom はカメラを
`enableAudio: false` で開いておりマイクを使わないので、`AndroidManifest.xml` の
`tools:node="remove"` で落としている。`open_filex` 由来の `READ_MEDIA_*` は
受信ファイルを既定アプリで開くのに要るので残している。

## 経緯: QR 経路の削除 (v0.4)

初期は QR (`mobile_scanner` + jsQR + qrcode.js) と vcode の 2 系統を並走させ、
vcode の性能を QR と比較しながら育てていた。実機で vcode が QR 経路の 4 倍
(26.8 KB/s 対 6.7 KB/s) に達し、比較対象としての役目が終わったので削除した。

削除で得たもの:

- **依存が 4 つ減った** — mobile_scanner (と ML Kit)、`qrcode` クレート、jsQR、qrcode.js
- **`INTERNET` 権限が消えた** — ML Kit のテレメトリ経路ごと無くなった
- **ライセンス面が単純になった** — ML Kit (Google 独自ライセンス) と jsQR (Apache-2.0) が外れた
- **UI が 5 タブから 3 タブに**、校正画面から種別トグルが消えた

失ったもの: 既存の QR デコーダとの互換性。vcode は独自形式なので、受信には Vloom が要る。

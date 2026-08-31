# 開発ガイド

ビルド・実行・実機検証の手順。全体構成と選定理由は [tech_stack.md](tech_stack.md)。

## 必要環境

- **Rust** (stable) + `wasm-pack` — `cargo install wasm-pack`
- **Flutter** 3.41+ — ネイティブアプリをビルドする場合
- **[uv](https://docs.astral.sh/uv/)** — Python 側 (デスクトップアプリ・開発スクリプト) の環境管理

## ビルドとテスト

```bash
cargo test --workspace          # Rust コア (fountain / vcode スキャナ)

cd core-wasm                    # PWA 用の WASM
wasm-pack build --target web --release --out-dir ../web/pwa/pkg

cd app                          # ネイティブアプリ
flutter pub get
flutter build apk --release     # Android
```

`web/pwa/pkg/` は git 管理外なので、クローン直後は必ず WASM ビルドが要ります。

### 計測用の並行インストール (lab ビルド)

リリース鍵 (`app/android/key.properties`) を持たない環境では debug 鍵にフォールバックするため、
配布版が入っている端末には署名不一致で上書きインストールできません。計測用に横へ入れたい
ときは、別パッケージになるサフィックスを付けてください。

```bash
flutter build apk --release -PappIdSuffix=.lab   # app.vloom.vloom.lab / ランチャー名 "Vloom (lab)"
```

配布版と受信履歴を残したまま併存します。サフィックス未指定なら通常どおりです。

### 受信アプリの起動 Intent (計測の自動化)

lab ビルドは起動 Intent で条件を指定できます (画面操作なしで条件を振るため)。

```bash
adb shell am start -n app.vloom.vloom.lab/app.vloom.vloom.MainActivity \
  --ei tab 1 --ei preset 5            # タブ (0=送信 1=受信) とプリセット
  # --es grid 13x16                   # プリセットに無い格子
  # --es ev -1.5                      # 露出補正 (EV)
  # --es aepoint none                 # AE 測光点 (既定 center) を外す
  # --ei exp 2083 --ei iso 300        # 露光時間 µs と ISO の直接指定 (AE off)
  # --es camlock none|ae|both         # 追従安定後のカメラロック
  # --ei dump 4                       # 70% 未満しか読めなかったフレームを N 枚保存
```

統計は `adb logcat` の `[vloom-stats]` (完了時) と `[vcode-rx] seq=` (フレームごと) に出ます。

## Python 環境 (uv)

用途ごとに依存グループを分けてあり、既定では何も入りません
(`tools/vcode_encode.py` は標準ライブラリだけで動くため)。

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
パッケージは消えます。両方要るときは同時に指定してください。既に `.venv` がある場合、
`uv sync` はそこに対して同期します。宣言に無いパッケージは削除されるので、別環境で
試すなら `UV_PROJECT_ENVIRONMENT` を指定してください。

## PWA を実機で試す

カメラ API (`getUserMedia`) は HTTPS または localhost でしか動きません。スマホから開くには
HTTPS が必要なので、自己署名証明書付きの開発サーバーを同梱しています。

```bash
uv run --group dev python web/pwa/serve_https.py
```

PC とスマホを同じ Wi-Fi に繋ぎ、表示された `https://<PC の IP>:8443/` を開きます。証明書警告は
「詳細設定 → アクセスする」で許容してください。開発専用です。

**サービスワーカーの罠**: PWA はオフライン用にキャッシュ優先で動くため、配信物を更新しても
端末には旧版が表示され続けます。`web/pwa/sw.js` の `CACHE` バージョンを上げるのを忘れずに。
端末側は再読み込み 2〜3 回 (切り替わりの途中に空ページが出ることがある) か、サイトデータの
削除で確実に新しくなります。GitHub Pages と LAN サーバーは別オリジン = 別キャッシュです。

## デスクトップ送信アプリ (Windows)

PC を送信機として使うだけなら、ブラウザより表示タイミングを正確に刻める PySide6 版が
あります。符号化は Rust コアをそのまま呼ぶので、出るフレームは PWA と同一です。

```bash
uv sync --group desktop     # PySide6 + Rust コア (py/) をビルドして入れる
uv run python -m desktop
```

uv を使わない場合は `pip install pyside6 maturin && maturin develop --release -m py/Cargo.toml`
でも同じ状態になります。

ファイルを選んで「送信開始」で送信ウィンドウが開きます。コードはアスペクト比を保って
フィットし、輝度とスムージングは表示中に調整できます (Esc で閉じる)。

**窓の中でのコードの位置と大きさも変えられます。** 送信中に矢印キーで動かし、
`+` / `-` で拡縮、`0` で戻ります (Shift で粗く)。`H` (または `--hold`) でフレームを止めて
1 枚を出し続ける「静止」になり、構図合わせ中に受信が完走して画面が変わるのを防げます。
現在値は下部バーに `--zoom 0.80 --dx +0.050 --dy -0.030` の形で出るので、そのまま
コマンドラインに渡せば同じ構図を再現できます。

受信は入れていません (PC の内蔵カメラは 720p 級が多く、vcode の目安 6px/セル に
届かないため)。スマホの「受信」を向けてください。

**PySide6 は LGPL-3.0** です。Vloom で唯一のコピーレフト依存で、このアプリにのみ
関係します。exe 化する場合の注意は [../THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md)。

## コマンドラインから vcode を作る

ブラウザやアプリを開かずに、ファイルを vcode のフレーム列へ変換できます。Python の標準
ライブラリだけで動くので追加インストールは要りません。

```bash
uv run python tools/vcode_encode.py photo.jpg --grid 9x8 --bpc 2 --fps 15
```

`photo_vcode/` に PNG のフレーム列と `index.html` が出ます。HTML をブラウザで開き、クリックで
全画面にすればそのまま「受信」で受け取れます。送信側の `VcodeTx` とバイト単位で同じ
フレームを吐くので、受信側の変更は要りません。

RaptorQ は source パケットのみを生成します (リペア無し)。RFC 6330 の中間シンボル生成は純
Python には重すぎるためで、取りこぼしたブロックは次の周回で拾うことになります。リペアの
損失耐性が要る場面では PWA かアプリを使ってください。

## iOS について

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

## 調査用ツール (vcode/examples)

実機で読めないときの切り分け用。カメラフレームは受信アプリの Intent `--ei dump N` で
`/sdcard/Android/data/<pkg>/files/vcode_*.gray` に保存されます (1200×1600 の生輝度)。

```bash
# ダンプに四隅と格子を与えてスキャン (探索が悪いのか照合が悪いのか)
cargo run --release -p vloom-vcode --example scan_quad -- dump.gray 1200 1600 13 18 <tlx tly trx try brx bry blx bly>

# ブロックごとの最適サブセルオフセット (誤差の場)。歪曲か四隅の誤差かを見る
cargo run --release -p vloom-vcode --example block_field -- dump.gray 1200 1600 13 18

# 送信側の正解フレームとビット単位で比較 (ヘッダが読めないとき)
cargo run --release -p vloom-vcode --example header_diag -- ...
```

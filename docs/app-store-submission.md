# Vloom App Store 提出メモ

App Store Connect の各フィールドに貼る文案と、審査前にやることの一覧。
ココメシ (`koko-meshi/docs/app-store-submission.md`) と同じ組み立てで、Apple 側の
アカウント・証明書・API キーはそちらと共有する。

リポジトリ側の準備物:

| ファイル | 役割 |
|---|---|
| `app/ios/Runner/PrivacyInfo.xcprivacy` | プライバシーマニフェスト (2024年5月以降必須)。CI が同梱を検査する |
| `app/ios/ExportOptions-AppStore.plist` | 提出用 IPA の書き出し設定 (teamID / プロファイル名) |
| `app/ios/Runner/Info.plist` の `ITSAppUsesNonExemptEncryption` | 輸出コンプライアンスの自己申告 (false) |
| `web/pwa/privacy-policy.html` | プライバシーポリシー本文。GitHub Pages で公開される |
| `.github/workflows/build.yml` の `ios-appstore` ジョブ | 署名・書き出し・App Store Connect へのアップロード |

---

## 現状 (2026-09-15)

- **iOS 版は実機で一度も動かしていない。** 計測はすべて Pixel 9a 受信で、iPhone 12 Pro は
  Safari の PWA で送る側にしか使っていない。
- CI (`build.yml` の `ios` ジョブ) は main への push ごとに署名なし IPA を作っていて、
  Rust のクロスビルドを含めて通っている。**まずこれを Sideloadly で iPhone に入れて、受信が
  動くかを見る**のが最初の一歩 ([docs/development.md](development.md#ios-について))。
- 実機で確かめるまで分からないこと:
  - 受信の解像度。Android は `ResolutionPreset.max` が 1600x1200 (4:3) だが、iOS では
    センサー最大 (12MP) になるので、`preset.dart` の `cameraResolutionFor` で 1920x1080 に
    読み替えている。画角は横が 25% 狭くなるので、構図の誘導 (充填率・余白) と px/セル が
    Android と同じ感覚で使えるかを見る。4K (`ultraHigh`) が処理速度的に間に合うなら
    そちらの方が密な格子に有利。
  - 露出補正 (-1 EV)・測光点・AF/AE ロックの挙動。iOS プラグインにも実装はあるが刻みが違う。
  - 手動露光 (計測用) は Android 専用の経路で、iOS では黙って飛ぶ。問題ない。
  - ファイルの保存 (flutter_file_dialog)・共有 (share_plus)・開く (open_filex) の iOS 動作。

---

## 審査前にやること (コード側)

### 1. Bundle ID — 対応済み

**`com.rou39.vloom`** にした (2026-09-15)。持っているドメインを逆にして名乗る慣習に合わせ、
OmniVerse (`com.rou39.omniverse`) / ココメシ (`com.rou39.kokomeshi`) と揃えた。

**Android の applicationId は `app.vloom.vloom` のまま**にしてある。Releases の APK として
配布済みで、変えると別アプリ扱いになって上書き更新できなくなるため。両者が違っていても
支障はない。

### 2. iPhone 専用 — 対応済み

`TARGETED_DEVICE_FAMILY = 1`。審査は iPad でも試されるので、確認できない iPad は対象から
外した。縦固定・カメラ前提のアプリなので iPad で得るものも少ない。

### 3. 権限の文言 — 対応済み

- カメラ: 「コードを読み取ってデータを受信するためにカメラを使用します。」
- マイク: 使わないが、camera プラグインがリンクする API に対してアップロード時に
  ITMS-90683 (purpose string 欠落) が出るので、「使用しない」旨を書いた文言を入れてある。
- 写真ライブラリ: 使う画面がないのに文言だけ残っていたので、未使用だった image_picker ごと外した。
  審査で「要求する権限を使う場面がない」と突かれる材料をなくすため。

### 4. バージョン

ストア表示の Version は `1.0` にしたい。`Info.plist` の `CFBundleShortVersionString` は
`$(FLUTTER_BUILD_NAME)` を見ているので、**提出するときに `app/pubspec.yaml` を `1.0.0+N` にする**
(Android の表示も同時に 1.0.0 になる。初回ストアリリースなので自然)。

ビルド番号 (`+N`) は App Store Connect 側で一意である必要があり、同じ組み合わせは二度
受け付けられない。差し戻されて出し直すときも必ず上げること。

### 5. 起動画面

Flutter 既定の真っ白な LaunchScreen のまま。落ちる要因ではないが、Vloom の配色
(`#111318`) に合わせるなら `app/ios/Runner/Base.lproj/LaunchScreen.storyboard` の背景色を変える。

---

## プロモーション用テキスト (170 文字以内)

```
光だけでファイルを渡す。
ネットワークもペアリングも要らない。片方の画面にコードを映し、もう片方のカメラで撮るだけ。
Wi-Fi も Bluetooth も、相手の連絡先も不要。データはどこにも送られません。
```

## 概要 (4000 文字以内)

```
【Vloom とは】
Vloom は、光だけでファイルを渡すアプリです。
送る側の画面にコードの動画を表示し、受け取る側のカメラで撮り続けるだけで、通信経路を一切介さずにファイルが届きます。
Wi-Fi も Bluetooth も、相手の連絡先も、アカウントも要りません。

【使い方】
1. 送る側: ファイルを選び、画面いっぱいにコードを表示します (画面の明るさは最大に)
2. 受け取る側: 「受信」を開き、相手の画面にカメラを向けてガイド枠に収めます
3. 画面の案内 (近づけて / 離して) に従って構図を合わせると、自動で受信が進みます
4. 受信が終わると内容をその場で確認でき、保存や共有ができます

送る側は Vloom アプリでも、パソコンのブラウザ (https://kuronekorou39.github.io/vloom/) でもかまいません。

【1 枚も取りこぼさずに撮る必要はありません】
ファイルは Fountain 符号 (RaptorQ) で細かい断片に分けて繰り返し表示されます。
受け取る側は十分な数の断片が集まった時点で復元するので、手ブレや位置ズレでいくつか読めなくても、次の周回で拾えます。

【データは、どこにも送られません】
アプリはネットワークに接続しません。
送受信したファイルと履歴は端末の中だけに保存され、開発者のサーバーは存在しません。
広告も、アプリ内課金も、アカウント登録もありません。

【こんなときに】
■ ネットワークが使えない場所で、隣の人にファイルを渡したい
■ 相手の連絡先を交換せずに、その場でデータを手渡したい
■ 機密性の高い環境で、通信経路を残さずに小さなファイルを移したい

【制約と目安】
・送る側の画面は平面のディスプレイが必要です (曲面ディスプレイでは読み取れません)
・格子 (プリセット) は送る側と受け取る側で同じものを選びます
・転送速度は画面とカメラの性能、明るさ、距離に左右されます。実測では 1MB のファイルを 5〜10 秒程度で受信できます (条件による)

【オープンソース】
Vloom のソースコードは GitHub で MIT ライセンスのもとに公開しています。
https://github.com/kuronekorou39/vloom
```

## キーワード (100 文字以内、カンマ区切り)

```
ファイル転送,オフライン,共有,カメラ,QR,光通信,ペアリング不要,Wi-Fi不要,Bluetooth不要,エアギャップ,データ転送,送信,受信
```

## 著作権

```
© 2026 rou39.com
```

## カテゴリ

- 第1: **ユーティリティ**
- 第2: 仕事効率化

---

## App Review 情報

### メモ (4000 文字以内)

審査で最も引っかかりやすいのは「審査員が 1 台では試せない」こと。送る側を Web 版で
代替できることを最初に書く。

```
【App Review担当者の方へ】

Vloom は、画面に表示したコードをカメラで撮ることで、通信を一切使わずにファイルを渡すアプリです。審査にあたってのご案内です。

■ 動作確認には「送る側」と「受け取る側」の 2 つの画面が必要です
送る側は iPhone 上の本アプリのほか、パソコンやタブレットのブラウザでも動く Web 版が使えます。
Web 版: https://kuronekorou39.github.io/vloom/ (インストール不要・アカウント不要)

推奨する確認手順:
1. パソコンのブラウザで上記 Web 版を開き、「送信」タブで任意のファイル (小さな画像など) を選びます。プリセットは既定のままで構いません
2. 「送信開始」を押すと、画面にコードの動画が表示されます。可能であれば全画面にし、画面の明るさを上げてください
3. 審査用の iPhone で本アプリを起動し、「受信」タブを開きます (カメラの許可を求めます)
4. 画面の案内 (近づけて / 離して) に従って、コードがガイド枠に収まるようにカメラを向けます。プリセットは送る側と同じ既定値です
5. 数秒で受信が完了し、内容のプレビューと「保存」「共有」が表示されます
6. 「履歴」タブに受信したファイルが残ります

iPhone を 2 台お持ちの場合は、1 台目の本アプリ「送信」タブから同じ手順で確認できます。

■ アカウント不要です
ログイン機能はありません。テスト用アカウントも必要ありません。

■ 各権限を使用する理由
・カメラ … 相手の画面に表示されたコードを読み取ってファイルを受信するため。映像は読み取りにのみ使い、保存も送信もしません
・マイク … 使用しません。カメラ機能のライブラリが Info.plist の宣言を必要とするため記載しています。マイクへのアクセスを求めることはありません
・ファイル … 送信するファイルの選択と、受信したファイルの保存・共有に、OS のファイル選択画面を使います

■ プライバシーについて
・本アプリはネットワークに接続しません
・送受信したファイルと履歴はすべて端末内にのみ保存されます
・当方のサーバーは存在せず、ユーザーデータの収集は一切行っていません
・広告 SDK、解析 SDK (アナリティクス) は組み込んでいません
・アプリ内課金はありません

■ 既知の留意点
・送る側の画面は平面のディスプレイが必要です。曲面ディスプレイのスマートフォンでは読み取れません
・転送速度は画面とカメラの性能や明るさに左右されます。小さなファイル (数十 KB) であれば数秒で完了します

ご不明な点がございましたらお知らせください。よろしくお願いいたします。
```

添付: 送受信の様子を撮った短い動画があると、審査員が手順を誤解する余地が減る。
Web 版 (送信) と iPhone (受信) を並べて 30 秒程度。

### 連絡先情報

| 項目 | 内容 |
|---|---|
| 名 / 姓 | 半角英字で本名 |
| 電話番号 | +81 形式 |
| メール | kuronekorou39@gmail.com |

### サインイン情報

**不要** (ログイン機能がないため、「サインインが必要」のチェックを外す)

---

## URL

| 種類 | URL | 必須 |
|---|---|---|
| サポート URL | `https://github.com/kuronekorou39/vloom` | 必須 |
| マーケティング URL | `https://kuronekorou39.github.io/vloom/` | 任意 |
| プライバシーポリシー URL | `https://kuronekorou39.github.io/vloom/privacy-policy.html` | **必須** |

ポリシー本文の実体は `web/pwa/privacy-policy.html`。main に push すると `pages.yml` が
GitHub Pages に反映する (PWA と同じ配信物)。**提出前にブラウザで開けることを確認する。**
ココメシ / OmniVerse は rou39.com 配下に置いているので、揃えたければ portfolio-rou39-dev
リポジトリに同じ内容を置いて URL を差し替える。

---

## App プライバシー (Nutrition Label)

App Store Connect の「App のプライバシー」で聞かれる質問への回答。
**すべて「データを収集しない」で通る。** ネットワーク通信自体がないので、ココメシより
さらに単純。間違って「収集する」にしないこと。

| 質問 | 回答 |
|---|---|
| このAppはデータを収集しますか？ | **いいえ** |

---

## 年齢制限

該当なしで **4+** になるはず。

---

## スクリーンショット

必須デバイス: **iPhone 6.9"** (Pro Max 系)。iPad は対象外にしたので不要。

撮影候補:

| 順 | 画面 | メインコピー | サブコピー |
|---|---|---|---|
| 1 | 受信中 (ガイド枠にコードが収まり、進捗が伸びている) | 光だけで、ファイルを渡す。 | Wi-Fi も Bluetooth も要らない。 |
| 2 | 送信 (画面いっぱいのコード) | 画面に映して、 | 相手はカメラを向けるだけ。 |
| 3 | 受信完了 (プレビューと保存・共有) | 届いたら、その場で確認。 | 保存も共有もワンタップ。 |
| 4 | 構図の誘導 (近づけて / 離して) | 合わせ方は、画面が教える。 | 充填率と余白を読み上げる。 |
| 5 | 履歴 | 受け取ったものは、手元に残る。 | データは端末の中だけ。 |

注意:
- 送信するファイルは自作の画像にする (他人の写真・実在の文書を使わない)
- 受信画面の背景に人物や部屋の様子が写り込まないようにする

---

## 実機確認 (Sideloadly)

Mac が無いので、実機確認は CI の署名なし IPA を Sideloadly で入れる。手順は
[docs/development.md](development.md#ios-について)。有料の Developer アカウント (ココメシ /
OmniVerse と同じ Apple ID) でサインインすれば 1 年有効、無料アカウントなら 7 日。

確認する順:

1. 起動 → 「受信」タブでカメラ許可のダイアログが出て、プレビューが映る
2. PWA (パソコン) を送信側にして、確実プリセット (5x4) から順に受信が通る
3. 構図の誘導 (充填率・余白・px/セル) の値が Android と同じ感覚で読める
4. 受信完了 → 保存 (ファイル App) と共有 (共有シート) が動く
5. 「送信」タブでファイルを選び、iPhone の画面を Pixel で受信できる (これは OLED 送信として計測済みなので通るはず)

---

## 提出の流れ (CI)

手元は Windows で Xcode が無いので、**署名も書き出しもアップロードも GitHub Actions でやる**。
`v*` タグを push すると `build.yml` の `ios-appstore` ジョブが動き、署名済み IPA を作って
App Store Connect まで送る。そのあと App Store Connect で「審査に提出」を押すのは手作業。

ココメシと同じ組み立てで、ハマりどころもそこで踏んだものを引き継いでいる:

- **`macos-26` ランナーを固定**。Apple の要件 (2026-04-28 以降) で iOS 26 SDK が要る
- **`flutter build ios --no-codesign` を archive の前に必ず走らせる**。ここがプラグイン統合と
  cargokit の Rust ビルドをやっていて、飛ばすと archive が落ちる
- **署名設定は `app/ios/Flutter/Release.xcconfig` に追記する**。`xcodebuild` の引数で渡すと
  ワークスペース内の全ターゲットに効き、パッケージのリソースバンドルが
  "does not support provisioning profiles" で archive を失敗させる
- **`.p8` は CRLF を落として末尾に改行を付ける**。altool は PEM の解釈が厳しい

Vloom で新しいのは Rust のクロスビルド (cargokit) だが、これは署名なしの `ios` ジョブで
すでに通っているので、`ios-appstore` も同じ手順で Rust ツールチェーンを入れている。

### 一度だけやる準備

Apple のサイトは 2 つあって役割が違う。**どちらで作業しているかを常に意識する**。

| サイト | URL | ここでやること |
|---|---|---|
| Developer ポータル | developer.apple.com/account | App ID、証明書、プロファイル |
| App Store Connect | appstoreconnect.apple.com | アプリの登録、審査提出、API キー |

ココメシ / OmniVerse で作ったものが流用できるので、**新しく作るのは 3 つだけ**
(App ID / プロファイル / アプリ登録)。証明書と API キーは使い回す。

#### 手順1: App ID を作る (Developer ポータル)

1. https://developer.apple.com/account → **Certificates, IDs & Profiles**
2. 左メニューの **Identifiers** → 右上の **＋**
3. **App IDs** → Continue → **App** → Continue
4. 入力する:
   - **Description**: `Vloom` (半角英数)
   - **Bundle ID**: **Explicit** にして `com.rou39.vloom`
5. **Capabilities は何もチェックしない**。カメラは Info.plist の権限であって、ここには出てこない。
   ココメシで要ったメモリ上限の引き上げも Vloom には不要
6. Continue → Register

#### 手順2: プロビジョニングプロファイルを作る (Developer ポータル)

1. 左メニューの **Profiles** → 右上の **＋**
2. **Distribution** の中の **App Store Connect** → Continue
3. **App ID**: `Vloom (com.rou39.vloom)`
4. **証明書**: ココメシ / OmniVerse で使っている **Apple Distribution** の証明書
   (`Apple Distribution: ... (M5D5CQRGX8)`。複数あれば有効期限が先のもの)
5. **Provisioning Profile Name**: `Vloom AppStore` と**正確に**入力する
   - この名前は `app/ios/ExportOptions-AppStore.plist` と `build.yml` の
     `PROVISIONING_PROFILE_SPECIFIER` が参照している。1 文字でも違うとビルドが落ちる
6. Generate → **Download** して `.mobileprovision` を保存

#### 手順3: App Store Connect にアプリを登録する

1. https://appstoreconnect.apple.com → **マイApp** → **＋** → **新規App**
2. 入力する:
   - **プラットフォーム**: iOS
   - **名前**: `Vloom` (ストアに出る名前。30 文字以内。取られていたら `Vloom - 光でファイル転送` など)
   - **プライマリ言語**: 日本語
   - **バンドルID**: `com.rou39.vloom` (出てこなければ手順1の反映待ち、数分)
   - **SKU**: `vloom-ios` (後で変えられない)
   - **ユーザーアクセス**: フルアクセス
3. 作成

#### 手順4: GitHub に Secrets を登録する

https://github.com/kuronekorou39/vloom/settings/secrets/actions の **New repository secret** から 6 つ。

| Secret | 中身 | どこから取るか |
|---|---|---|
| `IOS_CERTIFICATE_BASE64` | Distribution 証明書 (.p12) を base64 した文字列 | **ココメシと同じ値** |
| `IOS_CERTIFICATE_PASSWORD` | その .p12 のパスワード | 同上 |
| `IOS_APPSTORE_PROFILE_BASE64` | 手順2の `.mobileprovision` を base64 した文字列 | **今回新規**。下のコマンドで作る |
| `APPSTORE_API_KEY_ID` | API キーの ID (10 文字程度) | **ココメシと同じ値** |
| `APPSTORE_API_ISSUER_ID` | Issuer ID (UUID 形式) | 同上 |
| `APPSTORE_API_PRIVATE_KEY` | `.p8` ファイルの中身そのもの | 同上 |

base64 にするコマンド (PowerShell):

```powershell
[Convert]::ToBase64String([IO.File]::ReadAllBytes("C:\path\to\Vloom_AppStore.mobileprovision")) | Set-Clipboard
```

Secrets が無いうちは各ステップが飛ぶだけで、ワークフローは失敗しない。

### 毎回やること

1. `app/pubspec.yaml` の version を上げる。**ビルド番号 (`+N`) は必ず増やす**
2. タグを打って push する (`git tag v1.0.0 && git push origin v1.0.0`)
3. Actions が通ったら App Store Connect でビルドを選び、審査に提出する
   (ビルドが「処理中」から出るまで 10〜30 分かかる)

---

## 提出チェックリスト

### コード / ビルド
- [x] Bundle ID を `com.rou39.vloom` に確定
- [x] iPhone 専用にする
- [x] `PrivacyInfo.xcprivacy` を同梱し、CI で検査する
- [x] `ITSAppUsesNonExemptEncryption=false`
- [x] マイクの文言を入れ、未使用の写真ライブラリ権限 (image_picker) を外す
- [x] iOS の受信解像度を暫定で 1920x1080 に読み替える
- [ ] **Sideloadly で実機に入れて受信が通ることを確認する** (上の「実機確認」)
- [ ] 実機の結果を見て解像度 (1080p / 4K) と露出制御を決める
- [ ] Developer ポータルで App ID を登録 (手順1)
- [ ] プロビジョニングプロファイルを `Vloom AppStore` の名前で作る (手順2)
- [ ] App Store Connect にアプリを登録 (手順3)
- [ ] iOS 用の Secrets 6 件を登録 (手順4)
- [ ] ストア表示バージョンを `1.0` にする (pubspec を `1.0.0+N`)
- [ ] タグを打って `ios-appstore` ジョブが通ることを確認

### App Store Connect
- [ ] プロモーション用テキスト / 概要 / キーワード / 著作権
- [ ] カテゴリ (ユーティリティ)
- [ ] 年齢制限アンケート (該当なしで 4+)
- [ ] App のプライバシー = 「データを収集しない」
- [ ] サポート URL / プライバシーポリシー URL (**先に公開されていることを確認**)
- [ ] App Review メモを貼る (Web 版で送信側を代替できることを最初に)、サインイン不要にチェック
- [ ] 審査用のデモ動画を添付 (任意だが強く推奨)
- [ ] 連絡先情報 (実名・電話・メール)
- [ ] スクリーンショット (iPhone 6.9")
- [ ] ビルドを選んで審査に提出

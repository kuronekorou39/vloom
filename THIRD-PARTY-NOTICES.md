# サードパーティ ライセンス表記

Vloom 本体は [MIT](LICENSE) です。配布物 (PWA の WASM / Android APK / iOS アプリ /
デスクトップ送信アプリ) には以下のサードパーティのコードが含まれます。全文は `licenses/`
および各パッケージ付属の LICENSE を参照してください。

> **デスクトップ送信アプリ (`desktop/`) だけはコピーレフト (LGPL-3.0) の依存があります。**
> PySide6 が該当します。PWA・モバイルアプリ・Rust コア・`tools/` には影響しません。
> 詳細は「LGPL-3.0 (デスクトップアプリのみ)」の節を参照してください。

技術スタック全体の説明は [docs/tech_stack.md](docs/tech_stack.md) にあります。

## Apache License 2.0

全文: [licenses/Apache-2.0.txt](licenses/Apache-2.0.txt)

| コンポーネント | 著作権表示 | 含まれる配布物 |
|---|---|---|
| [raptorq](https://github.com/cberner/raptorq) 2.x | Copyright Christopher Berner | WASM / APK / iOS |

raptorq には上流に NOTICE ファイルはありません。

## MIT License

| コンポーネント | 著作権表示 | 含まれる配布物 |
|---|---|---|
| [flutter_rust_bridge](https://github.com/fzyzcjy/flutter_rust_bridge) 2.12.0 | Copyright (c) 2021 fzyzcjy | APK / iOS |
| [pyo3](https://github.com/PyO3/pyo3) 0.24 (MIT を選択) | Copyright (c) 2017-present PyO3 Project | デスクトップ |
| [screen_brightness](https://github.com/aaassseee/screen_brightness) 2.1.11 | Copyright (c) 2021 Jack Liu | APK / iOS |
| [cupertino_icons](https://github.com/flutter/packages) 1.0.9 | Copyright (c) 2016 Vladimir Kharlampidi | APK / iOS |

```
Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## MIT OR Apache-2.0 (デュアルライセンス)

Vloom は MIT を選択して利用しています。

| コンポーネント | 著作権表示 | 含まれる配布物 |
|---|---|---|
| [wasm-bindgen](https://github.com/rustwasm/wasm-bindgen) 0.2 | Copyright (c) 2014 Alex Crichton | WASM |

## LGPL-3.0 (デスクトップアプリのみ)

| コンポーネント | ライセンス | 含まれる配布物 |
|---|---|---|
| [PySide6](https://doc.qt.io/qtforpython/) / shiboken6 6.11 | LGPL-3.0-only OR GPL-2.0-only OR GPL-3.0-only | デスクトップ |

Vloom は **LGPL-3.0** を選択して利用しています (GPL を選ぶと Vloom 側まで及ぶため)。

現状 `desktop/` はリポジトリから実行する形で、PySide6 は利用者が自分で
`pip install` します。Vloom は PySide6 のバイナリを一切再配布していないので、
表記以上の義務は発生しません。

**exe 化するときは注意が要ります。** PyInstaller の `--onefile` のように Qt を
単一バイナリへ静的に取り込む形にすると、LGPL-3.0 が求める「利用者が PySide6 を
差し替えて再リンクできること」を満たしにくくなります。配布するなら Qt の DLL を
別ファイルのまま置く構成 (`--onedir`) にし、LGPL-3.0 と GPL-3.0 の全文を同梱して
ください。

## BSD 3-Clause License

| コンポーネント | 著作権表示 | 含まれる配布物 |
|---|---|---|
| Flutter SDK | Copyright 2014 The Flutter Authors | APK / iOS |
| camera 0.11.4 / image_picker 1.2.3 / file_selector 1.1.0 / path_provider 2.1.6 | Copyright 2013 The Flutter Authors | APK / iOS |
| share_plus 13.2.0 | Copyright 2017, the Flutter project authors | APK / iOS |
| wakelock_plus 1.6.1 | Copyright (c) 2020-2023, creativecreatorormaybenot | APK / iOS |
| flutter_file_dialog 3.3.1 | Copyright (c) 2020 KineApps | APK / iOS |
| open_filex 4.7.0 | Copyright 2018 crazecoder | APK / iOS |

```
Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.
3. Neither the name of the copyright holder nor the names of its contributors
   may be used to endorse or promote products derived from this software
   without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

Flutter パッケージの正確な全文は、アプリ内の「ライセンス」(Flutter 標準のライセンス画面)
で各パッケージごとに参照できます。

## 開発時のみ使用 (配布物には含まれない)

`png` (MIT OR Apache-2.0) — vcode のテスト画像出力。
`maturin` (MIT OR Apache-2.0) — `py/` の Python 拡張のビルド。
`cryptography` (Apache-2.0 OR BSD-3-Clause) — 開発用 HTTPS サーバの自己署名証明書生成。
`Pillow` (MIT-CMU) — 計測用テスト画像の生成。

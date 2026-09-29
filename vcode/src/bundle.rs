//! 複数ファイルを 1 本のペイロードにまとめる (無圧縮 ZIP)。
//!
//! vcode が運ぶのは 1 本のバイト列なので、複数ファイルは送信側でまとめてから
//! wrap_file に渡す。独自の入れ物ではなく ZIP にしてあるのは、受信側がこの実装を
//! 持っていなくても (古い版のアプリでも) 受け取ったものをそのまま開けるため。
//!
//! 圧縮はしない (method 0 = stored)。送るものの大半は JPEG/PDF/動画など圧縮済みで
//! 縮まず、deflate の実装 (= 新規依存) を持ち込む理由にならない。

use crate::crc32;

const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_END: u32 = 0x0605_4b50;
const LOCAL_HEADER_LEN: usize = 30;
const CENTRAL_HEADER_LEN: usize = 46;
const END_LEN: usize = 22;
/// 汎用フラグ bit 11: ファイル名が UTF-8
const FLAG_UTF8: u16 = 1 << 11;
/// 展開に必要なバージョン (2.0 = ディレクトリなし・stored/deflate の基本形)
const VERSION_NEEDED: u16 = 20;
/// 1 エントリの名前の上限 (byte)。異常に長い名前でヘッダを膨らませない
const NAME_MAX: usize = 255;

/// まとめる 1 ファイル。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleEntry {
    pub name: String,
    pub data: Vec<u8>,
}

/// ZIP に書く更新時刻 (ローカル時刻)。コアは時計を持たない (wasm では std::time が
/// 使えない) ので、呼び出し側から渡す。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BundleTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl BundleTime {
    /// MS-DOS 形式の (time, date)。年は 1980〜2107 に丸める
    fn dos(&self) -> (u16, u16) {
        let year = self.year.clamp(1980, 2107) - 1980;
        let month = self.month.clamp(1, 12) as u16;
        let day = self.day.clamp(1, 31) as u16;
        let time = ((self.hour.min(23) as u16) << 11)
            | ((self.minute.min(59) as u16) << 5)
            | (self.second.min(59) as u16 / 2);
        (time, (year << 9) | (month << 5) | day)
    }
}

/// パスとして安全な名前にする。区切りと制御文字を落とし、先頭のドットを潰す
/// (展開する側でディレクトリを掘らせない・親を指させない)。
fn safe_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':') || c.is_control() { '_' } else { c })
        .collect();
    s = s.trim().to_string();
    if s.starts_with('.') {
        s.replace_range(..1, "_");
    }
    if s.is_empty() {
        s.push_str("file");
    }
    // UTF-8 の文字境界で切る
    while s.len() > NAME_MAX {
        s.pop();
    }
    s
}

/// 同名が既にあれば "name (2).ext" のように番号を振る
fn unique_name(name: String, used: &[String]) -> String {
    if !used.contains(&name) {
        return name;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    (2..)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|c| !used.contains(c))
        .expect("番号は尽きない")
}

/// ファイル列を無圧縮 ZIP にまとめる。
/// ZIP64 には対応しない (合計 4GB 未満・65535 個未満が前提。光で送る量ではない)。
pub fn bundle_files(entries: &[BundleEntry], time: BundleTime) -> Vec<u8> {
    let (dos_time, dos_date) = time.dos();
    let total: usize = entries.iter().map(|e| e.data.len() + e.name.len() * 2).sum();
    let mut out = Vec::with_capacity(
        total + entries.len() * (LOCAL_HEADER_LEN + CENTRAL_HEADER_LEN) + END_LEN,
    );
    let mut central = Vec::new();
    let mut used: Vec<String> = Vec::with_capacity(entries.len());

    for e in entries {
        let name = unique_name(safe_name(&e.name), &used);
        let nb = name.as_bytes();
        let crc = crc32(&e.data);
        let size = e.data.len() as u32;
        let offset = out.len() as u32;

        out.extend_from_slice(&SIG_LOCAL.to_le_bytes());
        // ローカルヘッダとセントラルディレクトリで共通の並び
        let mut common = Vec::with_capacity(26);
        common.extend_from_slice(&VERSION_NEEDED.to_le_bytes());
        common.extend_from_slice(&FLAG_UTF8.to_le_bytes());
        common.extend_from_slice(&0u16.to_le_bytes()); // method 0 = stored
        common.extend_from_slice(&dos_time.to_le_bytes());
        common.extend_from_slice(&dos_date.to_le_bytes());
        common.extend_from_slice(&crc.to_le_bytes());
        common.extend_from_slice(&size.to_le_bytes()); // 圧縮後
        common.extend_from_slice(&size.to_le_bytes()); // 元
        common.extend_from_slice(&(nb.len() as u16).to_le_bytes());
        common.extend_from_slice(&0u16.to_le_bytes()); // extra
        out.extend_from_slice(&common);
        out.extend_from_slice(nb);
        out.extend_from_slice(&e.data);

        central.extend_from_slice(&SIG_CENTRAL.to_le_bytes());
        central.extend_from_slice(&VERSION_NEEDED.to_le_bytes()); // 作成側バージョン
        central.extend_from_slice(&common);
        central.extend_from_slice(&0u16.to_le_bytes()); // コメント長
        central.extend_from_slice(&0u16.to_le_bytes()); // ディスク番号
        central.extend_from_slice(&0u16.to_le_bytes()); // 内部属性
        central.extend_from_slice(&0u32.to_le_bytes()); // 外部属性
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(nb);
        used.push(name);
    }

    let cd_offset = out.len() as u32;
    let count = entries.len() as u16;
    out.extend_from_slice(&central);
    out.extend_from_slice(&SIG_END.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // このディスク
    out.extend_from_slice(&0u16.to_le_bytes()); // CD のあるディスク
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // コメント長
    out
}

fn u16_at(b: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]) as usize)
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, *b.get(i + 3)?]))
}

/// bundle_files の逆。受信側で中身を 1 つずつ見せるために使う。
///
/// 読めるのは**無圧縮のエントリだけで出来た ZIP** (= こちらがまとめたもの) に限る。
/// 圧縮されたエントリが 1 つでもあれば None を返し、呼び出し側は ZIP のまま扱う。
/// 入力は他人の画面から来るので、長さ・オフセット・CRC は全部確かめ、名前も
/// パスとして安全な形に直して返す。
pub fn unbundle_files(buf: &[u8]) -> Option<Vec<BundleEntry>> {
    // コメントなしの EOCD は末尾固定長 (こちらが書く形)
    let end = buf.len().checked_sub(END_LEN)?;
    if u32_at(buf, end)? != SIG_END {
        return None;
    }
    let count = u16_at(buf, end + 10)?;
    let mut pos = u32_at(buf, end + 16)? as usize;
    let mut entries = Vec::with_capacity(count.min(1024));
    let mut used: Vec<String> = Vec::new();

    for _ in 0..count {
        // 32bit (wasm) では位置の足し算があふれうるので、先に範囲を確かめる
        if pos >= buf.len() {
            return None;
        }
        if u32_at(buf, pos)? != SIG_CENTRAL {
            return None;
        }
        let method = u16_at(buf, pos + 10)?;
        let crc = u32_at(buf, pos + 16)?;
        let packed = u32_at(buf, pos + 20)? as usize;
        let size = u32_at(buf, pos + 24)? as usize;
        let name_len = u16_at(buf, pos + 28)?;
        let extra_len = u16_at(buf, pos + 30)?;
        let comment_len = u16_at(buf, pos + 32)?;
        let local = u32_at(buf, pos + 42)? as usize;
        if method != 0 || packed != size || local >= buf.len() || size > buf.len() {
            return None;
        }
        let name_at = pos + CENTRAL_HEADER_LEN;
        let name = std::str::from_utf8(buf.get(name_at..name_at + name_len)?).ok()?;

        // 中身の位置はローカルヘッダ側の名前長・extra 長で決まる
        if u32_at(buf, local)? != SIG_LOCAL {
            return None;
        }
        let data_at =
            local + LOCAL_HEADER_LEN + u16_at(buf, local + 26)? + u16_at(buf, local + 28)?;
        let data = buf.get(data_at..data_at.checked_add(size)?)?;
        if crc32(data) != crc {
            return None;
        }
        let name = unique_name(safe_name(name), &used);
        used.push(name.clone());
        entries.push(BundleEntry { name, data: data.to_vec() });
        pos = name_at + name_len + extra_len + comment_len;
    }
    if entries.is_empty() {
        return None;
    }
    Some(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: BundleTime =
        BundleTime { year: 2026, month: 9, day: 29, hour: 13, minute: 45, second: 30 };

    fn entry(name: &str, data: &[u8]) -> BundleEntry {
        BundleEntry { name: name.to_string(), data: data.to_vec() }
    }

    #[test]
    fn bundle_roundtrip() {
        let files = vec![
            entry("写真.jpg", b"\xff\xd8 jpeg body"),
            entry("memo.txt", "こんにちは".as_bytes()),
            entry("empty.bin", b""),
        ];
        let zip = bundle_files(&files, T);
        assert_eq!(&zip[..4], b"PK\x03\x04");
        assert_eq!(unbundle_files(&zip), Some(files));
    }

    #[test]
    fn bundle_layout_is_plain_zip() {
        // 1 ファイル・名前 5 byte・中身 3 byte のときの全長と、末尾の EOCD を確かめる
        let zip = bundle_files(&[entry("a.txt", b"abc")], T);
        assert_eq!(zip.len(), (30 + 5 + 3) + (46 + 5) + 22);
        let end = zip.len() - 22;
        assert_eq!(&zip[end..end + 4], b"PK\x05\x06");
        assert_eq!(u16_at(&zip, end + 10), Some(1));
        assert_eq!(u32_at(&zip, end + 16), Some(38)); // セントラルディレクトリの開始位置
        // 時刻: 13:45:30 / 2026-09-29
        assert_eq!(u16_at(&zip, 10), Some((13 << 11) | (45 << 5) | 15));
        assert_eq!(u16_at(&zip, 12), Some((46 << 9) | (9 << 5) | 29));
        // CRC-32 は中身のもの
        assert_eq!(u32_at(&zip, 14), Some(crc32(b"abc")));
    }

    #[test]
    fn bundle_renames_duplicates_and_unsafe_names() {
        let zip = bundle_files(
            &[
                entry("a.jpg", b"1"),
                entry("a.jpg", b"2"),
                entry("a.jpg", b"3"),
                entry("../../etc/passwd", b"4"),
                entry(".hidden", b"5"),
                entry("", b"6"),
            ],
            T,
        );
        let names: Vec<String> =
            unbundle_files(&zip).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(
            names,
            ["a.jpg", "a (2).jpg", "a (3).jpg", "_._.._etc_passwd", "_hidden", "file"]
        );
    }

    #[test]
    fn unbundle_rejects_foreign_or_broken_input() {
        assert_eq!(unbundle_files(b""), None);
        assert_eq!(unbundle_files(b"PK\x03\x04 not really a zip"), None);

        let zip = bundle_files(&[entry("a.txt", b"hello"), entry("b.txt", b"world")], T);
        // 中身が 1 byte 壊れていたら CRC で弾く
        let mut bad = zip.clone();
        bad[30 + 5] ^= 0x01;
        assert_eq!(unbundle_files(&bad), None);
        // 圧縮されたエントリ (method 8) は読まない
        let mut deflated = zip.clone();
        let cd = u32_at(&zip, zip.len() - 22 + 16).unwrap() as usize;
        deflated[cd + 10] = 8;
        assert_eq!(unbundle_files(&deflated), None);
        // 途中で切れている
        assert_eq!(unbundle_files(&zip[..zip.len() - 1]), None);
        // 範囲外を指すオフセット
        let mut far = zip.clone();
        far[cd + 42..cd + 46].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(unbundle_files(&far), None);
    }
}

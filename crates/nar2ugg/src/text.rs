//! 伺かのテキストファイルをデコードし、ブレス (ブロック) と行に分解する。
//!
//! `shell_def` から分離しているのは、**デコードだけ `source` からも使う**ため。
//! zip エントリ名は UTF-8 フラグが立っていない CP932 のことが多く、同じ
//! デコード規則が要る (consumer が 2 つあるので独立モジュールにする価値がある)。
//!
//! ## 文字コードの決め方 (順序が重要)
//! 1. BOM があれば UTF-8。
//! 2. 無ければ**先頭行を ASCII 互換とみなして** `charset,...` を読む。
//! 3. どちらも無ければ CP932 (日本語 Windows で作られた現物の既定)。
//!
//! 「全体をデコードしてから charset を読む」は鶏卵問題になるので不可。

use encoding_rs::{SHIFT_JIS, UTF_8};

const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// 伺かのテキストファイルをデコードする。不正バイトは U+FFFD に置換され、
/// 失敗しない (壊れたシェルでも変換を続行できるようにするため)。
pub fn decode(bytes: &[u8]) -> String {
    let (body, forced_utf8) = match bytes.strip_prefix(BOM) {
        Some(rest) => (rest, true),
        None => (bytes, false),
    };
    if forced_utf8 || charset_says_utf8(body) {
        UTF_8.decode(body).0.into_owned()
    } else {
        SHIFT_JIS.decode(body).0.into_owned()
    }
}

/// zip エントリ名など、charset 行を持たない CP932 バイト列のデコード。
pub fn decode_cp932(bytes: &[u8]) -> String {
    SHIFT_JIS.decode(bytes).0.into_owned()
}

/// 先頭行の `charset,<値>` が UTF-8 系かどうか。
///
/// 表記ゆれ (`Shift_JIS` / `shift-jis` / `sjis` / `utf8` / `UTF-8`) を吸収するため、
/// 英数字以外を落として小文字化してから照合する。
fn charset_says_utf8(body: &[u8]) -> bool {
    // charset 行は ASCII なので、デコード前にバイト列のまま読んでよい。
    let head = &body[..body.len().min(256)];
    let head = String::from_utf8_lossy(head);
    let Some(first) = head.split(['\r', '\n']).next() else {
        return false;
    };
    let Some(value) = first.strip_prefix("charset,").or_else(|| first.strip_prefix("charset ")) else {
        return false;
    };
    let normalized: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    normalized == "utf8"
}

/// `キー,値1,値2,...` の 1 行。行番号は警告でユーザーに位置を示すために持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// 1 始まり。ファイル全体での行番号 (ブレス内での相対位置ではない)。
    pub no: u32,
    pub key: String,
    pub values: Vec<String>,
}

impl Line {
    /// n 番目の値。無ければ None。
    pub fn value(&self, n: usize) -> Option<&str> {
        self.values.get(n).map(|s| s.as_str())
    }
}

/// `名前 { ... }` のブレス。名前が空文字のものはブレス外の行 (ファイル直下)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub name: String,
    pub lines: Vec<Line>,
}

/// デコード済みテキストをブレスと行に分解する。
///
/// 意味解釈は一切しない (キーの妥当性も見ない)。未知のキーは呼び出し側が
/// 黙って読み飛ばせるように、そのまま `Line` として返す。
///
/// - コメントは `//` **行頭のみ**。行中の `//` は値の一部 (URL を壊さないため)。
/// - `{` `}` は単独行。
/// - 行末の `\r` は落とす (CRLF が基本、稀に LF / CR のみ)。
pub fn lex(text: &str) -> Vec<Block> {
    let _ = text;
    todo!("text::lex")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_shift_jis_by_default() {
        // 「あ」= 0x82 0xA0 (CP932)
        assert_eq!(decode(&[0x82, 0xA0]), "あ");
    }

    #[test]
    fn charset_line_switches_to_utf8() {
        let sjis_bytes = b"charset,Shift_JIS\r\nname,\x82\xA0";
        assert!(decode(sjis_bytes).ends_with("あ"));

        let utf8_bytes = "charset,UTF-8\r\nname,あ".as_bytes();
        assert!(decode(utf8_bytes).ends_with("あ"));
    }

    #[test]
    fn charset_value_spelling_is_normalized() {
        for spelling in ["UTF-8", "utf8", "Utf_8", "UTF8"] {
            let raw = format!("charset,{spelling}\nname,あ");
            assert!(decode(raw.as_bytes()).ends_with("あ"), "{spelling}");
        }
        // Shift_JIS 系はどう綴っても UTF-8 とは判定されない。
        for spelling in ["Shift_JIS", "shift-jis", "sjis"] {
            let raw = format!("charset,{spelling}\n");
            assert!(!charset_says_utf8(raw.as_bytes()), "{spelling}");
        }
    }

    #[test]
    fn bom_wins_over_charset_line() {
        // BOM 付き UTF-8 なのに charset,Shift_JIS と書かれた不整合ファイル。
        let mut bytes = BOM.to_vec();
        bytes.extend_from_slice("charset,Shift_JIS\nname,あ".as_bytes());
        assert!(decode(&bytes).ends_with("あ"));
    }

    #[test]
    fn broken_bytes_do_not_panic() {
        // 単独のリードバイトで終わる壊れた CP932。U+FFFD に置換されて返る。
        assert!(!decode(&[0x82]).is_empty());
    }
}

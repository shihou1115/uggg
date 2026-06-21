//! 漢字→ひらがな前処理 (architecture §7.5)。
//!
//! voicevox_core の OpenJtalkRc::analyze を流用して、漢字混じり日本語を AccentPhrase JSON
//! に変換し、mora.text (カタカナ) を集めてひらがなへ落とす。Irodori-TTS (M4c) 用。
//! voicevox_core 自身の TTS は内部で読み解析を行うので前処理は不要。

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::tts::voicevox::VoicevoxEngine;

#[derive(Debug, Deserialize)]
struct AccentPhrase {
    moras: Vec<Mora>,
    #[allow(dead_code)]
    accent: i64,
    pause_mora: Option<Mora>,
}

#[derive(Debug, Deserialize)]
struct Mora {
    /// カタカナ表記 (例: "コ", "ン", "ニ", "チ", "ハ")。pause_mora の場合は空文字。
    text: String,
}

/// 漢字混じりテキスト → ひらがな + 句切れ「、」入り文字列。
/// engine の Open JTalk を使うので資産がロード済みであることが前提。
pub fn to_hiragana(engine: &VoicevoxEngine, text: &str) -> Result<String> {
    let json = engine
        .openjtalk_analyze(text)
        .map_err(|e| anyhow!("OpenJTalk analyze 失敗: {e}"))?;
    let phrases: Vec<AccentPhrase> =
        serde_json::from_str(&json).context("AccentPhrase JSON パース失敗")?;
    let mut out = String::with_capacity(text.len() * 2);
    for phrase in phrases {
        for mora in &phrase.moras {
            if mora.text.is_empty() {
                continue;
            }
            for ch in mora.text.chars() {
                out.push(katakana_to_hiragana(ch));
            }
        }
        if phrase.pause_mora.is_some() {
            out.push('、');
        }
    }
    Ok(out)
}

/// カタカナ 1 文字をひらがな 1 文字に変換 (それ以外はそのまま)。
fn katakana_to_hiragana(c: char) -> char {
    let code = c as u32;
    // カタカナブロック ァ(0x30A1) 〜 ン(0x30F3) はひらがな ぁ(0x3041)〜ん(0x3093) に -0x60 で対応。
    if (0x30A1..=0x30F6).contains(&code) {
        char::from_u32(code - 0x60).unwrap_or(c)
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_katakana_chars() {
        assert_eq!(katakana_to_hiragana('ア'), 'あ');
        assert_eq!(katakana_to_hiragana('ン'), 'ん');
        assert_eq!(katakana_to_hiragana('ヴ'), 'ゔ');
        // 非カタカナはそのまま
        assert_eq!(katakana_to_hiragana('a'), 'a');
        assert_eq!(katakana_to_hiragana('、'), '、');
    }

    /// AccentPhrase 風の JSON をパースしてひらがな化までの経路を確認 (engine 不要)。
    #[test]
    fn parse_accent_phrases_json() {
        let json = r#"[
          {"moras":[{"text":"コ"},{"text":"ン"},{"text":"ニ"},{"text":"チ"},{"text":"ハ"}],"accent":5,"pause_mora":null},
          {"moras":[{"text":"セ"},{"text":"カ"},{"text":"イ"}],"accent":1,"pause_mora":{"text":""}}
        ]"#;
        let phrases: Vec<AccentPhrase> = serde_json::from_str(json).unwrap();
        let mut out = String::new();
        for p in phrases {
            for m in p.moras {
                for c in m.text.chars() {
                    out.push(katakana_to_hiragana(c));
                }
            }
            if p.pause_mora.is_some() {
                out.push('、');
            }
        }
        assert_eq!(out, "こんにちはせかい、");
    }
}

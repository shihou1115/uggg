//! 漢字→ひらがな前処理 (architecture §7.5)。
//!
//! voicevox_core の OpenJtalkRc::analyze を流用して、漢字混じり日本語を AccentPhrase JSON
//! に変換し、mora.text (カタカナ) を集めてひらがなへ落とす。Irodori-TTS (M4c) 用。
//! voicevox_core 自身の TTS は内部で読み解析を行うので前処理は不要。
//!
//! さらに Irodori-TTS V3 の絵文字アノテーション (感情・スタイル・効果音制御) を保護する。
//! OpenJtalk 解析は mora を持たない文字 (記号・絵文字) を落とすため、素通しすると
//! アノテーションが消える。`to_hiragana_preserving_emoji` はテキストを対応絵文字と
//! 通常テキストのセグメントに分割し、テキスト部分だけをかな化して位置を保って再結合する。

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::tts::voicevox::VoicevoxEngine;

/// Irodori-TTS V3 が解釈する絵文字アノテーション一覧。
/// 出典: https://huggingface.co/Aratako/Irodori-TTS-500M-v3/blob/main/EMOJI_ANNOTATIONS.md (45 種)。
/// upstream で追加されたらここに追従する。
///
/// マッチは**最長一致**で行うこと: `😮‍💨` (U+1F62E U+200D U+1F4A8 の ZWJ シーケンス) は
/// 接頭辞に `😮` (U+1F62E) を含むため、配列順に依存せず常に長い候補を優先する
/// (`match_irodori_emoji` が保証)。`⏸️` `🌬️` は VS16 (U+FE0F) 付きで 1 絵文字。
const IRODORI_EMOJIS: [&str; 45] = [
    "👂",       // 囁き、耳元の音
    "😮\u{200D}💨", // 吐息、溜息、寝息 (ZWJ シーケンス)
    "⏸\u{FE0F}", // 間、沈黙
    "🤭",       // 笑い (くすくす、含み笑い)
    "🥵",       // 喘ぎ、うめき声、唸り声
    "📢",       // エコー、リバーブ
    "😏",       // からかうように、甘えるように
    "🥺",       // 声を震わせながら、自信のなさげに
    "🌬\u{FE0F}", // 息切れ、荒い息遣い、呼吸音
    "😮",       // 息をのむ
    "👅",       // 舐める音、咀嚼音、水音
    "💋",       // リップノイズ
    "🫶",       // 優しく
    "😭",       // 嗚咽、泣き声、悲しみ
    "😱",       // 悲鳴、叫び、絶叫
    "😪",       // 眠そうに、気だるげに
    "😴",       // 寝言、いびき
    "⏩",       // 早口、一気にまくしたてる、急いで
    "📞",       // 電話越し、スピーカー越しのような音
    "🐢",       // ゆっくりと
    "🥤",       // 唾を飲み込む音
    "🤧",       // 咳き込み、鼻をすする、くしゃみ、咳払い
    "😒",       // 舌打ち
    "😰",       // 慌てて、動揺、緊張、どもり
    "😆",       // 喜びながら
    "💥",       // 勢いよく、勢いに任せて
    "😠",       // 怒り、不満げに、拗ねながら
    "😲",       // 驚き、感嘆
    "🥱",       // あくび
    "😖",       // 苦しげに
    "😟",       // 心配そうに
    "🫣",       // 恥ずかしそうに、照れながら
    "🙄",       // 呆れたように
    "😊",       // 楽しげに、嬉しそうに
    "😎",       // 得意げに、自信ありげに
    "👌",       // 相槌、頷く音
    "🙏",       // 懇願するように
    "🥴",       // 酔っ払って
    "🎵",       // 鼻歌
    "🤐",       // 口を塞がれて
    "😌",       // 安堵、満足げに
    "🤔",       // 疑問の声
    "💪",       // 力を込めて、力強く
    "👃",       // 匂いを嗅ぐ音
    "📖",       // ナレーション、独白、モノローグ
];

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

/// テキスト先頭が Irodori 対応絵文字なら、**最長一致**でその絵文字を返す (pure)。
/// `😮‍💨` のような ZWJ シーケンスが接頭辞 `😮` に食われないよう、一致した候補のうち
/// 最も長いものを選ぶ。
fn match_irodori_emoji(s: &str) -> Option<&'static str> {
    IRODORI_EMOJIS
        .iter()
        .filter(|e| s.starts_with(**e))
        .max_by_key(|e| e.len())
        .copied()
}

/// 分割結果のセグメント (pure ロジックのテスト用に公開構造)。
#[derive(Debug, PartialEq)]
enum Segment {
    /// かな化対象の通常テキスト (対応外の絵文字・記号もここに含まれ、解析で自然に落ちる)。
    Text(String),
    /// Irodori 対応絵文字。無変換で合成テキストへ残す。連続使用 (強調) は連続セグメントになる。
    Emoji(&'static str),
}

/// テキストを Irodori 対応絵文字と通常テキストのセグメント列に分割する (pure)。
/// 分割セグメントを順に連結すると、対応絵文字と通常テキストの範囲では元テキストに一致する。
fn split_emoji_segments(text: &str) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut buf = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(emoji) = match_irodori_emoji(rest) {
            if !buf.is_empty() {
                segments.push(Segment::Text(std::mem::take(&mut buf)));
            }
            segments.push(Segment::Emoji(emoji));
            rest = &rest[emoji.len()..];
        } else {
            // 次の 1 文字 (char 境界) を通常テキストへ
            let ch = rest.chars().next().expect("non-empty rest");
            buf.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    if !buf.is_empty() {
        segments.push(Segment::Text(buf));
    }
    segments
}

/// 漢字混じりテキスト → ひらがな + 句切れ「、」入り文字列。**Irodori-TTS V3 の絵文字
/// アノテーションは位置を保って無変換で残す** (spec §4.5.1 / architecture §7.5)。
///
/// セグメント単位で `to_hiragana` を呼ぶため、絵文字を跨いだ読み解析コンテキストは
/// 切れるが、アノテーションは文末・句読点近くに置かれるのが通例なので実用上問題ない。
/// 1 セグメントでも解析に失敗したら全体を Err にし、呼び出し側の raw テキスト
/// フォールバックへ委ねる。
pub fn to_hiragana_preserving_emoji(engine: &VoicevoxEngine, text: &str) -> Result<String> {
    let segments = split_emoji_segments(text);
    let mut out = String::with_capacity(text.len() * 2);
    for seg in segments {
        match seg {
            Segment::Emoji(e) => out.push_str(e),
            Segment::Text(t) => {
                // 空白のみのセグメントは解析に回さない (無駄な analyze 回避)
                if t.trim().is_empty() {
                    continue;
                }
                out.push_str(&to_hiragana(engine, &t)?);
            }
        }
    }
    Ok(out)
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

    // === 絵文字アノテーション保護 (split_emoji_segments は pure、engine 不要) ===

    fn text(s: &str) -> Segment {
        Segment::Text(s.to_string())
    }

    #[test]
    fn split_no_emoji_returns_single_text_segment() {
        assert_eq!(split_emoji_segments("こんにちは世界"), vec![text("こんにちは世界")]);
    }

    #[test]
    fn split_single_emoji_mid_text() {
        assert_eq!(
            split_emoji_segments("こんにちは😊世界"),
            vec![text("こんにちは"), Segment::Emoji("😊"), text("世界")]
        );
    }

    #[test]
    fn split_repeated_emoji_kept_consecutively() {
        // upstream: 同一絵文字の連続は効果の強調。分割してもすべて残る
        assert_eq!(
            split_emoji_segments("🤧🤧ごめんね"),
            vec![Segment::Emoji("🤧"), Segment::Emoji("🤧"), text("ごめんね")]
        );
    }

    #[test]
    fn split_zwj_sequence_not_broken_into_prefix() {
        // 😮‍💨 (U+1F62E U+200D U+1F4A8) を接頭辞の 😮 (U+1F62E) に誤マッチさせない
        let segs = split_emoji_segments("😮‍💨ふう");
        assert_eq!(segs, vec![Segment::Emoji("😮\u{200D}💨"), text("ふう")]);
    }

    #[test]
    fn split_bare_gasp_emoji_still_matches() {
        // ZWJ 無しの 😮 単体は「息をのむ」として単独マッチ
        assert_eq!(
            split_emoji_segments("😮えっ"),
            vec![Segment::Emoji("😮"), text("えっ")]
        );
    }

    #[test]
    fn split_vs16_emojis_match_as_one() {
        // ⏸️ / 🌬️ は VS16 (U+FE0F) 込みで 1 絵文字
        assert_eq!(
            split_emoji_segments("待って⏸\u{FE0F}ね"),
            vec![text("待って"), Segment::Emoji("⏸\u{FE0F}"), text("ね")]
        );
        assert_eq!(
            split_emoji_segments("🌬\u{FE0F}はぁ"),
            vec![Segment::Emoji("🌬\u{FE0F}"), text("はぁ")]
        );
    }

    #[test]
    fn split_emoji_only_input() {
        assert_eq!(split_emoji_segments("😭"), vec![Segment::Emoji("😭")]);
    }

    #[test]
    fn split_unsupported_emoji_stays_in_text() {
        // 🍕 は Irodori 非対応 → Text 側に残り、かな化で従来通り落ちる
        assert_eq!(split_emoji_segments("ピザ🍕たべたい"), vec![text("ピザ🍕たべたい")]);
    }

    #[test]
    fn split_leading_and_trailing_emoji() {
        assert_eq!(
            split_emoji_segments("😊おはよう😴"),
            vec![Segment::Emoji("😊"), text("おはよう"), Segment::Emoji("😴")]
        );
    }

    #[test]
    fn split_upstream_example_sentence() {
        // upstream モデルカードの実例文
        assert_eq!(
            split_emoji_segments("うぅ…😭そんなに酷いこと、言わないで…😭"),
            vec![
                text("うぅ…"),
                Segment::Emoji("😭"),
                text("そんなに酷いこと、言わないで…"),
                Segment::Emoji("😭"),
            ]
        );
    }

    #[test]
    fn split_roundtrip_reconstructs_original() {
        // 不変条件: セグメントを順に連結すると元のテキストに戻る
        let cases = [
            "こんにちは😊世界",
            "🤧🤧ごめんね、風邪引いちゃってて🤧…大丈夫、ただの風邪だからすぐ治るよ🥺",
            "😮‍💨😮ダブル",
            "絵文字なしのふつうの文",
            "",
        ];
        for case in cases {
            let joined: String = split_emoji_segments(case)
                .iter()
                .map(|s| match s {
                    Segment::Text(t) => t.as_str(),
                    Segment::Emoji(e) => e,
                })
                .collect();
            assert_eq!(joined, case, "roundtrip failed for {case:?}");
        }
    }

    #[test]
    fn match_longest_wins_regardless_of_order() {
        // match_irodori_emoji が配列順に依存しないことの直接確認
        assert_eq!(match_irodori_emoji("😮‍💨x"), Some("😮\u{200D}💨"));
        assert_eq!(match_irodori_emoji("😮x"), Some("😮"));
        assert_eq!(match_irodori_emoji("abc"), None);
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

//! 台本形式 (upstream Irodori-TTS 互換) のパース (docs/script-reader-spec.md §2.3)。
//!
//! Markdown 内の ` ```json speakers ` / ` ```json lines ` (必須) / ` ```json defaults ` (任意)
//! コードブロックをフェンスの行スキャンで抽出し、`serde_json` でパースして検証する。
//! full Markdown パーサは使わない (依存追加なし、T1)。
//!
//! 1 台本行 = 1 `ReadingChunk`。120 字超の長行分割は `reader.rs` 側の後段処理
//! (T5, Step P1-2) が担当し、本モジュールでは行わない。

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::tts::reader::{ReadingChunk, VoiceSlot, DEFAULT_PAUSE_MS};

/// 台本パース・検証のエラー種別 (docs/script-reader-spec.md §2.8)。
#[derive(Debug, Error, PartialEq)]
pub enum ScriptError {
    /// speakers / lines ブロックが 1 つも認識できない (専用文言)。
    NotAScript,
    UnclosedFence,
    DuplicateBlock(&'static str),
    InvalidJson {
        block: &'static str,
        line_in_block: usize,
        file_line: usize,
        detail: String,
    },
    UnsupportedRefWav {
        speaker_id: String,
    },
    InvalidSlot {
        speaker_id: String,
        slot: String,
    },
    UnknownSpeaker {
        index: usize,
        speaker: String,
    },
    EmptyText {
        index: usize,
    },
    /// speed / pause_after / defaults の範囲外。`index` は lines の位置 (defaults は None)。
    OutOfRange {
        index: Option<usize>,
        key: &'static str,
        value: f64,
    },
    CaptionTooLong {
        index: usize,
        len: usize,
    },
    InvalidSpeakerId {
        id: String,
    },
    EmptyLines,
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScriptError::NotAScript => write!(
                f,
                "台本形式ではありません (speakers / lines ブロックが必要です)。通常の Markdown の読み上げには対応していません"
            ),
            ScriptError::UnclosedFence => write!(f, "コードブロックが閉じられていません (``` が不足しています)"),
            ScriptError::DuplicateBlock(name) => write!(f, "```json {name} ブロックが複数あります (1 つにまとめてください)"),
            ScriptError::InvalidJson { block, line_in_block, file_line, detail } => write!(
                f,
                "```json {block} ブロックの JSON が不正です (ブロック内 {line_in_block} 行目 / ファイル {file_line} 行目): {detail}"
            ),
            ScriptError::UnsupportedRefWav { speaker_id } => write!(
                f,
                "話者 {speaker_id} で ref_wav が指定されています。ugg では外部 WAV の参照 (ref_wav) をサポートしていません。代わりに slot キー (\"main\" または \"sub\") を指定してください"
            ),
            ScriptError::InvalidSlot { speaker_id, slot } => write!(
                f,
                "話者 {speaker_id} の slot \"{slot}\" は無効です (\"main\" または \"sub\" を指定してください)"
            ),
            ScriptError::UnknownSpeaker { index, speaker } => write!(
                f,
                "lines[{index}].speaker \"{speaker}\" は speakers に定義されていません"
            ),
            ScriptError::EmptyText { index } => write!(f, "lines[{index}].text が空です"),
            ScriptError::OutOfRange { index, key, value } => match index {
                Some(i) => write!(f, "lines[{i}].{key} の値 {value} が範囲外です"),
                None => write!(f, "defaults.{key} の値 {value} が範囲外です"),
            },
            ScriptError::CaptionTooLong { index, len } => write!(
                f,
                "lines[{index}].caption が長すぎます ({len} 文字 / 上限 200 文字)"
            ),
            ScriptError::InvalidSpeakerId { id } => write!(
                f,
                "話者 ID \"{id}\" が不正です (空・空白のみ・前後に空白のある ID は使用できません)"
            ),
            ScriptError::EmptyLines => write!(f, "lines 配列が空です"),
        }
    }
}

/// フェンス抽出で見つかったブロック本文と、元ファイル内での開始行番号 (1-origin、
/// フェンス行自体の次の行)。
struct FencedBlock {
    body: String,
    /// 元ファイル内でのブロック本文 1 行目の行番号 (1-origin)。
    start_file_line: usize,
}

const BLOCK_NAMES: [&str; 3] = ["defaults", "speakers", "lines"];

fn fence_open_line(name: &str) -> String {
    format!("```json {name}")
}

/// フェンスを行スキャンで抽出する (docs/script-reader-spec.md §2.3)。
fn extract_fenced_blocks(content: &str) -> Result<BTreeMap<&'static str, FencedBlock>, ScriptError> {
    // BOM 除去
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    // CRLF/LF 両対応: 行分割時に \r を除去
    let lines: Vec<&str> = content.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();

    let mut blocks: BTreeMap<&'static str, FencedBlock> = BTreeMap::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        let matched_name = BLOCK_NAMES.iter().find(|name| trimmed == fence_open_line(name));
        if let Some(&name) = matched_name {
            let body_start_line_idx = i + 1; // 0-origin index of first body line
            let mut j = body_start_line_idx;
            let mut closed = false;
            let mut body_lines: Vec<&str> = Vec::new();
            while j < lines.len() {
                if lines[j].trim() == "```" {
                    closed = true;
                    break;
                }
                body_lines.push(lines[j]);
                j += 1;
            }
            if !closed {
                return Err(ScriptError::UnclosedFence);
            }
            if blocks.contains_key(name) {
                return Err(ScriptError::DuplicateBlock(name));
            }
            blocks.insert(
                name,
                FencedBlock {
                    body: body_lines.join("\n"),
                    start_file_line: body_start_line_idx + 1, // 1-origin
                },
            );
            i = j + 1;
        } else {
            i += 1;
        }
    }
    Ok(blocks)
}

/// serde_json のエラー位置 (ブロック内相対 1-origin 行) から元ファイルの行番号を算出する。
fn parse_json_block<T: for<'de> Deserialize<'de>>(
    block_name: &'static str,
    block: &FencedBlock,
) -> Result<T, ScriptError> {
    serde_json::from_str::<T>(&block.body).map_err(|e| {
        let line_in_block = e.line();
        let file_line = block.start_file_line + line_in_block.saturating_sub(1);
        ScriptError::InvalidJson {
            block: block_name,
            line_in_block,
            file_line,
            detail: e.to_string(),
        }
    })
}

#[derive(Debug, Deserialize)]
struct DefaultsDef {
    #[serde(default)]
    default_pause_seconds: Option<f64>,
    #[serde(default)]
    speed: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct SpeakerDef {
    #[serde(default)]
    slot: Option<String>,
    #[serde(default)]
    ref_wav: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct LineDef {
    speaker: String,
    text: String,
    #[serde(default)]
    speed: Option<f64>,
    #[serde(default)]
    caption: Option<String>,
    #[serde(default)]
    pause_after: Option<f64>,
}

fn validate_pause_range(index: Option<usize>, key: &'static str, value: f64) -> Result<(), ScriptError> {
    if !(0.0..=10.0).contains(&value) {
        return Err(ScriptError::OutOfRange { index, key, value });
    }
    Ok(())
}

fn validate_speed_range(index: Option<usize>, key: &'static str, value: f64) -> Result<(), ScriptError> {
    if !(-1.0..=1.0).contains(&value) {
        return Err(ScriptError::OutOfRange { index, key, value });
    }
    Ok(())
}

/// 秒 → ミリ秒 (docs/script-reader-spec.md §2.6)。値は事前に [0, 10] 検証済みのため
/// オーバーフローしない。
fn seconds_to_ms(seconds: f64) -> u32 {
    (seconds * 1000.0).round() as u32
}

/// caption の有効判定・正規化 (§2.3): 空文字・空白のみは None、201 文字以上はエラー。
fn normalize_caption(index: usize, caption: Option<String>) -> Result<Option<String>, ScriptError> {
    let Some(raw) = caption else { return Ok(None) };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let len = raw.chars().count();
    if len > 200 {
        return Err(ScriptError::CaptionTooLong { index, len });
    }
    Ok(Some(raw))
}

/// 台本 (Markdown + JSON ブロック) をパースし、`ReadingChunk` 列を返す
/// (docs/script-reader-spec.md §2.3〜§2.8)。1 台本行 = 1 `ReadingChunk`。
pub(crate) fn parse_script(content: &str) -> Result<Vec<ReadingChunk>, ScriptError> {
    let blocks = extract_fenced_blocks(content)?;

    // エラー優先順位 (§2.8): 認識済みブロックが 1 つ以上あれば、speakers/lines 欠落より
    // JSON 不正を優先して報告する。1 つも無ければ NotAScript。
    if blocks.is_empty() {
        return Err(ScriptError::NotAScript);
    }

    let defaults: DefaultsDef = match blocks.get("defaults") {
        Some(b) => parse_json_block("defaults", b)?,
        None => DefaultsDef { default_pause_seconds: None, speed: None },
    };

    // §2.8: 認識済みブロックの JSON 不正は必須ブロック欠落 (NotAScript) より優先する。
    // 存在するブロックを先に JSON パースし、その後で必須ブロックの充足を判定する。
    let speakers: Option<BTreeMap<String, SpeakerDef>> = match blocks.get("speakers") {
        Some(b) => Some(parse_json_block("speakers", b)?),
        None => None,
    };
    let lines: Option<Vec<LineDef>> = match blocks.get("lines") {
        Some(b) => Some(parse_json_block("lines", b)?),
        None => None,
    };
    let (Some(speakers), Some(lines)) = (speakers, lines) else {
        return Err(ScriptError::NotAScript);
    };

    if let Some(seconds) = defaults.default_pause_seconds {
        validate_pause_range(None, "default_pause_seconds", seconds)?;
    }
    if let Some(speed) = defaults.speed {
        validate_speed_range(None, "speed", speed)?;
    }
    let default_speed = defaults.speed.unwrap_or(0.0);
    let default_pause_ms = defaults
        .default_pause_seconds
        .map(seconds_to_ms)
        .unwrap_or(DEFAULT_PAUSE_MS);

    // speakers 検証・slot マップ構築
    let mut slot_by_speaker: BTreeMap<String, VoiceSlot> = BTreeMap::new();
    for (id, def) in &speakers {
        if id != id.trim() || id.is_empty() {
            return Err(ScriptError::InvalidSpeakerId { id: id.clone() });
        }
        if def.ref_wav.is_some() {
            return Err(ScriptError::UnsupportedRefWav { speaker_id: id.clone() });
        }
        let slot_str = def.slot.clone().unwrap_or_default();
        let slot = match slot_str.as_str() {
            "main" => VoiceSlot::Main,
            "sub" => VoiceSlot::Sub,
            _ => {
                return Err(ScriptError::InvalidSlot {
                    speaker_id: id.clone(),
                    slot: slot_str,
                })
            }
        };
        slot_by_speaker.insert(id.clone(), slot);
    }

    if lines.is_empty() {
        return Err(ScriptError::EmptyLines);
    }

    let mut chunks = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        let slot = *slot_by_speaker.get(&line.speaker).ok_or_else(|| ScriptError::UnknownSpeaker {
            index,
            speaker: line.speaker.clone(),
        })?;

        if line.text.trim().is_empty() {
            return Err(ScriptError::EmptyText { index });
        }

        if let Some(speed) = line.speed {
            validate_speed_range(Some(index), "speed", speed)?;
        }
        let speed_offset = line.speed.unwrap_or(default_speed);

        if let Some(pause) = line.pause_after {
            validate_pause_range(Some(index), "pause_after", pause)?;
        }
        let pause_after_ms = line.pause_after.map(seconds_to_ms).unwrap_or(default_pause_ms);

        let caption = normalize_caption(index, line.caption.clone())?;

        chunks.push(ReadingChunk {
            text: line.text.clone(),
            slot,
            speed_offset,
            caption,
            pause_after_ms,
        });
    }

    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(content: &str) -> Vec<ReadingChunk> {
        parse_script(content).unwrap_or_else(|e| panic!("expected Ok, got Err: {e:?}"))
    }

    fn err(content: &str) -> ScriptError {
        parse_script(content).expect_err("expected Err")
    }

    const BASIC: &str = r#"
```json speakers
{ "host": { "slot": "main" }, "guest": { "slot": "sub" } }
```
```json lines
[
  { "speaker": "host",  "text": "ねえ、聞いた？" },
  { "speaker": "guest", "text": "知ってるわ。", "pause_after": 0.6, "speed": 0.1 },
  { "speaker": "host",  "text": "えええ！！", "caption": "驚いて大声で" }
]
```
"#;

    // 1. 正常系
    #[test]
    fn test01_basic_script_parses_all_fields() {
        let chunks = ok(BASIC);
        assert_eq!(chunks.len(), 3);

        assert_eq!(chunks[0].text, "ねえ、聞いた？");
        assert_eq!(chunks[0].slot, VoiceSlot::Main);
        assert_eq!(chunks[0].speed_offset, 0.0);
        assert_eq!(chunks[0].caption, None);
        assert_eq!(chunks[0].pause_after_ms, 500);

        assert_eq!(chunks[1].text, "知ってるわ。");
        assert_eq!(chunks[1].slot, VoiceSlot::Sub);
        assert_eq!(chunks[1].speed_offset, 0.1);
        assert_eq!(chunks[1].pause_after_ms, 600);

        assert_eq!(chunks[2].text, "えええ！！");
        assert_eq!(chunks[2].slot, VoiceSlot::Main);
        assert_eq!(chunks[2].caption.as_deref(), Some("驚いて大声で"));
    }

    // 2. defaults 省略 → speed 0 / pause 500ms
    #[test]
    fn test02_missing_defaults_uses_speed0_pause500() {
        let content = r#"
```json speakers
{ "host": { "slot": "main" } }
```
```json lines
[ { "speaker": "host", "text": "こんにちは" } ]
```
"#;
        let chunks = ok(content);
        assert_eq!(chunks[0].speed_offset, 0.0);
        assert_eq!(chunks[0].pause_after_ms, 500);
    }

    // 3. line.speed が defaults.speed を上書き (加算しない)
    #[test]
    fn test03_line_speed_overrides_defaults_not_additive() {
        let content = r#"
```json defaults
{ "speed": -0.1 }
```
```json speakers
{ "host": { "slot": "main" } }
```
```json lines
[
  { "speaker": "host", "text": "デフォルト適用" },
  { "speaker": "host", "text": "上書き", "speed": 0.3 }
]
```
"#;
        let chunks = ok(content);
        assert_eq!(chunks[0].speed_offset, -0.1);
        assert_eq!(chunks[1].speed_offset, 0.3);
    }

    // 4. speakers 未定義の話者 ID → UnknownSpeaker (index 付き)
    #[test]
    fn test04_unknown_speaker_reports_index() {
        let content = r#"
```json speakers
{ "host": { "slot": "main" } }
```
```json lines
[
  { "speaker": "host", "text": "ok" },
  { "speaker": "ghost", "text": "誰？" }
]
```
"#;
        assert_eq!(
            err(content),
            ScriptError::UnknownSpeaker { index: 1, speaker: "ghost".to_string() }
        );
    }

    // 5. ref_wav 指定 → UnsupportedRefWav
    #[test]
    fn test05_ref_wav_is_rejected() {
        let content = r#"
```json speakers
{ "host": { "ref_wav": "voice.wav" } }
```
```json lines
[ { "speaker": "host", "text": "ok" } ]
```
"#;
        assert_eq!(err(content), ScriptError::UnsupportedRefWav { speaker_id: "host".to_string() });
    }

    // 6. slot が main/sub 以外 → InvalidSlot
    #[test]
    fn test06_invalid_slot_value() {
        let content = r#"
```json speakers
{ "host": { "slot": "narrator" } }
```
```json lines
[ { "speaker": "host", "text": "ok" } ]
```
"#;
        assert_eq!(
            err(content),
            ScriptError::InvalidSlot { speaker_id: "host".to_string(), slot: "narrator".to_string() }
        );
    }

    // 7. speakers / lines ブロック欠落 → NotAScript、同名ブロック重複 → DuplicateBlock
    #[test]
    fn test07_missing_blocks_is_not_a_script() {
        let content = "# ただの Markdown\n\n本文だけです。";
        assert_eq!(err(content), ScriptError::NotAScript);
    }

    #[test]
    fn test07_duplicate_block_is_error() {
        let content = r#"
```json speakers
{ "host": { "slot": "main" } }
```
```json speakers
{ "host": { "slot": "main" } }
```
```json lines
[ { "speaker": "host", "text": "ok" } ]
```
"#;
        assert_eq!(err(content), ScriptError::DuplicateBlock("speakers"));
    }

    // 8. text 空文字列・空白のみ → EmptyText。未知キーは無視で通る
    #[test]
    fn test08_empty_text_variants() {
        let content = r#"
```json speakers
{ "host": { "slot": "main" } }
```
```json lines
[ { "speaker": "host", "text": "   " } ]
```
"#;
        assert_eq!(err(content), ScriptError::EmptyText { index: 0 });
    }

    #[test]
    fn test08_unknown_keys_are_ignored() {
        let content = r#"
```json defaults
{ "speed": 0.0, "unknown_default_key": 123 }
```
```json speakers
{ "host": { "slot": "main", "voice_color": "blue" } }
```
```json lines
[ { "speaker": "host", "text": "ok", "extra_key": true } ]
```
"#;
        let chunks = ok(content);
        assert_eq!(chunks.len(), 1);
    }

    // 9. pause_after / default_pause_seconds 範囲外 → OutOfRange (clamp しない)
    #[test]
    fn test09_pause_after_out_of_range() {
        let content = r#"
```json speakers
{ "host": { "slot": "main" } }
```
```json lines
[ { "speaker": "host", "text": "ok", "pause_after": 10.1 } ]
```
"#;
        assert_eq!(
            err(content),
            ScriptError::OutOfRange { index: Some(0), key: "pause_after", value: 10.1 }
        );
    }

    #[test]
    fn test09_default_pause_seconds_out_of_range() {
        let content = r#"
```json defaults
{ "default_pause_seconds": -0.1 }
```
```json speakers
{ "host": { "slot": "main" } }
```
```json lines
[ { "speaker": "host", "text": "ok" } ]
```
"#;
        assert_eq!(
            err(content),
            ScriptError::OutOfRange { index: None, key: "default_pause_seconds", value: -0.1 }
        );
    }

    // 10. speed 範囲外 → OutOfRange
    #[test]
    fn test10_speed_out_of_range() {
        let content = r#"
```json speakers
{ "host": { "slot": "main" } }
```
```json lines
[ { "speaker": "host", "text": "ok", "speed": 1.5 } ]
```
"#;
        assert_eq!(err(content), ScriptError::OutOfRange { index: Some(0), key: "speed", value: 1.5 });
    }

    // 11. caption 201 文字 → CaptionTooLong。caption 空文字・空白のみ → None として通る
    #[test]
    fn test11_caption_too_long() {
        let caption = "あ".repeat(201);
        let content = format!(
            r#"
```json speakers
{{ "host": {{ "slot": "main" }} }}
```
```json lines
[ {{ "speaker": "host", "text": "ok", "caption": "{caption}" }} ]
```
"#
        );
        assert_eq!(err(&content), ScriptError::CaptionTooLong { index: 0, len: 201 });
    }

    #[test]
    fn test11_blank_caption_becomes_none() {
        let content = r#"
```json speakers
{ "host": { "slot": "main" } }
```
```json lines
[
  { "speaker": "host", "text": "ok1", "caption": "" },
  { "speaker": "host", "text": "ok2", "caption": "   " }
]
```
"#;
        let chunks = ok(content);
        assert_eq!(chunks[0].caption, None);
        assert_eq!(chunks[1].caption, None);
    }

    // 12. フェンス外の Markdown 本文が無視される。大文字小文字/余分な info string は
    //     ブロック扱いされない (→ NotAScript に到達)
    #[test]
    fn test12_prose_outside_fences_is_ignored() {
        let content = r#"# メモ

これはフリーテキストです。```インラインではない```

```json speakers
{ "host": { "slot": "main" } }
```

さらに自由な本文。

```json lines
[ { "speaker": "host", "text": "ok" } ]
```

末尾のメモ。
"#;
        let chunks = ok(content);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn test12_case_and_extra_info_string_not_recognized() {
        let content = r#"
```JSON lines
[ { "speaker": "host", "text": "ok" } ]
```
"#;
        assert_eq!(err(content), ScriptError::NotAScript);
    }

    #[test]
    fn test12_extra_info_string_suffix_not_recognized() {
        let content = r#"
```json lines extra
[ { "speaker": "host", "text": "ok" } ]
```
"#;
        assert_eq!(err(content), ScriptError::NotAScript);
    }

    // 13. 未閉じフェンス → UnclosedFence
    #[test]
    fn test13_unclosed_fence() {
        let content = r#"
```json speakers
{ "host": { "slot": "main" } }
```
```json lines
[ { "speaker": "host", "text": "ok" } ]
"#;
        assert_eq!(err(content), ScriptError::UnclosedFence);
    }

    // 14. JSON 構文エラー → InvalidJson にブロック名 + 元ファイル行番号
    #[test]
    fn test14_json_syntax_error_reports_block_and_file_line() {
        let content = "line1\nline2\n```json speakers\n{ invalid json here\n```\n```json lines\n[ { \"speaker\": \"host\", \"text\": \"ok\" } ]\n```\n";
        match err(content) {
            ScriptError::InvalidJson { block, file_line, .. } => {
                assert_eq!(block, "speakers");
                // ブロック本文の 1 行目はファイルの 4 行目 (1-origin)
                assert_eq!(file_line, 4);
            }
            other => panic!("expected InvalidJson, got {other:?}"),
        }
    }

    // 15. UTF-8 BOM 付き・CRLF 改行の台本が正常にパースできる
    #[test]
    fn test15_bom_and_crlf_parses_ok() {
        let body = "```json speakers\r\n{ \"host\": { \"slot\": \"main\" } }\r\n```\r\n```json lines\r\n[ { \"speaker\": \"host\", \"text\": \"ok\" } ]\r\n```\r\n";
        let mut content = String::from('\u{feff}');
        content.push_str(body);
        let chunks = ok(&content);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "ok");
    }

    // 16. 話者 ID の異常系: 空文字 / 空白のみ / 前後空白付き → InvalidSpeakerId
    #[test]
    fn test16_invalid_speaker_id_whitespace_padded() {
        let content = r#"
```json speakers
{ " host": { "slot": "main" } }
```
```json lines
[ { "speaker": "host", "text": "ok" } ]
```
"#;
        assert_eq!(err(content), ScriptError::InvalidSpeakerId { id: " host".to_string() });
    }

    #[test]
    fn test16_invalid_speaker_id_blank() {
        let content = r#"
```json speakers
{ "   ": { "slot": "main" } }
```
```json lines
[ { "speaker": "host", "text": "ok" } ]
```
"#;
        assert_eq!(err(content), ScriptError::InvalidSpeakerId { id: "   ".to_string() });
    }

    // 17. lines 配列が空 → EmptyLines。ブロック本文が空 (フェンスのみ) → InvalidJson
    #[test]
    fn test17_empty_lines_array() {
        let content = r#"
```json speakers
{ "host": { "slot": "main" } }
```
```json lines
[]
```
"#;
        assert_eq!(err(content), ScriptError::EmptyLines);
    }

    #[test]
    fn test17_empty_block_body_is_invalid_json() {
        let content = r#"
```json speakers
```
```json lines
[ { "speaker": "host", "text": "ok" } ]
```
"#;
        match err(content) {
            ScriptError::InvalidJson { block, .. } => assert_eq!(block, "speakers"),
            other => panic!("expected InvalidJson, got {other:?}"),
        }
    }

    // 18. エラー優先順位: speakers の JSON 破損 + lines 欠落 → NotAScript ではなく InvalidJson
    #[test]
    fn test18_broken_speakers_with_missing_lines_is_invalid_json_not_not_a_script() {
        let content = r#"
```json speakers
{ this is not valid json
```
"#;
        match err(content) {
            ScriptError::InvalidJson { block, .. } => assert_eq!(block, "speakers"),
            other => panic!("expected InvalidJson, got {other:?}"),
        }
    }

    // 19. pause の ms 変換丸め: 0.333 → 333 / 10.0 → 10000 / 0.001 → 1
    #[test]
    fn test19_pause_ms_rounding() {
        assert_eq!(seconds_to_ms(0.333), 333);
        assert_eq!(seconds_to_ms(10.0), 10000);
        assert_eq!(seconds_to_ms(0.001), 1);
    }

    #[test]
    fn test19_pause_ms_rounding_via_parse_script() {
        let content = r#"
```json speakers
{ "host": { "slot": "main" } }
```
```json lines
[ { "speaker": "host", "text": "ok", "pause_after": 0.333 } ]
```
"#;
        let chunks = ok(content);
        assert_eq!(chunks[0].pause_after_ms, 333);
    }
}

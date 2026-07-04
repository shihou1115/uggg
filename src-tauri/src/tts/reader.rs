//! テキスト読み上げツール (docs/text-reader-spec.md)。
//!
//! - `decode_text_file`: .txt の読込 (拡張子 / 1MB 上限 / UTF-8・Shift_JIS 自動判定)
//! - `split_reading_chunks`: 長文をチャンク分割する pure 関数
//!
//! チャンク分割は Irodori 実モデルの長文生成失敗・遅延対策 (spec K4)。文境界を優先し
//! 最大 `MAX_CHUNK_CHARS` トークンで区切る。**Irodori 絵文字アノテーションはトークン化の
//! 段階で不可分単位として扱う**ため、どの分割経路 (文/読点/強制) でも ZWJ・VS16 込みの
//! 絵文字クラスタが分断されることは構造的にない (spec T3)。
//!
//! 合成そのものは既存 `synthesize_voice` をチャンクごとに呼ぶ (フロント側)。これにより
//! 漢字→かな前処理・絵文字保護・Irodori フォールバック・通知クールダウンが自動で乗る。

use std::path::Path;

use anyhow::{anyhow, bail, Result};
use serde::Serialize;

use crate::tts::preprocess::{split_emoji_segments, Segment};

/// 台本の話者マッピング先。ugg の声スロット (docs/script-reader-spec.md §2.9)。
#[derive(Debug, Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum VoiceSlot {
    Main,
    Sub,
}

/// 読み上げの 1 チャンク (docs/script-reader-spec.md §2.9)。
/// `caption` は `skip_serializing_if` を付けない — None を `null` として常時出力し、
/// TS 側に `undefined` を考慮させない。
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct ReadingChunk {
    pub text: String,
    pub slot: VoiceSlot,
    pub speed_offset: f64,
    pub caption: Option<String>,
    pub pause_after_ms: u32,
}

/// 1 チャンクの最大トークン数 (通常文字 1 トークン、Irodori 絵文字 1 トークン)。
/// Irodori 実モデルの 1 回の生成が数秒〜十数秒に収まる実用長。実測に応じて調整する。
pub const MAX_CHUNK_CHARS: usize = 120;

/// 読み込みを許可する最大ファイルサイズ。テキスト 1MB ≒ 全角 50 万字 (朗読 10 時間超)。
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// チャンク間の既定の間 (docs/script-reader-spec.md §2.6)。.txt は全チャンク一律この値。
/// 台本 (.md) は `line.pause_after` / `defaults.default_pause_seconds` 未指定時にこの値へ解決する。
pub const DEFAULT_PAUSE_MS: u32 = 500;

/// .txt を読み込んで文字列にする。拡張子・サイズ・エンコーディングを検証する。
pub fn decode_text_file(path: &Path) -> Result<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    if ext.as_deref() != Some("txt") {
        bail!("対応していないファイル形式です (.txt のみ読み上げできます)");
    }
    read_checked(path)
}

/// .md (台本) を読み込んで文字列にする。拡張子検証は呼び出し側 (dnd 受理規約) に委ね、
/// ここではサイズ上限・エンコーディング判定のみ `decode_text_file` と共有する
/// (docs/script-reader-spec.md §2.9)。
pub fn decode_script_file(path: &Path) -> Result<String> {
    read_checked(path)
}

/// サイズ上限チェック + バイト列読込 + デコード (.txt / .md 共通、拡張子非依存)。
fn read_checked(path: &Path) -> Result<String> {
    let meta = std::fs::metadata(path)
        .map_err(|e| anyhow!("ファイルを開けません: {e}"))?;
    if meta.len() > MAX_FILE_BYTES {
        bail!(
            "ファイルが大きすぎます ({}KB / 上限 {}KB)",
            meta.len() / 1024,
            MAX_FILE_BYTES / 1024
        );
    }
    let bytes = std::fs::read(path).map_err(|e| anyhow!("読み込みに失敗: {e}"))?;
    decode_bytes(&bytes)
}

/// バイト列 → 文字列 (pure、テスト対象)。UTF-8 (BOM 有無) を先に試し、
/// 無効なら Shift_JIS でデコードする。どちらでも解釈できなければエラー。
fn decode_bytes(bytes: &[u8]) -> Result<String> {
    // UTF-8 (BOM は encoding_rs が strip する)
    let (s, _, had_errors) = encoding_rs::UTF_8.decode(bytes);
    if !had_errors {
        return Ok(s.into_owned());
    }
    let (s, _, had_errors) = encoding_rs::SHIFT_JIS.decode(bytes);
    if !had_errors {
        return Ok(s.into_owned());
    }
    bail!("文字コードを判定できません (UTF-8 / Shift_JIS のみ対応)")
}

/// チャンク分割の最小単位。通常文字は 1 トークン、Irodori 絵文字はコードポイント数に
/// かかわらず 1 トークン (不可分)。
#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ch(char),
    Emoji(&'static str),
}

impl Tok {
    fn push_to(&self, out: &mut String) {
        match self {
            Tok::Ch(c) => out.push(*c),
            Tok::Emoji(e) => out.push_str(e),
        }
    }
}

fn is_sentence_end(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '!' | '?' | '…')
}

fn tokenize(text: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    for seg in split_emoji_segments(text) {
        match seg {
            Segment::Text(t) => toks.extend(t.chars().map(Tok::Ch)),
            Segment::Emoji(e) => toks.push(Tok::Emoji(e)),
        }
    }
    toks
}

/// トークン列を文単位に分割する。
/// - 改行は文境界 (空文は捨てる)
/// - 文末記号は連続 (「！！！？」) をまとめて現在の文に含める
/// - 文末記号直後の Irodori 絵文字は**直前の文に付随**させる (「ひどいよ…😭」の 😭 が
///   次チャンクへ漏れてエモート対象がずれるのを防ぐ)
fn split_sentences(toks: &[Tok]) -> Vec<Vec<Tok>> {
    let mut sentences: Vec<Vec<Tok>> = Vec::new();
    let mut cur: Vec<Tok> = Vec::new();
    let mut i = 0;
    let flush = |cur: &mut Vec<Tok>, out: &mut Vec<Vec<Tok>>| {
        let has_content = cur.iter().any(|t| match t {
            Tok::Ch(c) => !c.is_whitespace(),
            Tok::Emoji(_) => true,
        });
        if has_content {
            out.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };
    while i < toks.len() {
        match &toks[i] {
            Tok::Ch(c) if *c == '\n' || *c == '\r' => {
                flush(&mut cur, &mut sentences);
                i += 1;
            }
            Tok::Ch(c) if is_sentence_end(*c) => {
                cur.push(toks[i].clone());
                i += 1;
                // 連続する文末記号をまとめる
                while i < toks.len() {
                    if let Tok::Ch(c2) = &toks[i] {
                        if is_sentence_end(*c2) {
                            cur.push(toks[i].clone());
                            i += 1;
                            continue;
                        }
                    }
                    break;
                }
                // 文末直後の絵文字は同じ文に付随
                while i < toks.len() {
                    if matches!(&toks[i], Tok::Emoji(_)) {
                        cur.push(toks[i].clone());
                        i += 1;
                        continue;
                    }
                    break;
                }
                flush(&mut cur, &mut sentences);
            }
            _ => {
                cur.push(toks[i].clone());
                i += 1;
            }
        }
    }
    flush(&mut cur, &mut sentences);
    sentences
}

/// 長すぎる 1 文をさらに分割する: まず読点 `、` で断片化し、それでも
/// `max` を超える断片はトークン `max` 個ごとに強制分割する。
fn split_long_sentence(sentence: &[Tok], max: usize) -> Vec<Vec<Tok>> {
    // 読点で断片化 (読点は断片の末尾に含める)
    let mut pieces: Vec<Vec<Tok>> = Vec::new();
    let mut cur: Vec<Tok> = Vec::new();
    for t in sentence {
        cur.push(t.clone());
        if matches!(t, Tok::Ch('、')) {
            pieces.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        pieces.push(cur);
    }
    // 断片がまだ長ければ強制分割 (トークン単位なので絵文字は分断されない)
    let mut out: Vec<Vec<Tok>> = Vec::new();
    for piece in pieces {
        if piece.len() <= max {
            out.push(piece);
        } else {
            for chunk in piece.chunks(max) {
                out.push(chunk.to_vec());
            }
        }
    }
    out
}

fn toks_to_string(toks: &[Tok]) -> String {
    let mut s = String::new();
    for t in toks {
        t.push_to(&mut s);
    }
    s
}

/// テキストを読み上げチャンク列に分割する (pure、docs/text-reader-spec.md §2.3)。
pub fn split_reading_chunks(text: &str) -> Vec<String> {
    split_reading_chunks_with_max(text, MAX_CHUNK_CHARS)
}

/// .txt を既定メタ付きの `ReadingChunk` 列に変換する (docs/script-reader-spec.md §2.6)。
/// 全チャンク一律 slot=Main, speed_offset=0.0, caption=None, pause_after_ms=`DEFAULT_PAUSE_MS`。
pub fn plain_text_chunks(text: &str) -> Vec<ReadingChunk> {
    split_reading_chunks(text)
        .into_iter()
        .map(|text| ReadingChunk {
            text,
            slot: VoiceSlot::Main,
            speed_offset: 0.0,
            caption: None,
            pause_after_ms: DEFAULT_PAUSE_MS,
        })
        .collect()
}

/// 120 字超の `ReadingChunk` を既存の文分割ロジックでさらに分割する
/// (docs/script-reader-spec.md T5)。slot / speed_offset / caption は全断片に複製し、
/// `pause_after_ms` は最終断片のみ元の値、中間断片は 0 にする
/// (一文の途中に不自然な間を作らないため)。120 字以下のチャンクはそのまま返す。
pub fn split_long_chunks(chunks: Vec<ReadingChunk>) -> Vec<ReadingChunk> {
    let mut out = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        if chunk.text.chars().count() <= MAX_CHUNK_CHARS {
            out.push(chunk);
            continue;
        }
        let pieces = split_reading_chunks(&chunk.text);
        let last = pieces.len().saturating_sub(1);
        for (i, piece) in pieces.into_iter().enumerate() {
            out.push(ReadingChunk {
                text: piece,
                slot: chunk.slot,
                speed_offset: chunk.speed_offset,
                caption: chunk.caption.clone(),
                pause_after_ms: if i == last { chunk.pause_after_ms } else { 0 },
            });
        }
    }
    out
}

fn split_reading_chunks_with_max(text: &str, max: usize) -> Vec<String> {
    let toks = tokenize(text);
    let sentences = split_sentences(&toks);

    let mut chunks: Vec<String> = Vec::new();
    let mut cur: Vec<Tok> = Vec::new();
    let flush = |cur: &mut Vec<Tok>, chunks: &mut Vec<String>| {
        if !cur.is_empty() {
            let s = toks_to_string(cur);
            if !s.trim().is_empty() {
                chunks.push(s);
            }
            cur.clear();
        }
    };

    for sentence in sentences {
        let pieces = if sentence.len() > max {
            split_long_sentence(&sentence, max)
        } else {
            vec![sentence]
        };
        for piece in pieces {
            if cur.len() + piece.len() > max && !cur.is_empty() {
                flush(&mut cur, &mut chunks);
            }
            cur.extend(piece);
        }
    }
    flush(&mut cur, &mut chunks);
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(text: &str, max: usize) -> Vec<String> {
        split_reading_chunks_with_max(text, max)
    }

    // === split_reading_chunks ===

    #[test]
    fn short_text_is_single_chunk() {
        assert_eq!(chunks("こんにちは。", 120), vec!["こんにちは。"]);
    }

    #[test]
    fn multiple_short_sentences_pack_into_one_chunk() {
        assert_eq!(
            chunks("おはよう。こんにちは。こんばんは。", 120),
            vec!["おはよう。こんにちは。こんばんは。"]
        );
    }

    #[test]
    fn sentence_that_overflows_moves_to_next_chunk() {
        // max=10: 「あああああ。」(6) + 「いいいいい。」(6) は 12 > 10 なので分かれる
        assert_eq!(
            chunks("あああああ。いいいいい。", 10),
            vec!["あああああ。", "いいいいい。"]
        );
    }

    #[test]
    fn long_sentence_splits_at_comma() {
        // max=8: 1 文 12 トークンだが読点で 6+6 に割れる
        assert_eq!(
            chunks("ああああ、いいいいうう。", 8),
            vec!["ああああ、", "いいいいうう。"]
        );
    }

    #[test]
    fn comma_less_long_sentence_is_force_split() {
        let text = "あいうえおかきくけこさし"; // 12 トークン、句読点なし
        assert_eq!(chunks(text, 5), vec!["あいうえお", "かきくけこ", "さし"]);
    }

    #[test]
    fn force_split_never_breaks_zwj_emoji() {
        // 😮‍💨 (3 コードポイント) を跨ぐ位置で強制分割しても 1 トークンとして保持される
        let text = "ああああ😮\u{200D}💨いいいい"; // トークン: 4 + 1 + 4 = 9
        let got = chunks(text, 5);
        assert_eq!(got, vec!["ああああ😮\u{200D}💨", "いいいい"]);
        // どのチャンクにも壊れた ZWJ 断片が現れない
        for c in &got {
            assert!(!c.contains('\u{200D}') || c.contains("😮\u{200D}💨"));
        }
    }

    #[test]
    fn emoji_inside_sentence_is_preserved() {
        assert_eq!(
            chunks("😊今日はいい天気だね。", 120),
            vec!["😊今日はいい天気だね。"]
        );
    }

    #[test]
    fn trailing_emoji_stays_with_its_sentence() {
        // 文末記号の直後の絵文字は直前の文に付随する (次チャンクへ漏れない)
        assert_eq!(
            chunks("ひどいよ…😭ほんとにひどい。", 10),
            vec!["ひどいよ…😭", "ほんとにひどい。"]
        );
    }

    #[test]
    fn blank_lines_are_dropped() {
        assert_eq!(
            chunks("一行目。\n\n   \n二行目。", 120),
            vec!["一行目。二行目。"]
        );
    }

    #[test]
    fn newline_is_sentence_boundary() {
        // 改行だけで終わる行も文として扱われる (句点なし)
        assert_eq!(chunks("タイトル\n本文です。", 6), vec!["タイトル", "本文です。"]);
    }

    #[test]
    fn consecutive_exclamations_stay_together() {
        assert_eq!(
            chunks("えええ！！！！すごい。", 120),
            vec!["えええ！！！！すごい。"]
        );
    }

    #[test]
    fn all_sentence_end_marks_split() {
        let got = chunks("あ。い！う？え…お!か?", 2);
        assert_eq!(got, vec!["あ。", "い！", "う？", "え…", "お!", "か?"]);
    }

    // === decode_bytes ===

    #[test]
    fn decode_utf8_plain_and_bom() {
        assert_eq!(decode_bytes("こんにちは".as_bytes()).unwrap(), "こんにちは");
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice("こんにちは".as_bytes());
        assert_eq!(decode_bytes(&bom).unwrap(), "こんにちは");
    }

    #[test]
    fn decode_shift_jis() {
        // "こんにちは" の Shift_JIS バイト列
        let sjis: &[u8] = &[0x82, 0xB1, 0x82, 0xF1, 0x82, 0xC9, 0x82, 0xBF, 0x82, 0xCD];
        assert_eq!(decode_bytes(sjis).unwrap(), "こんにちは");
    }

    #[test]
    fn decode_text_file_rejects_non_txt_extension() {
        let dir = std::env::temp_dir();
        let p = dir.join("ugg-reader-test.pdf");
        std::fs::write(&p, b"dummy").unwrap();
        let err = decode_text_file(&p).unwrap_err().to_string();
        assert!(err.contains(".txt"), "unexpected: {err}");
        let _ = std::fs::remove_file(&p);
    }

    // === plain_text_chunks / split_long_chunks (docs/script-reader-spec.md §5.1 test20-22) ===

    #[test]
    fn test20_plain_text_chunks_have_default_meta() {
        let got = plain_text_chunks("おはよう。こんにちは。\n次の行。");
        assert!(!got.is_empty());
        for chunk in &got {
            assert_eq!(chunk.slot, VoiceSlot::Main);
            assert_eq!(chunk.speed_offset, 0.0);
            assert_eq!(chunk.caption, None);
            assert_eq!(chunk.pause_after_ms, DEFAULT_PAUSE_MS);
        }
    }

    #[test]
    fn test21_split_long_chunks_duplicates_meta_and_zeroes_middle_pause() {
        // 120 字超 (句点区切りの文を連ねて長行を作る)。
        let long_text = "あ。".repeat(70); // 140 トークン
        let original = ReadingChunk {
            text: long_text.clone(),
            slot: VoiceSlot::Sub,
            speed_offset: 0.2,
            caption: Some("驚いて大声で".to_string()),
            pause_after_ms: 600,
        };
        let got = split_long_chunks(vec![original]);
        assert!(got.len() > 1, "long chunk should be split into fragments");
        let last = got.len() - 1;
        for (i, frag) in got.iter().enumerate() {
            assert_eq!(frag.slot, VoiceSlot::Sub);
            assert_eq!(frag.speed_offset, 0.2);
            assert_eq!(frag.caption.as_deref(), Some("驚いて大声で"));
            if i == last {
                assert_eq!(frag.pause_after_ms, 600, "final fragment keeps original pause");
            } else {
                assert_eq!(frag.pause_after_ms, 0, "middle fragment pause must be 0");
            }
        }
        // 再結合すれば元のテキストに戻る (欠落・重複が無いこと)
        let rejoined: String = got.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(rejoined, long_text);
    }

    #[test]
    fn test21b_short_chunk_passes_through_unchanged() {
        let original = ReadingChunk {
            text: "短い行。".to_string(),
            slot: VoiceSlot::Sub,
            speed_offset: 0.2,
            caption: Some("caption".to_string()),
            pause_after_ms: 600,
        };
        let got = split_long_chunks(vec![original.clone()]);
        assert_eq!(got, vec![original]);
    }

    #[test]
    fn test22_split_long_chunks_never_breaks_emoji_cluster() {
        // 絵文字クラスタ (ZWJ) を含む長行が分断されないこと。
        let mut long_text = "あ".repeat(130);
        long_text.push_str("😮\u{200D}💨");
        long_text.push_str(&"い".repeat(10));
        let original = ReadingChunk {
            text: long_text,
            slot: VoiceSlot::Main,
            speed_offset: 0.0,
            caption: None,
            pause_after_ms: 500,
        };
        let got = split_long_chunks(vec![original]);
        assert!(got.len() > 1);
        let joined: Vec<&str> = got.iter().map(|c| c.text.as_str()).collect();
        // ZWJ 絵文字はどこかのチャンクに丸ごと含まれ、分断された断片は現れない
        let holder_count = joined.iter().filter(|c| c.contains("😮\u{200D}💨")).count();
        assert_eq!(holder_count, 1, "emoji cluster must stay intact in exactly one fragment");
        for c in &joined {
            assert!(
                !c.contains('\u{200D}') || c.contains("😮\u{200D}💨"),
                "broken ZWJ fragment found: {c:?}"
            );
        }
    }
}

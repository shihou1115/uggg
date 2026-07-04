//! テキスト読み上げツールのコマンド (docs/text-reader-spec.md §2.4)。
//!
//! 合成は既存 `synthesize_voice` をフロントがチャンクごとに呼ぶため、本モジュールは
//! 「ファイル読込 + チャンク分割」と「読み上げ中フラグ」だけを提供する。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::State;

use crate::state::AppState;
use crate::tts::reader::{self, ReadingChunk};
use crate::tts::script;

/// .txt / .md (台本) を読み込み、読み上げチャンク列にして返す (docs/script-reader-spec.md §2.9)。
/// - `.txt`: 従来どおりプレーン読み (拡張子 / サイズ 1MB / エンコーディング検証は `decode_text_file`)
/// - `.md`: 台本形式としてパース (`script::parse_script`) し、120 字超の行は追加分割する
/// - その他拡張子: エラー
#[tauri::command]
pub fn reader_load_text(path: String) -> Result<Vec<ReadingChunk>, String> {
    let p = std::path::PathBuf::from(&path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let chunks = match ext.as_deref() {
        Some("txt") => {
            let text = reader::decode_text_file(&p).map_err(|e| format!("{e:#}"))?;
            reader::plain_text_chunks(&text)
        }
        Some("md") => {
            let text = reader::decode_script_file(&p).map_err(|e| format!("{e:#}"))?;
            let parsed = script::parse_script(&text).map_err(|e| e.to_string())?;
            reader::split_long_chunks(parsed)
        }
        _ => return Err("対応していないファイル形式です (.txt / .md のみ読み上げできます)".to_string()),
    };
    if chunks.is_empty() {
        return Err("読み上げるテキストがありません".to_string());
    }
    Ok(chunks)
}

/// 読み上げ中フラグの設定。true の間は自発発話 (独り言・放置反応) が抑制される。
/// 読み上げ開始で true、停止/完走/パネルクローズで false をフロントから呼ぶ。
#[tauri::command]
pub fn set_reading_active(active: bool, state: State<'_, Arc<AppState>>) {
    state.presence.reading.store(active, Ordering::SeqCst);
}

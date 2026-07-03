//! テキスト読み上げツールのコマンド (docs/text-reader-spec.md §2.4)。
//!
//! 合成は既存 `synthesize_voice` をフロントがチャンクごとに呼ぶため、本モジュールは
//! 「ファイル読込 + チャンク分割」と「読み上げ中フラグ」だけを提供する。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::State;

use crate::state::AppState;
use crate::tts::reader;

/// .txt を読み込み、読み上げチャンク列にして返す。
/// 拡張子 (.txt) / サイズ (1MB) / エンコーディング (UTF-8 / Shift_JIS) を検証する。
#[tauri::command]
pub fn reader_load_text(path: String) -> Result<Vec<String>, String> {
    let p = std::path::PathBuf::from(&path);
    let text = reader::decode_text_file(&p).map_err(|e| format!("{e:#}"))?;
    let chunks = reader::split_reading_chunks(&text);
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

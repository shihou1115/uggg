//! ツール系コマンド (M5-B): クリップボード読取。
//!
//! リマインダー系コマンドは M7 で `commands::daily` へ移設した
//! (tools_enabled から独立した常時ローカル機能へ移行、spec §4.2.1 / daily-support-design §5)。

use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::state::AppState;
use crate::tools;

/// クリップボードのテキストを取得する。空 or 非テキストなら空文字。
/// `tools_enabled = false` のときは明示エラーで拒否 (UI 側でも disable しているが二重防御)。
#[tauri::command]
pub fn read_clipboard_text(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let enabled = state.settings.lock().expect("settings poisoned").tools_enabled;
    if !enabled {
        return Err("ツール機能が無効です (設定で有効化してください)".to_string());
    }
    Ok(tools::clipboard::read_text(&app).unwrap_or_default())
}

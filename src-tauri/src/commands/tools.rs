//! ツール系コマンド (M5-B): リマインダー CRUD + クリップボード読取。

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::db::ReminderRow;
use crate::state::AppState;
use crate::tools;

#[derive(Debug, Clone, Serialize)]
pub struct ReminderEntry {
    pub id: i64,
    pub due_ts: i64,
    pub text: String,
    pub created_ts: i64,
}

impl From<ReminderRow> for ReminderEntry {
    fn from(r: ReminderRow) -> Self {
        Self {
            id: r.id,
            due_ts: r.due_ts,
            text: r.text,
            created_ts: r.created_ts,
        }
    }
}

#[tauri::command]
pub fn list_reminders(state: State<'_, Arc<AppState>>) -> Result<Vec<ReminderEntry>, String> {
    let rows = tools::reminder::list(state.inner()).map_err(|e| format!("{e:#}"))?;
    Ok(rows.into_iter().map(ReminderEntry::from).collect())
}

/// 手動でリマインダーを追加する (offset_secs は現在時刻からの相対秒)。
#[tauri::command]
pub fn add_reminder(
    text: String,
    offset_secs: i64,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ReminderEntry>, String> {
    let body = text.trim();
    if body.is_empty() {
        return Err("リマインダー本文が空です".to_string());
    }
    if offset_secs <= 0 {
        return Err("offset_secs は正の値を指定してください".to_string());
    }
    tools::reminder::add(state.inner(), body, offset_secs).map_err(|e| format!("{e:#}"))?;
    list_reminders(state)
}

#[tauri::command]
pub fn delete_reminder(
    id: i64,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ReminderEntry>, String> {
    tools::reminder::delete(state.inner(), id).map_err(|e| format!("{e:#}"))?;
    list_reminders(state)
}

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

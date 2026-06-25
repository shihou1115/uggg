//! クリップボード補助 (M5-B, spec §4.5.3)。
//!
//! 入力欄の 📋 ボタンが押されたときだけ呼ばれる想定。常時監視 / 自動読み取りはしない
//! (プライバシーへの配慮)。`tauri-plugin-clipboard-manager` のラッパ。

use tauri::AppHandle;
use tauri_plugin_clipboard_manager::ClipboardExt;

/// クリップボードのテキスト内容を取得する。空 or 非テキストの場合は `None`。
pub fn read_text(app: &AppHandle) -> Option<String> {
    match app.clipboard().read_text() {
        Ok(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

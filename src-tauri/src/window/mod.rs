use std::sync::Arc;

use anyhow::{Context, Result};
use tauri::{AppHandle, Manager};

use crate::state::AppState;

pub mod mask;
pub mod tray;

pub fn configure_main_window(app: &AppHandle) -> Result<()> {
    let window = app
        .get_webview_window("main")
        .context("main ウインドウが見つかりません")?;
    // 透過/装飾無/常時最前面/非リサイズは tauri.conf.json で設定済み。
    let _ = window.set_focus();
    Ok(())
}

pub fn start_cursor_watcher(app: AppHandle, state: Arc<AppState>) {
    mask::spawn_cursor_watcher(app, state);
}

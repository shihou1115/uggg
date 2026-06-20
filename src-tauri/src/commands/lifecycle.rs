use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};

use crate::dialogue::low;
use crate::ghost::dict::WhenContext;
use crate::state::AppState;

const FIRST_BOOT_KEY: &str = "first_boot_done";

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    // トレイ経由の終了挨拶を再利用するよう、tray::quit_with_farewell と同じ流れに揃えたいが
    // public 化していないので暫定でこちらは即 exit。コンテキストメニュー「終了」は
    // トレイの quit を呼ばないので、ここで挨拶を出さないと UX に齟齬が出る。M3 では
    // tray と挙動を合わせるため、シンプルに即 exit に統一する判断。
    app.exit(0);
}

#[tauri::command]
pub fn hide_window(app: AppHandle) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

/// フロントの初期化完了通知。
/// 初回起動なら events.first_boot、それ以外は events.boot を時間帯別に発火させる。
/// 二重呼び出しは greeted ガードで no-op。
#[tauri::command]
pub fn frontend_ready(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    if state.dialogue.greeted.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    let first_boot = match state.db.get_setting(FIRST_BOOT_KEY) {
        Ok(v) => v.is_none(),
        Err(err) => return Err(format!("{err:#}")),
    };

    let bundle_guard = state.ghost.lock().expect("ghost poisoned");
    let bundle = bundle_guard.as_ref().map_err(|s| s.clone())?;
    let ctx = WhenContext::now();
    let response = low::boot_greeting(
        &bundle.dictionary,
        &ctx,
        first_boot,
        bundle.sub_available(),
    );
    drop(bundle_guard);

    if let Some(resp) = response {
        app.emit("dialogue", &resp)
            .map_err(|e| format!("dialogue イベント送信に失敗しました: {e}"))?;
    }

    if first_boot {
        state
            .db
            .set_setting(FIRST_BOOT_KEY, "1")
            .map_err(|e| format!("{e:#}"))?;
    }

    Ok(())
}

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::dialogue::low;
use crate::ghost::dict::WhenContext;
use crate::state::AppState;

const FIRST_BOOT_KEY: &str = "first_boot_done";

#[tauri::command]
pub async fn quit_app(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    // M4c Phase E: Irodori サイドカーを best-effort で shutdown してからアプリを終了する。
    // shutdown は最大 1〜2 秒待つが、サイドカーが起動していなければ即 return。
    let _ = state.tts.irodori.shutdown().await;
    // トレイ経由の終了挨拶を再利用するよう、tray::quit_with_farewell と同じ流れに揃えたいが
    // public 化していないので暫定でこちらは即 exit。コンテキストメニュー「終了」は
    // トレイの quit を呼ばないので、ここで挨拶を出さないと UX に齟齬が出る。M3 では
    // tray と挙動を合わせるため、シンプルに即 exit に統一する判断。
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub fn hide_window(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

/// M5-H: 自動起動の有効/無効切替 (`tauri-plugin-autostart` 経由)。
/// Settings.autostart の値変更時にフロントから呼ぶ。OS 状態と Settings を揃える役割。
#[tauri::command]
pub fn set_autostart(enabled: bool, app: AppHandle) -> Result<(), String> {
    let manager = app.autolaunch();
    if enabled {
        manager
            .enable()
            .map_err(|e| format!("自動起動の有効化に失敗: {e}"))?;
    } else {
        manager
            .disable()
            .map_err(|e| format!("自動起動の無効化に失敗: {e}"))?;
    }
    Ok(())
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

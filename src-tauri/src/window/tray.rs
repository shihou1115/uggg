//! タスクトレイ (spec §4.1.7)。
//! 左クリック → ウインドウ表示/非表示トグル。右クリックメニュー: 表示/モード/静音/設定/終了。
//! 終了時は events.quit を再生してから exit。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::{Context, Result};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use crate::dialogue::{self, low};
use crate::ghost::dict::WhenContext;
use crate::state::{AppState, DialogueMode};

const ID_TOGGLE_WINDOW: &str = "toggle_window";
const ID_MODE_LOW: &str = "mode_low";
const ID_MODE_ADVANCED: &str = "mode_advanced";
const ID_QUIET_TOGGLE: &str = "quiet_toggle";
const ID_OPEN_SETTINGS: &str = "open_settings";
const ID_QUIT: &str = "quit";

pub fn install(app: &AppHandle, state: Arc<AppState>) -> Result<()> {
    let menu = build_menu(app, &state)?;
    let _tray = TrayIconBuilder::with_id("ugg-tray")
        .tooltip("ugg")
        .icon(app.default_window_icon().cloned().context("default icon missing")?)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event({
            let app = app.clone();
            let state = state.clone();
            move |_app, event| handle_menu(&app, &state, event.id.as_ref())
        })
        .on_tray_icon_event({
            let app = app.clone();
            move |_tray, event| match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } => toggle_window(&app),
                _ => {}
            }
        })
        .build(app)
        .context("install tray icon failed")?;
    Ok(())
}

fn build_menu(app: &AppHandle, state: &Arc<AppState>) -> Result<Menu<tauri::Wry>> {
    let cur = state.settings.lock().expect("settings poisoned").clone();
    let is_advanced = matches!(cur.mode, DialogueMode::Advanced);

    let toggle = MenuItem::with_id(app, ID_TOGGLE_WINDOW, "表示 / 非表示", true, None::<&str>)?;
    let mode_low = CheckMenuItem::with_id(
        app,
        ID_MODE_LOW,
        "low モード (辞書のみ)",
        true,
        !is_advanced,
        None::<&str>,
    )?;
    let mode_adv = CheckMenuItem::with_id(
        app,
        ID_MODE_ADVANCED,
        "advanced モード (LLM)",
        true,
        is_advanced,
        None::<&str>,
    )?;
    let quiet = CheckMenuItem::with_id(
        app,
        ID_QUIET_TOGGLE,
        "静音モード",
        true,
        cur.quiet_mode,
        None::<&str>,
    )?;
    let settings = MenuItem::with_id(app, ID_OPEN_SETTINGS, "設定を開く", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, ID_QUIT, "終了", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;

    Menu::with_items(
        app,
        &[
            &toggle, &sep, &mode_low, &mode_adv, &quiet, &sep, &settings, &sep, &quit,
        ],
    )
    .context("build tray menu")
}

fn handle_menu(app: &AppHandle, state: &Arc<AppState>, id: &str) {
    match id {
        ID_TOGGLE_WINDOW => toggle_window(app),
        ID_MODE_LOW => set_mode(app, state, DialogueMode::Low),
        ID_MODE_ADVANCED => set_mode(app, state, DialogueMode::Advanced),
        ID_QUIET_TOGGLE => toggle_quiet(app, state),
        ID_OPEN_SETTINGS => {
            let _ = app.emit("open-settings", ());
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }
        ID_QUIT => quit_with_farewell(app.clone(), state.clone()),
        _ => {}
    }
}

fn toggle_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    match window.is_visible() {
        Ok(true) => {
            let _ = window.hide();
        }
        _ => {
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
}

fn set_mode(app: &AppHandle, state: &Arc<AppState>, mode: DialogueMode) {
    {
        let mut s = state.settings.lock().expect("settings poisoned");
        s.mode = mode;
    }
    // 永続化 + フロント通知
    let snapshot = state.settings.lock().expect("settings poisoned").clone();
    persist_and_broadcast(app, state, &snapshot);
}

fn toggle_quiet(app: &AppHandle, state: &Arc<AppState>) {
    {
        let mut s = state.settings.lock().expect("settings poisoned");
        s.quiet_mode = !s.quiet_mode;
    }
    let snapshot = state.settings.lock().expect("settings poisoned").clone();
    persist_and_broadcast(app, state, &snapshot);
}

fn persist_and_broadcast(app: &AppHandle, state: &Arc<AppState>, settings: &crate::state::Settings) {
    if let Ok(json) = serde_json::to_string(settings) {
        let _ = state.db.set_setting("settings", &json);
    }
    let _ = app.emit("settings-changed", settings);
}

/// 終了挨拶 (events.quit) を再生してから exit。
/// quit が辞書に無い場合や ghost ロード失敗時は即 exit。
fn quit_with_farewell(app: AppHandle, state: Arc<AppState>) {
    // 同時起動防止: greeted のように単発フラグは置かないが、
    // タイマー競合は無視できる短さなので素直に動かす。
    tauri::async_runtime::spawn(async move {
        let ctx = WhenContext::now();
        let resp = {
            let guard = state.ghost.lock().expect("ghost poisoned");
            match guard.as_ref() {
                Ok(b) => low::event(&b.dictionary, "quit", &ctx, b.sub_available()),
                Err(_) => None,
            }
        };
        let hold_ms = match &resp {
            Some(r) => {
                dialogue::persist_and_speak(&app, &state, r);
                let total = r.main.text.chars().count()
                    + r.sub.as_ref().map(|s| s.text.chars().count()).unwrap_or(0);
                // フロントの hold (~1.6s + 60ms/char) + 余裕を持って exit
                (1600 + total as u64 * 60).min(8000) + 500
            }
            None => 0,
        };
        if hold_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(hold_ms)).await;
        }
        // ウインドウ位置を即時保存してから終了
        crate::presence::window_pos::persist_now(&app, &state);
        // greeted は ATOMIC 操作で先に下ろしておく (短期間で再起動した場合の重複挨拶を防ぐ ―
        // ただし frontend_ready が greeted を見るのは初回のみなので影響軽微)
        state.dialogue.greeted.store(false, Ordering::SeqCst);
        app.exit(0);
    });
}

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod db;
mod dialogue;
mod ghost;
mod presence;
mod state;
mod system;
mod tasks;
mod tts;
mod window;

use std::sync::Arc;

use tauri::Manager;

use crate::state::AppState;

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let state = Arc::new(AppState::initialize(app.handle())?);
            app.manage(state.clone());
            window::configure_main_window(app.handle())?;
            window::start_cursor_watcher(app.handle().clone(), state.clone());
            // ウインドウ位置の復元 + 監視保存
            presence::window_pos::restore(app.handle(), &state);
            presence::window_pos::spawn_pos_saver(app.handle().clone(), state.clone());
            // 自発挙動: ランダムトーク + 放置監視
            tasks::spawn_random_talk(app.handle().clone(), state.clone());
            tasks::spawn_idle_watcher(app.handle().clone(), state.clone());
            // タスクトレイ
            if let Err(err) = window::tray::install(app.handle(), state.clone()) {
                eprintln!("[tray] install failed: {err:#}");
            }
            // TTS が有効 & 資産あり なら背景で voicevox engine を事前 init (初発話のラグ解消)
            {
                let s = state.settings.lock().expect("settings poisoned").clone();
                if s.tts_enabled {
                    commands::tts::spawn_preinit(state.clone());
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::boot::get_boot_payload,
            commands::dialogue::send_user_message,
            commands::interaction::poke,
            commands::interaction::nade,
            commands::lifecycle::frontend_ready,
            commands::lifecycle::quit_app,
            commands::lifecycle::hide_window,
            commands::onboarding::complete_onboarding,
            commands::onboarding::skip_onboarding,
            commands::pomodoro::start_pomodoro,
            commands::pomodoro::stop_pomodoro,
            commands::pomodoro::get_pomodoro_status,
            commands::profile::get_profile,
            commands::profile::add_profile,
            commands::profile::delete_profile,
            commands::secrets::set_api_key,
            commands::secrets::has_api_key,
            commands::secrets::delete_api_key,
            commands::settings::get_settings,
            commands::settings::set_settings,
            commands::tts::voicevox_assets_ready,
            commands::tts::download_voicevox_assets,
            commands::tts::list_voices,
            commands::tts::synthesize_voice,
            commands::tts::set_github_token,
            commands::tts::has_github_token,
            commands::tts::delete_github_token,
            commands::window::update_alpha_mask,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start ugg");
}

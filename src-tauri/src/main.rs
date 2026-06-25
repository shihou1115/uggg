#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod db;
mod dialogue;
mod ghost;
mod presence;
mod state;
mod system;
mod tasks;
mod tools;
mod tts;
mod window;

use std::sync::Arc;

use tauri::Manager;

use crate::state::AppState;

fn main() {
    tauri::Builder::default()
        // M5-H: 自動起動 (Windows ではレジストリ HKCU\...\Run に登録される)
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        // M5-B クリップボード補助は `tauri-plugin-clipboard-manager` が Windows で
        // 起動時 hang する問題があったため、`arboard` crate を tools/clipboard.rs から
        // 直接呼ぶ実装に切り替え (plugin は使わない)。
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
            // M4c Phase E: Irodori サイドカーのアイドル監視 (5 分未使用で自動 shutdown)
            tasks::spawn_irodori_idle_watcher(state.clone());
            // M4c Phase G: Irodori サイドカーのヘルスチェック (30 秒間隔、3 連続失敗で再起動)
            tasks::spawn_irodori_health_watcher(app.handle().clone(), state.clone());
            // M5-D: 起動 30 秒後 + 24 時間毎に update フィードをチェック
            tasks::spawn_update_watcher(app.handle().clone(), state.clone());
            // M5-C: topics_enabled が ON の間、1 時間おきに RSS を取得して topics_cache に蓄積
            tasks::spawn_topics_watcher(state.clone());
            // M5-B: リマインダー watcher (10 秒間隔、due_ts 到達で persist_and_speak)
            tasks::spawn_reminder_watcher(app.handle().clone(), state.clone());
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
            // Irodori sidecar.py をリソースから %APPDATA%\ugg\irodori\ にコピー (best-effort)。
            // dev 起動でリソース未配置の場合は黙って skip (実害なし)。
            if let Ok(asset_root) = crate::tts::voice_ref::irodori_root() {
                if let Ok(resource_dir) = app.path().resource_dir() {
                    let _ = crate::tts::sidecar::install_sidecar_script(&resource_dir, &asset_root);
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::assets::list_ghosts,
            commands::assets::list_shells,
            commands::assets::dnd_install,
            commands::boot::get_boot_payload,
            commands::data::get_chat_log,
            commands::data::export_data,
            commands::data::clear_history,
            commands::data::check_update_now,
            commands::dialogue::send_user_message,
            commands::interaction::poke,
            commands::interaction::nade,
            commands::lifecycle::frontend_ready,
            commands::lifecycle::quit_app,
            commands::lifecycle::hide_window,
            commands::lifecycle::set_autostart,
            commands::onboarding::complete_onboarding,
            commands::onboarding::skip_onboarding,
            commands::pomodoro::start_pomodoro,
            commands::pomodoro::stop_pomodoro,
            commands::pomodoro::get_pomodoro_status,
            commands::topics::get_interests,
            commands::topics::set_interests,
            commands::topics::fetch_topics_now,
            commands::tools::list_reminders,
            commands::tools::add_reminder,
            commands::tools::delete_reminder,
            commands::tools::read_clipboard_text,
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
            commands::tts::irodori_check_gpu,
            commands::tts::irodori_assets_ready,
            commands::tts::download_irodori_assets,
            commands::tts::voice_ref_list,
            commands::tts::voice_ref_delete,
            commands::tts::voice_ref_generate,
            commands::tts::voice_ref_preview,
            commands::window::update_alpha_mask,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start ugg");
}

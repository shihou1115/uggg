use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};

use crate::state::{AppState, Settings};

pub(crate) const SETTINGS_KEY: &str = "settings";

#[tauri::command]
pub fn get_settings(state: State<'_, Arc<AppState>>) -> Settings {
    state.settings.lock().expect("settings poisoned").clone()
}

#[tauri::command]
pub fn set_settings(
    settings: Settings,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Settings, String> {
    let mut next = settings;
    next.clamp();

    // メモリ反映前の値と比較する (🔕 backoff リセット・天気キャッシュ消去の判定に使う)。
    let prev = state.settings.lock().expect("settings poisoned").clone();

    // M9/M11: Situation*/Regular* を OFF→ON に戻したら 🔕 backoff をリセットする
    // (reviewer 指摘。恒久 throttle が再有効化後も残り、理由の見えないまま間隔が
    // 絞られるのを防ぐ)。
    for cat in crate::system::governance::feedback_reenabled(&prev, &next) {
        crate::system::governance::reset_backoff(state.inner(), cat);
    }

    // M11: 天気の「解除」(weather_ready: true→false、同意撤回) では
    // app_settings の weather_cache も消す (regular-talk-design §9.2、設定行為 = 同意
    // の対称。地名・キャッシュを残さない)。
    if prev.weather_ready() && !next.weather_ready() {
        crate::system::weather::clear_cache(state.inner());
    }

    // 永続化 (app_settings."settings" に JSON で保存)
    let json = serde_json::to_string(&next)
        .map_err(|e| format!("Settings の JSON シリアライズ失敗: {e}"))?;
    state
        .db
        .set_setting(SETTINGS_KEY, &json)
        .map_err(|err| format!("{err:#}"))?;

    // メモリ反映
    {
        let mut guard = state.settings.lock().expect("settings poisoned");
        *guard = next.clone();
    }

    // フロントへ変更通知 (settings-changed)
    let _ = app.emit("settings-changed", &next);
    Ok(next)
}

/// AppState::initialize で呼び出す: 起動時に DB から Settings を復元する。
/// レコードが無い / パース失敗時は引数の `current` をそのまま返す (デフォルト値が温存される)。
pub fn load_persisted_settings(db: &crate::db::Db, current: Settings) -> Settings {
    let stored = match db.get_setting(SETTINGS_KEY) {
        Ok(Some(v)) => v,
        _ => return current,
    };
    match serde_json::from_str::<Settings>(&stored) {
        Ok(mut s) => {
            s.clamp();
            s
        }
        Err(_) => current,
    }
}

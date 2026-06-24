//! データ系コマンド (M5-G/E): チャットログ取得 / エクスポート / 履歴クリア。

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::json;
use tauri::State;

use crate::db::ChatLogRow;
use crate::state::AppState;

/// M5-G: 新しい順に N 件の chat_log を返す。
#[tauri::command]
pub fn get_chat_log(
    limit: u32,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ChatLogRow>, String> {
    let limit = limit.clamp(1, 1000);
    state
        .db
        .list_recent_chat_log(limit)
        .map_err(|e| format!("{e:#}"))
}

/// M5-E: 会話ログ・API 使用履歴・(option で) 記憶 を JSON でダウンロードフォルダに書き出す。
/// 戻り値: 書き出した絶対パス。
#[tauri::command]
pub fn export_data(
    include_profile: bool,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    // chat_log は最大 10000 件まで (DB が肥大化しても export を破綻させない上限)
    let chat = state
        .db
        .list_recent_chat_log(10000)
        .map_err(|e| format!("{e:#}"))?;
    let usage = state.db.list_api_usage().map_err(|e| format!("{e:#}"))?;
    let profile = if include_profile {
        Some(state.db.list_profile().map_err(|e| format!("{e:#}"))?)
    } else {
        None
    };

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let payload = json!({
        "schema": "ugg-export-v1",
        "exported_at": ts,
        "include_profile": include_profile,
        "chat_log": chat,
        "api_usage": usage,
        "user_profile": profile,
    });

    let dir = dirs::download_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| "ダウンロードフォルダが解決できませんでした".to_string())?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("保存先の作成に失敗: {e}"))?;
    let path = dir.join(format!("ugg-export-{ts}.json"));
    let body = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("JSON 整形に失敗: {e}"))?;
    std::fs::write(&path, body).map_err(|e| format!("書き出しに失敗: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

/// M5-E: 履歴クリア。常に chat_log を全件削除、`include_profile=true` で user_profile も全削除。
/// (origin に関係なく削除する仕様。記憶を残したいなら include_profile=false。)
#[tauri::command]
pub fn clear_history(
    include_profile: bool,
    state: State<'_, Arc<AppState>>,
) -> Result<ClearResult, String> {
    state.db.clear_chat_log().map_err(|e| format!("{e:#}"))?;
    let mut cleared_profiles: u64 = 0;
    if include_profile {
        cleared_profiles = state.db.clear_user_profile().map_err(|e| format!("{e:#}"))?;
    }
    Ok(ClearResult {
        chat_cleared: true,
        profile_cleared_count: cleared_profiles,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ClearResult {
    pub chat_cleared: bool,
    pub profile_cleared_count: u64,
}

/// M5-D: 設定パネルの「いますぐチェック」ボタンから呼ぶ。
/// `update_feed_url` が未設定なら明示エラー、更新なしなら Ok でメッセージなし。
#[tauri::command]
pub async fn check_update_now(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let url = state
        .settings
        .lock()
        .expect("settings poisoned")
        .update_feed_url
        .clone();
    if url.is_none() {
        return Err("更新フィードの URL が設定されていません".to_string());
    }
    let state_arc = state.inner().clone();
    crate::system::update::check_update_once(&app, &state_arc)
        .await
        .map_err(|e| format!("{e:#}"))
}

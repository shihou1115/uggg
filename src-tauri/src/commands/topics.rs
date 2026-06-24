//! 時事ネタコマンド (M5-C): 興味分野 CRUD + 即時取得。

use std::sync::Arc;

use tauri::State;

use crate::db::InterestTopic;
use crate::state::AppState;

#[tauri::command]
pub fn get_interests(state: State<'_, Arc<AppState>>) -> Result<Vec<InterestTopic>, String> {
    state.db.list_interests().map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn set_interests(
    topics: Vec<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<InterestTopic>, String> {
    if topics.len() > 20 {
        return Err("興味分野は 20 件まで設定できます".to_string());
    }
    state
        .db
        .replace_interests(&topics)
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn fetch_topics_now(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let state_arc = state.inner().clone();
    crate::system::topics::fetch_all_into_cache(&state_arc)
        .await
        .map_err(|e| format!("{e:#}"))
}

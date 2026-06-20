use std::sync::Arc;

use chrono::Utc;
use tauri::State;

use crate::db::{ProfileEntry, ProfileOrigin};
use crate::state::AppState;

#[tauri::command]
pub fn get_profile(state: State<'_, Arc<AppState>>) -> Result<Vec<ProfileEntry>, String> {
    state.db.list_profile().map_err(|err| format!("{err:#}"))
}

#[tauri::command]
pub fn add_profile(
    content: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ProfileEntry>, String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err("記憶内容が空です".to_string());
    }
    state
        .db
        .insert_profile(trimmed, ProfileOrigin::Manual, None, Utc::now().timestamp())
        .map_err(|err| format!("{err:#}"))?;
    state.db.list_profile().map_err(|err| format!("{err:#}"))
}

#[tauri::command]
pub fn delete_profile(
    id: i64,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ProfileEntry>, String> {
    state
        .db
        .delete_profile(id)
        .map_err(|err| format!("{err:#}"))?;
    state.db.list_profile().map_err(|err| format!("{err:#}"))
}

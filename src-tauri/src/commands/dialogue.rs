use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::dialogue::{self, DialogueResponse};
use crate::state::AppState;

#[tauri::command]
pub async fn send_user_message(
    text: String,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<DialogueResponse, String> {
    let state = state.inner().clone();
    dialogue::handle_user_message(app, &state, &text).await
}

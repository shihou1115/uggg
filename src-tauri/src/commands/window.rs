use std::sync::Arc;

use base64::Engine;
use tauri::State;

use crate::state::AppState;

#[tauri::command]
pub fn update_alpha_mask(
    cols: u32,
    rows: u32,
    cell_size_css: u32,
    data: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    if cell_size_css == 0 {
        return Err("alpha mask の cell_size_css は 1 以上にしてください".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| format!("alpha mask の base64 デコードに失敗しました: {e}"))?;
    let expected = (cols as usize).saturating_mul(rows as usize);
    if bytes.len() != expected {
        return Err(format!(
            "alpha mask のサイズ不一致: 期待 {expected} バイト、実 {} バイト",
            bytes.len()
        ));
    }
    let mut mask = state
        .window
        .alpha_mask
        .lock()
        .expect("alpha_mask poisoned");
    mask.cols = cols;
    mask.rows = rows;
    mask.cell_size_css = cell_size_css;
    mask.data = bytes;
    Ok(())
}

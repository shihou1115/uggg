use std::sync::Arc;

use base64::Engine;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::state::AppState;

/// キャラごとの X 位置 (ステージ内 CSS px、視覚ボックス左端)。spec §4.1.6。
/// 未保存・sub 無しゴーストは None。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct CharPositions {
    pub main: Option<f64>,
    pub sub: Option<f64>,
}

const CHAR_POS_KEY: &str = "char_pos";

/// ドラッグ終了時にフロントから呼ばれる (spec §4.3.4)。全置換で保存する。
#[tauri::command]
pub fn set_char_positions(
    main: Option<f64>,
    sub: Option<f64>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    for v in [main, sub].into_iter().flatten() {
        if !v.is_finite() {
            return Err("キャラ位置に不正な値が指定されました".into());
        }
    }
    let json = serde_json::to_string(&CharPositions { main, sub })
        .map_err(|e| format!("キャラ位置のシリアライズに失敗しました: {e}"))?;
    state
        .db
        .set_setting(CHAR_POS_KEY, &json)
        .map_err(|e| format!("キャラ位置の保存に失敗しました: {e}"))
}

/// boot payload 用: 保存済みキャラ位置を読む。無ければ・壊れていれば既定 (None, None)。
pub fn load_char_positions(db: &crate::db::Db) -> CharPositions {
    match db.get_setting(CHAR_POS_KEY) {
        Ok(Some(v)) => serde_json::from_str(&v).unwrap_or_default(),
        _ => CharPositions::default(),
    }
}

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

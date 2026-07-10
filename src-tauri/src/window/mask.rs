use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::state::AppState;

/// 50ms 間隔でカーソル位置とアルファマスクを突き合わせ、
/// 透過セルの上にカーソルが居れば `set_ignore_cursor_events(true)` を呼ぶ。
/// 状態が変わったときだけ呼び出すので IPC は最小限。
pub fn spawn_cursor_watcher(app: AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let mut last_ignore: Option<bool> = None;
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let Some(window) = app.get_webview_window("main") else {
                continue;
            };
            let Ok(cursor_phys) = window.cursor_position() else {
                continue;
            };
            let Ok(win_pos_phys) = window.outer_position() else {
                continue;
            };
            let Ok(win_inner_phys) = window.inner_size() else {
                continue;
            };
            let scale = window.scale_factor().unwrap_or(1.0);

            // 物理 → 論理 (CSS px) 変換
            let rel_x_phys = cursor_phys.x - win_pos_phys.x as f64;
            let rel_y_phys = cursor_phys.y - win_pos_phys.y as f64;
            let rel_x_css = rel_x_phys / scale;
            let rel_y_css = rel_y_phys / scale;
            let inner_w_css = win_inner_phys.width as f64 / scale;
            let inner_h_css = win_inner_phys.height as f64 / scale;

            let opaque = check_opaque_at(&state, rel_x_css, rel_y_css, inner_w_css, inner_h_css);
            let want_ignore = !opaque;
            if last_ignore != Some(want_ignore) {
                // 透過化 (ignore=true) への遷移は左ボタン押下中は保留する。
                // キャラドラッグ (spec §4.3.4) 中はマスク更新 (50ms デバウンス) が
                // カーソルに追いつかず、古いマスクの透明セル上で click-through が
                // 発動して mousemove/mouseup を取りこぼすため。
                // 対話化 (ignore=false) への遷移は常に即時。
                if want_ignore && left_button_held() {
                    continue;
                }
                let _ = window.set_ignore_cursor_events(want_ignore);
                last_ignore = Some(want_ignore);
            }
        }
    });
}

#[cfg(windows)]
fn left_button_held() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    (unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000) != 0
}

#[cfg(not(windows))]
fn left_button_held() -> bool {
    false
}

fn check_opaque_at(state: &AppState, x_css: f64, y_css: f64, w_css: f64, h_css: f64) -> bool {
    let mask = state.window.alpha_mask.lock().expect("alpha_mask poisoned");
    if mask.cols == 0 || mask.rows == 0 || mask.cell_size_css == 0 {
        return false;
    }
    if x_css < 0.0 || y_css < 0.0 || x_css >= w_css || y_css >= h_css {
        return false;
    }
    let cell = mask.cell_size_css as f64;
    let c = (x_css / cell) as u32;
    let r = (y_css / cell) as u32;
    if c >= mask.cols || r >= mask.rows {
        return false;
    }
    let idx = (r * mask.cols + c) as usize;
    mask.data.get(idx).copied().unwrap_or(0) != 0
}

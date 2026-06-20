//! 静音判定 (spec §4.4.8 / §4.4.9)。
//!
//! 自発発話 (ランダムトーク・放置反応) を止めるべきかを返す。
//! 条件は OR:
//!   - quiet_mode 設定 ON
//!   - ポモドーロ集中中
//!   - auto_quiet_fullscreen ON かつ前面ウインドウがモニタ全面
//!
//! ユーザー操作への応答・起動/終了挨拶・リマインダーはこの判定を無視する (呼び出し側の責務)。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::state::AppState;

pub fn should_stay_quiet(state: &Arc<AppState>) -> bool {
    // ポモドーロ集中中
    if state.pomodoro.focus.load(Ordering::SeqCst) {
        return true;
    }
    let settings = state.settings.lock().expect("settings poisoned");
    if settings.quiet_mode {
        return true;
    }
    if settings.auto_quiet_fullscreen && is_foreground_fullscreen() {
        return true;
    }
    false
}

/// 前面ウインドウがモニタ全面を占有しているか (ゲーム・全画面動画等)。
/// デスクトップシェル (Progman/WorkerW) は除外する。
#[cfg(windows)]
fn is_foreground_fullscreen() -> bool {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowRect,
    };

    unsafe {
        let hwnd: HWND = GetForegroundWindow();
        if hwnd.0.is_null() {
            return false;
        }

        // デスクトップシェルを除外
        let mut class_buf = [0u16; 256];
        let len = GetClassNameW(hwnd, &mut class_buf);
        if len > 0 {
            let class = String::from_utf16_lossy(&class_buf[..len as usize]);
            if class == "Progman" || class == "WorkerW" {
                return false;
            }
        }

        let mut win_rect = RECT::default();
        if GetWindowRect(hwnd, &mut win_rect).is_err() {
            return false;
        }

        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut mi).as_bool() {
            return false;
        }

        // ウインドウ矩形がモニタ全体を覆っているか (タスクバー込みの rcMonitor 基準)
        let m = mi.rcMonitor;
        win_rect.left <= m.left
            && win_rect.top <= m.top
            && win_rect.right >= m.right
            && win_rect.bottom >= m.bottom
    }
}

#[cfg(not(windows))]
fn is_foreground_fullscreen() -> bool {
    false
}

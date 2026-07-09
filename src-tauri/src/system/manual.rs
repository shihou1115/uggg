//! 初回起動時に取扱説明書 (manual.md) を一度だけ開く。
//!
//! 配布版は `bundle.resources` で resource_dir に `manual.md` として同梱される。
//! dev はリポジトリの `docs/manual.md` を使う。開いたかどうかは app_settings の
//! `manual_shown` キーで管理し、2 回目以降は開かない (既存ユーザーが本機能を含む版へ
//! 更新した直後は 1 度だけ開く)。

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Manager};

use crate::state::{resolve_assets_dir, AppState};

const MANUAL_SHOWN_KEY: &str = "manual_shown";

/// 初回起動なら manual.md を開き、**開けたときだけ**フラグを立てる。
/// フラグ済み、または manual.md が見つからない場合は何もしない。
/// 開けなかった場合はフラグを立てず、次回起動で再試行する。
pub fn open_on_first_run(app: &AppHandle, state: &Arc<AppState>) {
    if matches!(state.db.get_setting(MANUAL_SHOWN_KEY), Ok(Some(v)) if v == "1") {
        return;
    }
    let Some(path) = resolve_manual_path(app) else {
        // 見つからないときはフラグを立てない (将来のビルドで開けるよう温存)。
        return;
    };
    if open_file(&path) {
        let _ = state.db.set_setting(MANUAL_SHOWN_KEY, "1");
    }
}

/// ファイルを開く。まず ShellExecuteW の既定 verb (関連付けアプリ)。
/// `.md` の関連付けが無いクリーンな Windows では既定 verb が SE_ERR_NOASSOC 等で
/// 失敗するため、必ず存在する notepad.exe にフォールバックする
/// (v0.1.3 の explorer.exe 経由は、環境によって未関連付けファイルを黙って無視する)。
fn open_file(path: &std::path::Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // ShellExecuteW は成功時に 32 より大きい疑似 HINSTANCE を返す (Win32 仕様)。
    let ok = unsafe {
        let h = ShellExecuteW(
            None,
            PCWSTR::null(), // 既定 verb (= ダブルクリック相当)
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        h.0 as usize > 32
    };
    if ok {
        return true;
    }
    // 関連付けが無い環境向けの確実なフォールバック
    std::process::Command::new("notepad.exe")
        .arg(path)
        .spawn()
        .is_ok()
}

/// manual.md の場所を解決する。
/// - 配布版: `resource_dir/manual.md` (bundle.resources で同梱)
/// - dev: `<assets_dir>/docs/manual.md` (assets_dir はリポジトリルート)
fn resolve_manual_path(app: &AppHandle) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(res) = app.path().resource_dir() {
        candidates.push(res.join("manual.md"));
    }
    if let Ok(base) = resolve_assets_dir(app) {
        candidates.push(base.join("manual.md")); // prod: base=$INSTDIR
        candidates.push(base.join("docs").join("manual.md")); // dev: base=repo root
    }
    candidates.into_iter().find(|p| p.is_file())
}

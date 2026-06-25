//! クリップボード補助 (M5-B, spec §4.5.3)。
//!
//! 入力欄の 📋 ボタンが押されたときだけ呼ばれる想定。常時監視 / 自動読み取りはしない
//! (プライバシーへの配慮)。`tauri-plugin-clipboard-manager` のラッパ。

use tauri::AppHandle;

/// クリップボードのテキスト内容を取得する。空 or 非テキストの場合は `None`。
///
/// 注: tauri-plugin-clipboard-manager は Windows でアプリ起動時に hang する症状があったため、
/// `arboard` クレートを直接呼ぶ実装に切り替えた (機能等価)。`AppHandle` は将来の per-window
/// ハンドリング拡張のために引数として残している。
pub fn read_text(_app: &AppHandle) -> Option<String> {
    let mut ctx = arboard::Clipboard::new().ok()?;
    let text = ctx.get_text().ok()?;
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

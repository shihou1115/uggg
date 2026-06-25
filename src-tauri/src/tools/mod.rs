//! advanced モードのツール群 (M5-B, spec §4.5.3)。
//!
//! - `clock`: 現在時刻を LLM system prompt に注入
//! - `reminder`: 「N 分後に教えて」を検出 → DB に登録 → watcher で発火
//! - `clipboard`: 入力欄の 📋 ボタン押下時のみクリップボードを読み取る
//!
//! いずれも `Settings.tools_enabled = true` のときだけ動く前提 (個別切替なし)。

pub mod clipboard;
pub mod clock;
pub mod reminder;

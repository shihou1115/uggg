//! ツール群。
//!
//! - `clock`: 現在時刻を LLM system prompt に注入 (tools_enabled 時のみ、M5-B)
//! - `clipboard`: 入力欄の 📋 ボタン押下時のみクリップボードを読み取る (tools_enabled 時のみ)
//! - `reminder`: 統合リマインダーの自然文パーサ + TZ 変換 (M7)。
//!   `daily_support_enabled` 配下の常時ローカル機能で tools_enabled から独立 (spec §4.2.1)
//! - `todo`: ToDo・日課のドメインロジック (M8)。同じく daily_support 配下

pub mod clipboard;
pub mod clock;
pub mod reminder;
pub mod todo;

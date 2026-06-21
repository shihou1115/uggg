//! TTS パイプライン (spec §4.5.1 / architecture §7)。
//!
//! 現状は voicevox_core 埋め込み (M4a) と漢字→ひらがな前処理 (M4b)。
//! Irodori-TTS サイドカー (M4c) は別セッションで追加予定。
//!
//! 合成は別アプリ/別サーバを起動しない: voicevox_core.dll を libloading で
//! 実行時ロードし、プロセス内で CPU 合成する。

pub mod download;
pub mod preprocess;
pub mod voicevox;

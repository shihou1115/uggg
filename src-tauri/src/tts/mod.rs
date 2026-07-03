//! TTS パイプライン (spec §4.5.1 / architecture §7)。
//!
//! 既定エンジンは voicevox_core (M4a, CPU 合成, 無サーバ)。M4b で漢字→ひらがな前処理を
//! 用意し、M4c の Irodori-TTS (Python サイドカー, GPU 必須) で利用する。
//!
//! 合成は別アプリ/別サーバを起動しない要件 (spec §1.3) を満たす:
//! - voicevox: voicevox_core.dll を libloading で実行時ロード、プロセス内 CPU 合成
//! - irodori: ugg が同梱 Python サイドカーを起動・停止管理 (M4c で実装)

pub mod download;
pub mod gpu;
pub mod irodori;
pub mod irodori_download;
pub mod preprocess;
pub mod reader;
pub mod sidecar;
pub mod voice_ref;
pub mod voicevox;

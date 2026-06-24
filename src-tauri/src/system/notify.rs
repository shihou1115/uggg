//! ゴースト発話による横断通知 (spec §3.1 / architecture §11)。
//!
//! 辞書 `system_messages` にキーがあればそれを発話。無ければトーストフォールバックの
//! 代わりに `system-toast` イベントをフロントへ流す (M2 段階では console.error 代替)。

use std::sync::Arc;

use tauri::{AppHandle, Emitter};

use crate::dialogue::{banter, DialogueResponse};
use crate::ghost::dict::WhenContext;
use crate::state::AppState;

#[derive(Debug, Clone)]
pub enum NoticeKind {
    CostWarning80 {
        provider: String,
    },
    CostLimitExceeded {
        provider: String,
    },
    ModeDegraded {
        reason: DegradeReason,
    },
    ModeRecovered,
    /// voicevox_core 資産 DL 完了。
    VoicevoxDlComplete,
    /// voicevox_core 資産 DL 失敗 (詳細は reason)。
    VoicevoxDlFailed {
        reason: String,
    },
    /// Irodori-TTS が利用できない (GPU 不可 / サイドカー起動失敗 / ヘルスチェック失敗 等)。
    /// architecture §11.2: severity = Important (現状は dialogue 経路のみ、トースト二段は将来)。
    /// M4c Phase G の `tasks::spawn_irodori_health_watcher` から発火する。
    IrodoriUnavailable {
        reason: String,
    },
    /// Irodori Python ランタイム + 共通依存 DL が完了 (M4c Phase C 以降)。
    IrodoriDlComplete,
    /// Irodori 資産 DL が失敗 (Python embeddable / pip / torch / 依存のいずれかで失敗)。
    IrodoriDlFailed {
        reason: String,
    },
    /// M5-D: 新バージョン検出 (`update_feed_url` からの応答に基づく告知)。
    UpdateAvailable {
        version: String,
    },
}

#[derive(Debug, Clone)]
pub enum DegradeReason {
    ApiError,
    CostLimit,
}

impl NoticeKind {
    fn dict_key(&self) -> &'static str {
        match self {
            NoticeKind::CostWarning80 { .. } => "cost_warning_80",
            NoticeKind::CostLimitExceeded { .. } => "cost_limit_exceeded",
            NoticeKind::ModeDegraded { .. } => "mode_degraded",
            NoticeKind::ModeRecovered => "mode_recovered",
            NoticeKind::VoicevoxDlComplete => "voicevox_dl_complete",
            NoticeKind::VoicevoxDlFailed { .. } => "voicevox_dl_failed",
            NoticeKind::IrodoriUnavailable { .. } => "irodori_unavailable",
            NoticeKind::IrodoriDlComplete => "irodori_dl_complete",
            NoticeKind::IrodoriDlFailed { .. } => "irodori_dl_failed",
            NoticeKind::UpdateAvailable { .. } => "update_available",
        }
    }

    fn fallback_text(&self) -> String {
        match self {
            NoticeKind::CostWarning80 { provider } => {
                format!("LLM 月次コストが上限の 80% に到達しました ({provider})")
            }
            NoticeKind::CostLimitExceeded { provider } => {
                format!("LLM 月次コストが上限を超過しました ({provider})。低負荷モードに降格します")
            }
            NoticeKind::ModeDegraded { reason } => match reason {
                DegradeReason::ApiError => "API エラーが続いたので一時的に低負荷モードへ切り替えました".to_string(),
                DegradeReason::CostLimit => "コスト上限超過により低負荷モードへ降格しました".to_string(),
            },
            NoticeKind::ModeRecovered => "通常モードに復帰しました".to_string(),
            NoticeKind::VoicevoxDlComplete => "VOICEVOX の音声資産ダウンロードが完了しました".to_string(),
            NoticeKind::VoicevoxDlFailed { reason } => {
                format!("VOICEVOX 音声資産のダウンロードに失敗しました: {reason}")
            }
            NoticeKind::IrodoriUnavailable { reason } => {
                format!("Irodori-TTS が利用できません: {reason}。VOICEVOX 経路で発話します")
            }
            NoticeKind::IrodoriDlComplete => {
                "Irodori-TTS の Python ランタイム導入が完了しました".to_string()
            }
            NoticeKind::IrodoriDlFailed { reason } => {
                format!("Irodori-TTS の導入に失敗しました: {reason}")
            }
            NoticeKind::UpdateAvailable { version } => {
                format!("ugg の新しいバージョン {version} が出ています")
            }
        }
    }
}

pub async fn notify(app: &AppHandle, state: &Arc<AppState>, kind: NoticeKind) {
    let key = kind.dict_key();
    let line = {
        let guard = state.ghost.lock().expect("ghost poisoned");
        match guard.as_ref() {
            Ok(b) => b
                .dictionary
                .pick_system_message(key, &WhenContext::now(), b.sub_available()),
            Err(_) => None,
        }
    };

    match line {
        Some(line) => {
            let resp: DialogueResponse = banter::pattern_1("system_message", "low", line);
            if let Err(err) = app.emit("dialogue", &resp) {
                eprintln!("[notify] dialogue emit failed: {err}");
            }
        }
        None => {
            // 辞書未定義 → トースト fallback。フロントが拾わなければ console.error 相当。
            if let Err(err) = app.emit("system-toast", kind.fallback_text()) {
                eprintln!("[notify] toast emit failed: {err}");
            }
        }
    }
}

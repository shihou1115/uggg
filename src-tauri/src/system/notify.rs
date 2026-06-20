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

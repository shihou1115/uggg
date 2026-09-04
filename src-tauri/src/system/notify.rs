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
    // M7: ReminderFired variant は削除した。リマインダー発火は
    // `system::deliver::deliver_event` + 辞書 events.reminder_fired 経路に一本化
    // (daily-support-design §3/§7.1)。
}

#[derive(Debug, Clone)]
pub enum DegradeReason {
    ApiError,
    CostLimit,
}

impl NoticeKind {
    /// 辞書 `system_messages` のキー。
    ///
    /// **変種を増やすとこの match がコンパイルエラーになる**ので、キーの割り当て漏れは
    /// 起きない。一方「割り当てたキーが既定辞書に無い」は静かに起きる
    /// (`cost_warning_80` / `cost_limit_exceeded` が実際にそうだった) ため、
    /// 下の `dict_key_contract` テストが出荷辞書との突合を行う。
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
                crate::ulog!("[notify] dialogue emit failed: {err}");
            }
        }
        None => {
            // 辞書未定義 → トースト fallback。フロントが拾わなければ console.error 相当。
            if let Err(err) = app.emit("system-toast", kind.fallback_text()) {
                crate::ulog!("[notify] toast emit failed: {err}");
            }
        }
    }
}

/// `NoticeKind` が引く辞書キーが、**出荷している既定辞書に実在する**ことの契約テスト。
///
/// `cost_warning_80` / `cost_limit_exceeded` は `dict_key()` が以前から引きに来て
/// いたのに `ghosts/default/dic/main.yaml` に定義が無く、月額上限の 80% 到達も
/// 超過もキャラクターが黙っていた。コンパイルもテストも緑のまま出荷されていた。
#[cfg(test)]
mod dict_key_contract {
    use super::*;
    use std::collections::BTreeSet;

    /// 全変種のサンプル。**変種を増やしたらここにも足すこと。**
    ///
    /// 足し忘れは `sample_covers_every_variant` が件数で捕まえる
    /// （変種を増やすと `dict_key` の match がコンパイルエラーになるので、
    /// そこを直した開発者は必ずテストを走らせることになる）。
    fn all_kinds() -> Vec<NoticeKind> {
        vec![
            NoticeKind::CostWarning80 { provider: "openai".into() },
            NoticeKind::CostLimitExceeded { provider: "openai".into() },
            NoticeKind::ModeDegraded { reason: DegradeReason::ApiError },
            NoticeKind::ModeRecovered,
            NoticeKind::VoicevoxDlComplete,
            NoticeKind::VoicevoxDlFailed { reason: "x".into() },
            NoticeKind::IrodoriUnavailable { reason: "x".into() },
            NoticeKind::IrodoriDlComplete,
            NoticeKind::IrodoriDlFailed { reason: "x".into() },
            NoticeKind::UpdateAvailable { version: "1.0".into() },
        ]
    }

    /// サンプルが全変種を覆っていること（件数での歯止め）。
    #[test]
    fn sample_covers_every_variant() {
        let keys: BTreeSet<_> = all_kinds().iter().map(|k| k.dict_key()).collect();
        assert_eq!(
            keys.len(),
            10,
            "NoticeKind の変種を増やしたら all_kinds() にも足すこと（現在のキー: {keys:?}）"
        );
    }

    /// **全変種の辞書キーが既定辞書の system_messages に存在すること。**
    #[test]
    fn every_dict_key_exists_in_default_dictionary() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri の親")
            .join("ghosts/default/dic/main.yaml");
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("既定辞書を読めない {}: {e}", path.display()));
        let doc: serde_yaml::Value =
            serde_yaml::from_str(&raw).expect("既定辞書が YAML として壊れている");
        let sysmsgs = doc
            .get("system_messages")
            .and_then(|v| v.as_mapping())
            .expect("既定辞書に system_messages が無い");

        let missing: Vec<&str> = all_kinds()
            .iter()
            .map(|k| k.dict_key())
            .filter(|key| !sysmsgs.contains_key(serde_yaml::Value::from(*key)))
            .collect();
        assert!(
            missing.is_empty(),
            "Rust が引くのに既定辞書に無い system_messages キー: {missing:?}\n\
             （引けないとゴーストが黙る。辞書に足すこと）"
        );
    }
}

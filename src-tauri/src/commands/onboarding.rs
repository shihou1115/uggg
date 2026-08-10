use std::sync::Arc;

use chrono::Utc;
use tauri::{AppHandle, Emitter, State};

use crate::db::ProfileOrigin;
use crate::state::AppState;

const ONBOARDED_KEY: &str = "profile_onboarded";
/// 興味キーワードの上限 (設定パネルの `parseInterestList` と同値)。
const MAX_INTERESTS: usize = 20;

/// 初回オンボーディングの確定 (spec §4.2.5)。
/// 聞き取った 4 項目をそれぞれの保存先へ投入し、profile_onboarded フラグを立てる:
/// - nickname / talk_style / 興味 → `user_profile` (origin=onboarding。LLM の system prompt に注入)
/// - 興味 → `interest_topics` (時事ネタ RSS のキーワード、spec §4.4.6)
/// - topics_enabled → `Settings.topics_enabled` (**時事ネタの明示同意**。spec §3.3/§4.4.6 で
///   「既定オフ・オンボーディング同意必須」と定められている)
///
/// interests / topics_enabled は M2 時点では保存先 (interest_topics / 時事ネタ機能) が
/// 未実装だったため捨てていたが、M5 で両方そろったので結線した。
#[tauri::command]
pub fn complete_onboarding(
    nickname: Option<String>,
    interests: Vec<String>,
    talk_style: Option<String>,
    topics_enabled: bool,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let now = Utc::now().timestamp();

    if let Some(nick) = nickname.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let content = format!("ユーザーの呼び名は「{nick}」");
        state
            .db
            .insert_profile(&content, ProfileOrigin::Onboarding, None, now)
            .map_err(|err| format!("{err:#}"))?;
    }

    if let Some(style) = talk_style
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        let content = format!("話し方の希望: {style}");
        state
            .db
            .insert_profile(&content, ProfileOrigin::Onboarding, None, now)
            .map_err(|err| format!("{err:#}"))?;
    }

    let interests = normalize_interests(interests);
    if !interests.is_empty() {
        // (1) 長期記憶へ (spec §4.2.5「聞き取り → 自動投入」)
        let content = format!("興味のあること: {}", interests.join("、"));
        state
            .db
            .insert_profile(&content, ProfileOrigin::Onboarding, None, now)
            .map_err(|err| format!("{err:#}"))?;
        // (2) 時事ネタ RSS のキーワードへ (spec §4.4.6)。
        //     topics_enabled が false ならフェッチ自体が走らないので外部送信は起きない。
        state
            .db
            .replace_interests(&interests)
            .map_err(|err| format!("{err:#}"))?;
    }

    // 時事ネタの明示同意 (spec §3.3/§4.4.6: 既定オフ・オンボーディング同意必須)。
    // 同意されたときだけ設定を書き換える (既定が false のため、未同意なら何もしなくてよい)。
    // 永続化 + settings-changed の作法は set_settings / feedback_speech と共有する。
    if topics_enabled {
        let snapshot = {
            let mut s = state.settings.lock().expect("settings poisoned");
            s.topics_enabled = true;
            s.clone()
        };
        if let Ok(json) = serde_json::to_string(&snapshot) {
            let _ = state
                .db
                .set_setting(crate::commands::settings::SETTINGS_KEY, &json);
        }
        let _ = app.emit("settings-changed", &snapshot);
    }

    state
        .db
        .set_setting(ONBOARDED_KEY, "1")
        .map_err(|err| format!("{err:#}"))?;
    Ok(())
}

/// 興味キーワードの正規化 (純関数): 前後空白を落とし、空要素と重複を除き、上限まで。
/// フロント側 (`parseInterestList`) でも同じ規則で刈っているが、コマンドは他経路からも
/// 呼ばれうるのでバックエンドでも正規化する。
fn normalize_interests(interests: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::<String>::new();
    interests
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && seen.insert(s.clone()))
        .take(MAX_INTERESTS)
        .collect()
}

#[tauri::command]
pub fn skip_onboarding(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state
        .db
        .set_setting(ONBOARDED_KEY, "1")
        .map_err(|err| format!("{err:#}"))
}

/// boot payload 構築時に参照: オンボーディング済みかどうか。
pub fn is_onboarded(db: &crate::db::Db) -> bool {
    matches!(db.get_setting(ONBOARDED_KEY), Ok(Some(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn normalize_interests_trims_and_drops_empty() {
        assert_eq!(
            normalize_interests(v(&["  映画 ", "", "   ", "料理"])),
            v(&["映画", "料理"])
        );
    }

    #[test]
    fn normalize_interests_dedupes_keeping_first_order() {
        assert_eq!(
            normalize_interests(v(&["映画", "料理", "映画 ", "宇宙"])),
            v(&["映画", "料理", "宇宙"])
        );
    }

    #[test]
    fn normalize_interests_caps_at_max() {
        let many: Vec<String> = (0..30).map(|i| format!("topic{i}")).collect();
        let out = normalize_interests(many);
        assert_eq!(out.len(), MAX_INTERESTS);
        assert_eq!(out[0], "topic0");
        assert_eq!(out[MAX_INTERESTS - 1], format!("topic{}", MAX_INTERESTS - 1));
    }

    #[test]
    fn normalize_interests_empty_stays_empty() {
        // 空なら user_profile / interest_topics のどちらにも書かない (呼び出し側の分岐条件)
        assert!(normalize_interests(v(&["", "  "])).is_empty());
    }
}

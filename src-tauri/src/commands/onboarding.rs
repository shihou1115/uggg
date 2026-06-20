use std::sync::Arc;

use chrono::Utc;
use tauri::State;

use crate::db::ProfileOrigin;
use crate::state::AppState;

const ONBOARDED_KEY: &str = "profile_onboarded";

/// 初回オンボーディングの確定。
/// nickname / talk_style を user_profile (origin=onboarding) に投入し、
/// profile_onboarded フラグを立てる。
///
/// interests / topics_enabled は引数として受け取るが、interest_topics テーブルと
/// 時事ネタ機能が M5 スコープのため M2 では保存しない（フラグだけ進める）。
/// architecture §4.9 のシグネチャを将来そのまま満たせるよう引数は確定済み。
#[tauri::command]
pub fn complete_onboarding(
    nickname: Option<String>,
    interests: Vec<String>,
    talk_style: Option<String>,
    topics_enabled: bool,
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

    // interests / topics_enabled は M5 (時事ネタ) で本実装。
    // ここで握りつぶしているわけではなく、保存先テーブルがまだ無いだけ。
    let _ = (interests, topics_enabled);

    state
        .db
        .set_setting(ONBOARDED_KEY, "1")
        .map_err(|err| format!("{err:#}"))?;
    Ok(())
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

//! データ系コマンド (M5-G/E): チャットログ取得 / エクスポート / 履歴クリア。

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::json;
use tauri::State;

use crate::db::ChatLogRow;
use crate::state::AppState;

/// M5-G: 新しい順に N 件の chat_log を返す。
#[tauri::command]
pub fn get_chat_log(
    limit: u32,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ChatLogRow>, String> {
    let limit = limit.clamp(1, 1000);
    state
        .db
        .list_recent_chat_log(limit)
        .map_err(|e| format!("{e:#}"))
}

/// M5-E: 会話ログ・API 使用履歴・(option で) 記憶 を JSON でダウンロードフォルダに書き出す。
/// 戻り値: 書き出した絶対パス。
/// 1 テーブル分の読み取り結果を「出せた / 出せなかった」に振り分ける (v0.5.3)。
///
/// **破損 DB からの部分救出が目的**なので、失敗しても呼び出し側は続行する。
/// 失敗はログと `failed_tables` の両方に残す (ファイルだけ見ても欠落が分かるように)。
fn rescue<T>(
    table: &str,
    res: anyhow::Result<T>,
    failed: &mut Vec<serde_json::Value>,
) -> Option<T> {
    match res {
        Ok(v) => Some(v),
        Err(err) => {
            crate::ulog!("[export] {table} の読み取りに失敗 (部分救出を続行): {err:#}");
            failed.push(json!({ "table": table, "error": format!("{err:#}") }));
            None
        }
    }
}

/// エクスポートの payload を組み立てる (v0.5.3 で切り出し)。
///
/// **部分救出**が仕事。v0.5.2 までは各テーブルの読み取りを `?` で伝播しており、
/// **1 つでも読めないと JSON を一切出さずに終了**していた。§4.5.5 と manual は
/// 「破損を検知したら、まず JSON エクスポートで手元に控えてから対処」と案内しているので、
/// **案内した先が、必要な場面でだけ失敗する**状態だった (Codex レビュー 2026-09-06)。
///
/// 読めたテーブルは出し、読めなかったものは `failed_tables` に理由つきで並べる。
/// コマンドから切り離してあるのは、**壊れたテーブルを含む DB で挙動を固定する**ため
/// (`State` と保存先ディレクトリに依存すると検査できない)。
fn build_export_payload(db: &crate::db::Db, include_profile: bool, ts: u64) -> serde_json::Value {
    let mut failed: Vec<serde_json::Value> = Vec::new();

    // chat_log は最大 10000 件まで (DB が肥大化しても export を破綻させない上限)
    let chat = rescue("chat_log", db.list_recent_chat_log(10000), &mut failed);
    let usage = rescue("api_usage", db.list_api_usage(), &mut failed);
    let profile = if include_profile {
        rescue("user_profile", db.list_profile(), &mut failed)
    } else {
        None
    };
    let reminders = rescue(
        "reminders",
        db.list_reminders(crate::db::ReminderFilter::All),
        &mut failed,
    );
    let reminder_log = rescue("reminder_log", db.list_all_reminder_log(10000), &mut failed);
    let todos = rescue("todos", db.list_todos(None), &mut failed);
    let interests = rescue("interest_topics", db.list_interests(), &mut failed);
    let voice_refs = rescue("voice_refs", db.list_voice_refs(), &mut failed);
    let app_settings = rescue("app_settings", db.list_all_settings(), &mut failed)
        .map(|v| v.into_iter().collect::<std::collections::BTreeMap<_, _>>());

    json!({
        "schema": "ugg-export-v3",
        "exported_at": ts,
        "include_profile": include_profile,
        // 再生成できるキャッシュ (calendar_cache / topics_cache / monologue_cache) は
        // 意図的に含めない。持ち出す価値のあるユーザーデータだけを出す。
        "omitted_caches": ["calendar_cache", "topics_cache", "monologue_cache"],
        // **読み取りに失敗したテーブル (v0.5.3)。** 空配列なら全件読めている。
        // 該当テーブルの値は null になる。`include_profile=false` による
        // `user_profile: null` とは意味が違うので、区別はこの配列で行う。
        "failed_tables": failed,
        "chat_log": chat,
        "api_usage": usage,
        "user_profile": profile,
        "reminders": reminders,
        "reminder_log": reminder_log,
        "todos": todos,
        "interest_topics": interests,
        "voice_refs": voice_refs,
        "app_settings": app_settings,
    })
}

#[tauri::command]
pub fn export_data(
    include_profile: bool,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let payload = build_export_payload(&state.db, include_profile, ts);

    let dir = dirs::download_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| "ダウンロードフォルダが解決できませんでした".to_string())?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("保存先の作成に失敗: {e}"))?;
    let path = dir.join(format!("ugg-export-{ts}.json"));
    let body = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("JSON 整形に失敗: {e}"))?;
    std::fs::write(&path, body).map_err(|e| format!("書き出しに失敗: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

/// M5-E: 履歴クリア。常に chat_log を全件削除、`include_profile=true` で user_profile も全削除。
/// (origin に関係なく削除する仕様。記憶を残したいなら include_profile=false。)
///
/// M14: advanced 独り言のストックも**常に**消す (spec §4.4.4「履歴クリアの対象に含める」)。
/// `include_profile` に依存させないのは、これが記憶ではなく生成物のキャッシュで、
/// chat_log と同格だから (foundation-design §3.6)。
#[tauri::command]
pub fn clear_history(
    include_profile: bool,
    state: State<'_, Arc<AppState>>,
) -> Result<ClearResult, String> {
    state.db.clear_chat_log().map_err(|e| format!("{e:#}"))?;
    state
        .db
        .clear_monologue_cache()
        .map_err(|e| format!("{e:#}"))?;
    let mut cleared_profiles: u64 = 0;
    if include_profile {
        cleared_profiles = state.db.clear_user_profile().map_err(|e| format!("{e:#}"))?;
    }
    Ok(ClearResult {
        chat_cleared: true,
        profile_cleared_count: cleared_profiles,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ClearResult {
    pub chat_cleared: bool,
    pub profile_cleared_count: u64,
}

/// M5-D: 設定パネルの「いますぐチェック」ボタンから呼ぶ。
/// `update_feed_url` が未設定なら明示エラー、更新なしなら Ok でメッセージなし。
#[tauri::command]
pub async fn check_update_now(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let url = state
        .settings
        .lock()
        .expect("settings poisoned")
        .update_feed_url
        .clone();
    if url.is_none() {
        return Err("更新フィードの URL が設定されていません".to_string());
    }
    let state_arc = state.inner().clone();
    crate::system::update::check_update_once(&app, &state_arc)
        .await
        .map_err(|e| format!("{e:#}"))
}

/// 起動時の DB 整合性検査の結果 (v0.5.1、spec §4.5.5)。
///
/// 破損していても DB は作り直さない（リマインダー・ToDo・記憶が消えるため）。
/// 検知した事実と退避先をユーザーに見せ、どうするかは本人が決める。
#[tauri::command]
pub fn get_db_health(state: State<'_, Arc<AppState>>) -> crate::db::DbIntegrity {
    state.db.integrity().clone()
}

#[cfg(test)]
mod export_tests {
    use super::*;
    use crate::db::Db;

    /// リマインダーだけ入った DB を作る。パスも返す (テーブルを落とすのに使う)。
    fn db_with_data() -> (tempfile::TempDir, Db, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("companion.db");
        let db = Db::open(&path).unwrap();
        db.migrate().unwrap();
        db.insert_reminder(1_800_000_000, "牛乳を買う", 1_700_000_000)
            .unwrap();
        (dir, db, path)
    }

    /// テーブルを読めなくする。**本番 API を増やさない**ため、別コネクションから落とす
    /// (実破損を作るより意図が明確で、再現も安定する)。
    fn drop_table(path: &std::path::Path, table: &str) {
        let c = rusqlite::Connection::open(path).unwrap();
        c.execute_batch(&format!("DROP TABLE {table};")).unwrap();
    }

    #[test]
    fn healthy_db_exports_everything_and_reports_no_failure() {
        let (_dir, db, _path) = db_with_data();
        let v = build_export_payload(&db, true, 42);
        assert_eq!(v["schema"], "ugg-export-v3");
        assert_eq!(v["exported_at"], 42);
        assert_eq!(
            v["failed_tables"].as_array().unwrap().len(),
            0,
            "健全な DB で失敗が報告されている: {}",
            v["failed_tables"]
        );
        assert!(v["reminders"].is_array());
        assert_eq!(v["reminders"].as_array().unwrap().len(), 1);
        assert!(v["chat_log"].is_array());
    }

    /// **1 テーブルが読めなくても、読めた分は出す (v0.5.3)。**
    ///
    /// v0.5.2 までは最初の失敗で `?` が伝播し、**JSON を一切出さずに終了**していた。
    /// §4.5.5 と manual が「破損したらまず export で控えて」と案内している以上、
    /// **案内した先が必要な場面でだけ失敗する**のは通らない
    /// (Codex レビュー 2026-09-06。同レビューも「チャットだけ壊してもリマインダーは
    /// 読める」ことを実験で確認している)。
    #[test]
    fn unreadable_table_does_not_abort_the_whole_export() {
        let (_dir, db, path) = db_with_data();
        // chat_log だけ読めなくする (実破損の代わりにテーブルごと落とす)。
        drop_table(&path, "chat_log");

        let v = build_export_payload(&db, true, 42);

        // 失敗したテーブルは名前と理由が残り、値は null になる。
        let failed = v["failed_tables"].as_array().unwrap();
        assert_eq!(failed.len(), 1, "failed_tables: {}", v["failed_tables"]);
        assert_eq!(failed[0]["table"], "chat_log");
        assert!(!failed[0]["error"].as_str().unwrap().is_empty());
        assert!(v["chat_log"].is_null());

        // **読めた分はちゃんと出ている**のが要点。
        assert_eq!(
            v["reminders"].as_array().unwrap().len(),
            1,
            "読めるはずのリマインダーまで落ちている"
        );
        assert!(v["app_settings"].is_object());
    }

    /// `include_profile=false` の null と、読み取り失敗の null を取り違えない。
    #[test]
    fn excluded_profile_is_not_reported_as_failure() {
        let (_dir, db, _path) = db_with_data();
        let v = build_export_payload(&db, false, 42);
        assert!(v["user_profile"].is_null());
        assert_eq!(
            v["failed_tables"].as_array().unwrap().len(),
            0,
            "除外を失敗として報告している"
        );
    }
}

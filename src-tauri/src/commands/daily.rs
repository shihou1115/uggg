//! 日常支援コマンド (M7/M8、spec §4.6.1・§4.6.2 / daily-support-design §7.1・§7.2・§8.1)。
//!
//! - リマインダー (M7): 一覧・登録・完了・無視・スヌーズ・削除・編集・通知履歴。
//!   変更系は `reminders-changed` を emit。
//! - ToDo (M8): 追加・一覧・完了・削除・編集。変更系は `todos-changed` を emit。
//!   完了時はキャラが労う (deliver 経由・ガバナンス下)。
//! 登録の主経路はチャット自然文 (`dialogue::run_dispatch`) で、パネルは確認・編集用。

use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::db::{ReminderFilter, ReminderKind, ReminderLogRow, ReminderRow, TodoRow};
use crate::state::AppState;
use crate::system::deliver;
use crate::system::governance::{Priority, SpeechCategory};
use crate::tools::{reminder, todo};

/// フロント向けリマインダー 1 件。`ReminderRow` と同じフィールドを持つ
/// (3 表現の同期、daily-support-design §8.4。TS 側は types.ts の ReminderEntry)。
#[derive(Debug, Clone, Serialize)]
pub struct ReminderEntry {
    pub id: i64,
    pub due_ts: i64,
    pub text: String,
    pub created_ts: i64,
    pub kind: ReminderKind,
    pub weekday_mask: u8,
    pub time_of_day: i32,
    pub active: bool,
    pub base_due_ts: Option<i64>,
    /// 通知済み・未処理 (ack='fired' のログが残っている)。
    pub pending: bool,
}

impl From<ReminderRow> for ReminderEntry {
    fn from(r: ReminderRow) -> Self {
        Self {
            id: r.id,
            due_ts: r.due_ts,
            text: r.text,
            created_ts: r.created_ts,
            kind: r.kind,
            weekday_mask: r.weekday_mask,
            time_of_day: r.time_of_day,
            active: r.active,
            base_due_ts: r.base_due_ts,
            pending: r.pending,
        }
    }
}

/// update_reminder の部分更新。指定した項目だけ変更する。
#[derive(Debug, Clone, Deserialize)]
pub struct ReminderPatch {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub due_ts: Option<i64>,
}

fn parse_filter(filter: Option<String>) -> ReminderFilter {
    match filter.as_deref() {
        Some("completed") => ReminderFilter::Completed,
        Some("all") => ReminderFilter::All,
        // 既定は「要対応」(予定あり or 未処理の発火あり)
        _ => ReminderFilter::Active,
    }
}

fn list_entries(state: &Arc<AppState>, filter: ReminderFilter) -> Result<Vec<ReminderEntry>, String> {
    let rows = reminder::list(state, filter).map_err(|e| format!("{e:#}"))?;
    Ok(rows.into_iter().map(ReminderEntry::from).collect())
}

fn emit_changed(app: &AppHandle) {
    let _ = app.emit("reminders-changed", ());
}

#[tauri::command]
pub fn list_reminders(
    filter: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ReminderEntry>, String> {
    list_entries(state.inner(), parse_filter(filter))
}

/// 自然文からの登録 (パネルの追加欄用。会話経路と同じパーサを通る)。
/// 戻り値は Active フィルタの一覧。
#[tauri::command]
pub fn add_reminder_nl(
    text: String,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ReminderEntry>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("予定の文を入力してください".to_string());
    }
    let now_local = chrono::Local::now().naive_local();
    let parsed = reminder::parse_reminder(trimmed, now_local).ok_or_else(|| {
        "時刻を読み取れませんでした（例:「3分後にお茶」「明日の朝ゴミ出し」「毎週月曜9時に会議」）"
            .to_string()
    })?;
    // パネル登録で本文が取れないときは入力全文を本文にする
    reminder::register(state.inner(), &parsed, trimmed).map_err(|e| format!("{e:#}"))?;
    emit_changed(&app);
    list_entries(state.inner(), ReminderFilter::Active)
}

/// 単発 (once) の内部 API (M5-B 互換。offset_secs は現在時刻からの相対秒)。
#[tauri::command]
pub fn add_reminder(
    text: String,
    offset_secs: i64,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ReminderEntry>, String> {
    let body = text.trim();
    if body.is_empty() {
        return Err("リマインダー本文が空です".to_string());
    }
    if offset_secs <= 0 {
        return Err("offset_secs は正の値を指定してください".to_string());
    }
    reminder::add(state.inner(), body, offset_secs).map_err(|e| format!("{e:#}"))?;
    emit_changed(&app);
    list_entries(state.inner(), ReminderFilter::Active)
}

/// 完了: 最新の未処理発火ログを ack='completed' にする。once は再発火も停止する
/// (未発火のうちに完了した場合は「先に済ませた」扱いで鳴らさない)。
#[tauri::command]
pub fn complete_reminder(
    id: i64,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ReminderEntry>, String> {
    ack_reminder(state.inner(), id, "completed")?;
    emit_changed(&app);
    list_entries(state.inner(), ReminderFilter::Active)
}

/// 無視: ack='dismissed'。once は再発火も停止する。
#[tauri::command]
pub fn dismiss_reminder(
    id: i64,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ReminderEntry>, String> {
    ack_reminder(state.inner(), id, "dismissed")?;
    emit_changed(&app);
    list_entries(state.inner(), ReminderFilter::Active)
}

fn ack_reminder(state: &Arc<AppState>, id: i64, ack: &str) -> Result<(), String> {
    let row = state
        .db
        .get_reminder(id)
        .map_err(|e| format!("{e:#}"))?
        .ok_or_else(|| format!("リマインダー {id} が見つかりません"))?;
    let now = Utc::now().timestamp();
    state.db.set_ack(id, ack, now).map_err(|e| format!("{e:#}"))?;
    if row.kind == ReminderKind::Once {
        state.db.deactivate_reminder(id).map_err(|e| format!("{e:#}"))?;
    }
    Ok(())
}

/// スヌーズ: due を mins 分後へ延ばす (既定 10 分はフロント側)。
/// 本来時刻は base_due_ts に保持され、once でも再度鳴る (active=1 に戻す)。
#[tauri::command]
pub fn snooze_reminder(
    id: i64,
    mins: u32,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ReminderEntry>, String> {
    if mins == 0 || mins > 24 * 60 {
        return Err("スヌーズは 1〜1440 分で指定してください".to_string());
    }
    let row = state
        .db
        .get_reminder(id)
        .map_err(|e| format!("{e:#}"))?
        .ok_or_else(|| format!("リマインダー {id} が見つかりません"))?;
    let now = Utc::now().timestamp();
    let new_due = now + (mins as i64) * 60;
    state
        .db
        .snooze_reminder(id, row.due_ts, new_due)
        .map_err(|e| format!("{e:#}"))?;
    // ユーザー操作の確認発話 (辞書 events.reminder_snoozed、ゲート非対象)。
    // 辞書未定義なら黙る (パネルの一覧更新で足りる)。
    let time_label = format!("{mins}分後");
    let _ = deliver::speak_event_now(
        &app,
        state.inner(),
        "reminder_snoozed",
        &[("body", row.text.as_str()), ("time", time_label.as_str())],
    );
    emit_changed(&app);
    list_entries(state.inner(), ReminderFilter::Active)
}

#[tauri::command]
pub fn delete_reminder(
    id: i64,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ReminderEntry>, String> {
    reminder::delete(state.inner(), id).map_err(|e| format!("{e:#}"))?;
    emit_changed(&app);
    list_entries(state.inner(), ReminderFilter::Active)
}

/// 本文・時刻の編集 (部分更新)。
#[tauri::command]
pub fn update_reminder(
    id: i64,
    patch: ReminderPatch,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ReminderEntry>, String> {
    if let Some(text) = &patch.text {
        if text.trim().is_empty() {
            return Err("本文を空にはできません".to_string());
        }
    }
    state
        .db
        .update_reminder(id, patch.text.as_deref().map(str::trim), patch.due_ts)
        .map_err(|e| format!("{e:#}"))?;
    emit_changed(&app);
    list_entries(state.inner(), ReminderFilter::Active)
}

/// 通知履歴 (新しい順、最大 50 件)。
#[tauri::command]
pub fn get_reminder_log(
    id: i64,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ReminderLogRow>, String> {
    state.db.list_reminder_log(id, 50).map_err(|e| format!("{e:#}"))
}

// ===== ToDo (M8) =====

/// フロント向け ToDo 1 件。`TodoRow` と同じフィールド (3 表現の同期、TS は types.ts の TodoEntry)。
#[derive(Debug, Clone, Serialize)]
pub struct TodoEntry {
    pub id: i64,
    pub text: String,
    pub bucket: String,
    pub priority: i32,
    pub recurring: Option<String>,
    pub status: String,
    pub done_ts: Option<i64>,
    pub created_ts: i64,
    pub sort_order: i64,
}

impl From<TodoRow> for TodoEntry {
    fn from(r: TodoRow) -> Self {
        Self {
            id: r.id,
            text: r.text,
            bucket: r.bucket,
            priority: r.priority,
            recurring: r.recurring,
            status: r.status,
            done_ts: r.done_ts,
            created_ts: r.created_ts,
            sort_order: r.sort_order,
        }
    }
}

/// update_todo の部分更新。指定した項目だけ変更する。
/// recurring は「省略=変更しない / null=日課解除 / "daily"|"weekly"=設定」の三値。
#[derive(Debug, Clone, Deserialize)]
pub struct TodoPatch {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub bucket: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    /// 外側 None = 変更しない、Some(None) = 日課解除。
    /// serde では `"recurring": null` が Some(None) にならないため
    /// `deserialize_with` で「キー存在 = Some」に倒す。
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub recurring: Option<Option<String>>,
}

/// `"recurring": null` (クリア) と キー省略 (変更なし) を区別するためのヘルパ。
/// フィールドがあれば Some(値 or None)、無ければ default の None。
fn deserialize_double_option<'de, D>(de: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<String>::deserialize(de)?))
}

fn list_todo_entries(state: &Arc<AppState>, bucket: Option<&str>) -> Result<Vec<TodoEntry>, String> {
    let rows = state.db.list_todos(bucket).map_err(|e| format!("{e:#}"))?;
    Ok(rows.into_iter().map(TodoEntry::from).collect())
}

fn emit_todos_changed(app: &AppHandle) {
    let _ = app.emit("todos-changed", ());
}

/// 一覧。bucket 省略で全件 (open 先・優先度高先)。
#[tauri::command]
pub fn list_todos(
    bucket: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<TodoEntry>, String> {
    if let Some(b) = &bucket {
        todo::validate_bucket(b).map_err(|e| format!("{e:#}"))?;
    }
    list_todo_entries(state.inner(), bucket.as_deref())
}

#[tauri::command]
pub fn add_todo(
    text: String,
    bucket: String,
    priority: i32,
    recurring: Option<String>,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<TodoEntry>, String> {
    let body = text.trim();
    if body.is_empty() {
        return Err("ToDo の内容を入力してください".to_string());
    }
    todo::validate_bucket(&bucket).map_err(|e| format!("{e:#}"))?;
    todo::validate_priority(priority).map_err(|e| format!("{e:#}"))?;
    todo::validate_recurring(recurring.as_deref()).map_err(|e| format!("{e:#}"))?;
    let now = Utc::now().timestamp();
    state
        .db
        .insert_todo(body, &bucket, priority, recurring.as_deref(), now)
        .map_err(|e| format!("{e:#}"))?;
    emit_todos_changed(&app);
    list_todo_entries(state.inner(), None)
}

/// 完了。労いの発話はガバナンス下 (Todo/Ambient) で配達する — 集中・静音中は黙る。
/// async なのは deliver_event を await するため (busy は try_acquire なのでブロックしない)。
#[tauri::command]
pub async fn complete_todo(
    id: i64,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<TodoEntry>, String> {
    let row = state
        .db
        .get_todo(id)
        .map_err(|e| format!("{e:#}"))?
        .ok_or_else(|| format!("ToDo {id} が見つかりません"))?;
    if row.status == "done" {
        // 既に完了 (パネルの二重クリック等) は no-op で現状を返す
        return list_todo_entries(state.inner(), None);
    }
    let now = Utc::now().timestamp();
    state
        .db
        .set_todo_status(id, "done", now)
        .map_err(|e| format!("{e:#}"))?;
    emit_todos_changed(&app);
    // 労い (辞書 events.todo_done、{body}=本文)。抑制・辞書未定義なら黙る (fallback なし)。
    let _ = deliver::deliver_event(
        &app,
        state.inner(),
        SpeechCategory::Todo,
        Priority::Ambient,
        "todo_done",
        &[("body", row.text.as_str())],
        None,
    )
    .await;
    list_todo_entries(state.inner(), None)
}

/// done → open へ戻す (パネルのチェック解除)。
#[tauri::command]
pub fn reopen_todo(
    id: i64,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<TodoEntry>, String> {
    let now = Utc::now().timestamp();
    state
        .db
        .set_todo_status(id, "open", now)
        .map_err(|e| format!("{e:#}"))?;
    emit_todos_changed(&app);
    list_todo_entries(state.inner(), None)
}

#[tauri::command]
pub fn delete_todo(
    id: i64,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<TodoEntry>, String> {
    state.db.delete_todo(id).map_err(|e| format!("{e:#}"))?;
    emit_todos_changed(&app);
    list_todo_entries(state.inner(), None)
}

#[tauri::command]
pub fn update_todo(
    id: i64,
    patch: TodoPatch,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<TodoEntry>, String> {
    if let Some(text) = &patch.text {
        if text.trim().is_empty() {
            return Err("ToDo の内容を空にはできません".to_string());
        }
    }
    if let Some(b) = &patch.bucket {
        todo::validate_bucket(b).map_err(|e| format!("{e:#}"))?;
    }
    if let Some(p) = patch.priority {
        todo::validate_priority(p).map_err(|e| format!("{e:#}"))?;
    }
    if let Some(rec) = &patch.recurring {
        todo::validate_recurring(rec.as_deref()).map_err(|e| format!("{e:#}"))?;
    }
    state
        .db
        .update_todo(
            id,
            patch.text.as_deref().map(str::trim),
            patch.bucket.as_deref(),
            patch.priority,
            patch.recurring.as_ref().map(|r| r.as_deref()),
        )
        .map_err(|e| format!("{e:#}"))?;
    emit_todos_changed(&app);
    list_todo_entries(state.inner(), None)
}

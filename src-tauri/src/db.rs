use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

pub struct Db {
    conn: Mutex<Connection>,
    /// 起動時に 1 回だけ行う整合性検査の結果 (v0.5.1)。
    integrity: DbIntegrity,
}

/// `PRAGMA quick_check` の結果と、破損時に作った退避ファイル。
///
/// **破損しても DB は作り直さない。** 作り直すとリマインダー・ToDo・記憶が消える。
/// 検知して記録し、原本のコピーと（可能なら）救出コピーを残すところまでを行い、
/// どうするかはユーザーが決める。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DbIntegrity {
    /// `quick_check` が "ok" を返したか。検査できなかった場合も false。
    pub ok: bool,
    /// 検査の生の結果（"ok" またはエラー本文）。
    pub detail: String,
    /// 破損時に作った原本のコピー。
    pub backup_path: Option<String>,
    /// 破損時に `VACUUM INTO` で救出できた分のコピー（成功した場合のみ）。
    pub salvaged_path: Option<String>,
}

// ===== Domain types =====

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Main,
    Sub,
}

impl ChatRole {
    fn as_str(&self) -> &'static str {
        match self {
            ChatRole::User => "user",
            ChatRole::Main => "main",
            ChatRole::Sub => "sub",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileOrigin {
    Manual,
    Onboarding,
    Auto,
}

impl ProfileOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProfileOrigin::Manual => "manual",
            ProfileOrigin::Onboarding => "onboarding",
            ProfileOrigin::Auto => "auto",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "manual" => Self::Manual,
            "onboarding" => Self::Onboarding,
            _ => Self::Auto,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProfileEntry {
    pub id: i64,
    pub content: String,
    pub origin: ProfileOrigin,
    /// カンマ区切り、recall トリガー用。
    pub source_keywords: Option<String>,
    pub ts: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ApiUsageRow {
    pub provider: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_usd: f64,
    pub ts: i64,
}

/// API 使用量サマリ。M6 でリリースノート/設定パネルの使用状況表示に使う想定で残す。
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct UsageSummary {
    pub month_usd: f64,
    pub limit_usd: f64,
    pub limited: bool,
}

/// リマインダーの繰り返し種別 (M7, daily-support-design §2.1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderKind {
    Once,
    Daily,
    Weekly,
}

impl ReminderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReminderKind::Once => "once",
            ReminderKind::Daily => "daily",
            ReminderKind::Weekly => "weekly",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "daily" => Self::Daily,
            "weekly" => Self::Weekly,
            _ => Self::Once,
        }
    }
}

/// リマインダー 1 件 (M5-B、M7 で v6 拡張)。
/// reminders はスケジュール定義であり、発火・確認の履歴は `reminder_log` に分離する
/// (発火 ≠ 完了、daily-support-design §2.1)。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReminderRow {
    pub id: i64,
    /// 次回発火予定 (UTC 秒)。
    pub due_ts: i64,
    pub text: String,
    pub created_ts: i64,
    pub kind: ReminderKind,
    /// weekly のみ使用: bit0=月 .. bit6=日。
    pub weekday_mask: u8,
    /// daily/weekly のみ使用: ローカル 0:00 からの秒 (§2.5 TZ 契約)。
    pub time_of_day: i32,
    /// 1=有効。once は発火到達・完了・dismiss で 0 (再発火停止)。
    pub active: bool,
    /// スヌーズ前の本来時刻 (UTC 秒)。スヌーズしていなければ None。
    pub base_due_ts: Option<i64>,
    /// 導出列: ack='fired' のログが残っている (=通知したが未処理)。
    /// テーブル列ではなく SELECT 時に reminder_log から EXISTS で計算する。
    pub pending: bool,
}

/// リマインダー発火・確認履歴 1 件 (M7, daily-support-design §2.1)。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReminderLogRow {
    pub id: i64,
    pub reminder_id: i64,
    /// 発火 (配達試行) 時刻 UTC 秒。
    pub fired_ts: i64,
    /// 'fired' | 'completed' | 'dismissed'。
    pub ack: String,
    pub ack_ts: Option<i64>,
    /// 配達結果 (§3.1 DeliveryOutcome): 'ghost' | 'toast' | 'deferred' | 'failed'。
    pub delivery: String,
}

/// list_reminders のフィルタ (M7)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderFilter {
    /// 予定あり (active=1) または未処理の発火が残っている (要対応)。
    Active,
    /// 終了済み: active=0 かつ未処理の発火なし。
    Completed,
    All,
}

/// カレンダー予定の 1 発生インスタンス (M10, spec §4.6.4 / daily-support-design §2.3)。
/// 繰り返しは near-term 展開して発生回ごとに 1 行持つ。時刻はすべて UTC 秒 (§2.5)。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CalendarCacheRow {
    pub source_id: i64,
    pub uid: String,
    pub recurrence_id: Option<String>,
    pub summary: String,
    pub start_ts: i64,
    pub end_ts: Option<i64>,
    pub all_day: bool,
    /// 'confirmed' | 'cancelled'。
    pub status: String,
    /// 繰り返しだが RRULE を near-term 展開できなかった予定 (当日分のみ・UI に印)。
    pub unsupported: bool,
    pub notified: bool,
}

/// ToDo 1 件 (M8, spec §4.6.2 / daily-support-design §2.2)。
/// bucket/status/recurring は文字列のまま持ち、正規化・検証はコマンド層
/// (`tools::todo`) が行う (TS 側の文字列 union と 1:1 のため。enum 化はしない)。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TodoRow {
    pub id: i64,
    pub text: String,
    /// 'today' | 'week' | 'someday'。
    pub bucket: String,
    /// 0=普通, 1=高 (2 段階のみ、spec §4.6.2)。
    pub priority: i32,
    /// None | 'daily' | 'weekly' (日課)。
    pub recurring: Option<String>,
    /// 'open' | 'done'。
    pub status: String,
    pub done_ts: Option<i64>,
    pub created_ts: i64,
    pub sort_order: i64,
}

/// 興味分野 1 件 (M5-C, architecture §2.2)。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InterestTopic {
    pub id: i64,
    pub topic: String,
    pub enabled: bool,
}

/// 時事ネタキャッシュ 1 件 (M5-C)。
/// M14 で advanced 独り言の補充 (`system::monologue`) が材料として読み出す。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TopicCacheRow {
    pub id: i64,
    pub topic: String,
    pub headline: String,
    pub link: String,
    pub fetched_ts: i64,
}

/// advanced 独り言ストック 1 件 (M14, foundation-design §3.2)。
/// `pop_monologue_cache` が返す「そのまま喋れる 1 件」。ghost_id / created_ts は
/// pop 時の選別で使い切るので持たない (消費側に用途が無いフィールドは生やさない)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonologueCacheRow {
    pub text: String,
    pub pose: Option<String>,
    /// 織り込んだ見出しの取得時刻。時事ネタを含まない文は None。
    pub topic_fetched_ts: Option<i64>,
}

/// 時事ネタの賞味期限 (spec §4.4.6「賞味期限は 1 週間」)。
/// **織り込み時** (`system::monologue` の材料選別) と**発話時** (`pop_monologue_cache`)
/// の二段で同じ値を使う — 片方だけずれると「古いネタを喋る / 使える材料を捨てる」に化けるため、
/// 定義は 1 箇所に置いて両方から参照する。
pub const TOPIC_MAX_AGE_SECS: i64 = 7 * 24 * 60 * 60;

/// 時事ネタを含まない独り言の失効 (foundation-design §3.2)。
/// 人格や状況が変わっている可能性があり、無期限に持ち越す理由が無い。
pub const MONOLOGUE_MAX_AGE_SECS: i64 = 30 * 24 * 60 * 60;

/// チャットログ 1 件 (M5-G: get_chat_log の返却型)。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatLogRow {
    pub id: i64,
    pub ts: i64,
    pub mode: String,
    pub role: ChatRole,
    pub text: String,
    pub pose: Option<String>,
}

/// Irodori 参照音声 1 件分のメタ (architecture §2.2 voice_refs)。
/// `file_path` は `%APPDATA%\ugg\irodori\refs\<slot>_<id>.wav` を指す。
/// MVP では `UNIQUE(slot)` で各 slot 最新 1 件のみ保持する。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VoiceRefRow {
    pub id: i64,
    pub slot: String,
    pub caption: String,
    pub file_path: String,
    pub created_ts: i64,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create db parent dir: {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open sqlite database: {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("set WAL journal mode")?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .context("enable foreign keys")?;
        let integrity = check_integrity_and_preserve(&conn, path);
        Ok(Self {
            conn: Mutex::new(conn),
            integrity,
        })
    }

    /// 起動時の `PRAGMA quick_check` の結果。`None` = 検査自体ができなかった。
    pub fn integrity(&self) -> &DbIntegrity {
        &self.integrity
    }

    pub fn migrate(&self) -> Result<()> {
        let mut guard = self.conn.lock().expect("db poisoned");
        let tx = guard.transaction().context("begin migration tx")?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS app_settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )
        .context("create app_settings")?;

        let current: i64 = tx
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM app_settings WHERE key = 'db_schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if current < 1 {
            // M0/M1: app_settings のみ
            tx.execute_batch(
                "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('db_schema_version', '1');",
            )
            .context("write db_schema_version=1")?;
        }

        if current < 2 {
            // M2: chat_log / user_profile / api_usage を追加
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS chat_log (
                    id   INTEGER PRIMARY KEY AUTOINCREMENT,
                    ts   INTEGER NOT NULL,
                    mode TEXT NOT NULL,
                    role TEXT NOT NULL,
                    text TEXT NOT NULL,
                    pose TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_chat_log_ts ON chat_log(ts);

                CREATE TABLE IF NOT EXISTS user_profile (
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    content         TEXT NOT NULL,
                    origin          TEXT NOT NULL,
                    source_keywords TEXT,
                    ts              INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_user_profile_origin ON user_profile(origin);

                CREATE TABLE IF NOT EXISTS api_usage (
                    id               INTEGER PRIMARY KEY AUTOINCREMENT,
                    ts               INTEGER NOT NULL,
                    provider         TEXT NOT NULL,
                    model            TEXT NOT NULL,
                    prompt_tokens    INTEGER NOT NULL,
                    completion_tokens INTEGER NOT NULL,
                    cost_usd         REAL NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_api_usage_ts ON api_usage(ts);

                INSERT OR REPLACE INTO app_settings (key, value) VALUES ('db_schema_version', '2');",
            )
            .context("migrate to schema v2")?;
        }

        if current < 3 {
            // M4c: Irodori 参照音声メタ (architecture §2.2)。
            // UNIQUE(slot) で 1 slot 1 件、再生成は INSERT OR REPLACE で上書きする。
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS voice_refs (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    slot       TEXT NOT NULL,
                    caption    TEXT NOT NULL,
                    file_path  TEXT NOT NULL,
                    created_ts INTEGER NOT NULL,
                    UNIQUE(slot)
                );
                CREATE INDEX IF NOT EXISTS idx_voice_refs_slot ON voice_refs(slot);

                INSERT OR REPLACE INTO app_settings (key, value) VALUES ('db_schema_version', '3');",
            )
            .context("migrate to schema v3")?;
        }

        if current < 4 {
            // M5-C: 興味分野 + 時事ネタキャッシュ (architecture §2.2)。
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS interest_topics (
                    id      INTEGER PRIMARY KEY AUTOINCREMENT,
                    topic   TEXT NOT NULL UNIQUE,
                    enabled INTEGER NOT NULL DEFAULT 1
                );
                CREATE INDEX IF NOT EXISTS idx_interest_topics_enabled ON interest_topics(enabled);

                CREATE TABLE IF NOT EXISTS topics_cache (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    topic      TEXT NOT NULL,
                    headline   TEXT NOT NULL,
                    link       TEXT NOT NULL,
                    fetched_ts INTEGER NOT NULL,
                    UNIQUE(topic, headline)
                );
                CREATE INDEX IF NOT EXISTS idx_topics_cache_ts ON topics_cache(fetched_ts);

                INSERT OR REPLACE INTO app_settings (key, value) VALUES ('db_schema_version', '4');",
            )
            .context("migrate to schema v4")?;
        }

        if current < 5 {
            // M5-B: リマインダー。due_ts に達した行を watcher が消費する。
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS reminders (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    due_ts     INTEGER NOT NULL,
                    text       TEXT NOT NULL,
                    created_ts INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_reminders_due ON reminders(due_ts);

                INSERT OR REPLACE INTO app_settings (key, value) VALUES ('db_schema_version', '5');",
            )
            .context("migrate to schema v5")?;
        }

        if current < 6 {
            // M7: 統合リマインダー (daily-support-design §2.1)。
            // reminders = スケジュール定義、reminder_log = 発火・確認の履歴に分離する。
            tx.execute_batch(
                "ALTER TABLE reminders ADD COLUMN kind         TEXT    NOT NULL DEFAULT 'once';
                 ALTER TABLE reminders ADD COLUMN weekday_mask INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE reminders ADD COLUMN time_of_day  INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE reminders ADD COLUMN active       INTEGER NOT NULL DEFAULT 1;
                 ALTER TABLE reminders ADD COLUMN base_due_ts  INTEGER;

                 CREATE TABLE IF NOT EXISTS reminder_log (
                     id          INTEGER PRIMARY KEY AUTOINCREMENT,
                     reminder_id INTEGER NOT NULL,
                     fired_ts    INTEGER NOT NULL,
                     ack         TEXT    NOT NULL DEFAULT 'fired',
                     ack_ts      INTEGER,
                     delivery    TEXT    NOT NULL DEFAULT 'ghost'
                 );
                 CREATE INDEX IF NOT EXISTS idx_reminder_log_rid ON reminder_log(reminder_id);

                 INSERT OR REPLACE INTO app_settings (key, value) VALUES ('db_schema_version', '6');",
            )
            .context("migrate to schema v6")?;
        }

        if current < 7 {
            // M8: ToDo・日課管理 (daily-support-design §2.2)。
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS todos (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    text       TEXT    NOT NULL,
                    bucket     TEXT    NOT NULL DEFAULT 'today',
                    priority   INTEGER NOT NULL DEFAULT 0,
                    recurring  TEXT,
                    status     TEXT    NOT NULL DEFAULT 'open',
                    done_ts    INTEGER,
                    created_ts INTEGER NOT NULL,
                    sort_order INTEGER NOT NULL DEFAULT 0
                );
                CREATE INDEX IF NOT EXISTS idx_todos_status ON todos(status, bucket);

                INSERT OR REPLACE INTO app_settings (key, value) VALUES ('db_schema_version', '7');",
            )
            .context("migrate to schema v7")?;
        }

        if current < 8 {
            // M10: カレンダー参照 (daily-support-design §2.3)。
            // 複合キー (source_id, uid, start_ts) で発生インスタンス単位に持つ。
            // unsupported は展開できない RRULE の当日分に印を付ける実装追加列 (§11.1)。
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS calendar_cache (
                    source_id     INTEGER NOT NULL,
                    uid           TEXT    NOT NULL,
                    recurrence_id TEXT,
                    summary       TEXT    NOT NULL,
                    start_ts      INTEGER NOT NULL,
                    end_ts        INTEGER,
                    all_day       INTEGER NOT NULL DEFAULT 0,
                    status        TEXT    NOT NULL DEFAULT 'confirmed',
                    notify_key    TEXT    NOT NULL,
                    notified      INTEGER NOT NULL DEFAULT 0,
                    unsupported   INTEGER NOT NULL DEFAULT 0,
                    fetched_ts    INTEGER NOT NULL,
                    PRIMARY KEY (source_id, uid, start_ts)
                );
                CREATE INDEX IF NOT EXISTS idx_calendar_start ON calendar_cache(start_ts);

                INSERT OR REPLACE INTO app_settings (key, value) VALUES ('db_schema_version', '8');",
            )
            .context("migrate to schema v8")?;
        }

        if current < 9 {
            // M14: advanced 独り言のストック (foundation-design §3.2)。
            // ghost_id: 生成に使ったゴースト。切替後に前ゴーストの人格で喋らないための鍵。
            // topic_fetched_ts: 織り込んだ見出しの**取得時刻** (生成時刻ではない)。
            //   発話時の賞味期限判定 (§4.4.6 の二段失効) に使う。時事ネタ無しの文は NULL。
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS monologue_cache (
                    id               INTEGER PRIMARY KEY AUTOINCREMENT,
                    ghost_id         TEXT    NOT NULL,
                    text             TEXT    NOT NULL,
                    pose             TEXT,
                    topic_fetched_ts INTEGER,
                    created_ts       INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_monologue_cache_ghost ON monologue_cache(ghost_id);

                INSERT OR REPLACE INTO app_settings (key, value) VALUES ('db_schema_version', '9');",
            )
            .context("migrate to schema v9")?;
        }

        tx.commit().context("commit migration tx")?;
        Ok(())
    }

    // ===== chat_log =====

    pub fn append_chat(&self, ts: i64, mode: &str, role: ChatRole, text: &str, pose: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "INSERT INTO chat_log (ts, mode, role, text, pose) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![ts, mode, role.as_str(), text, pose],
        )
        .context("append_chat")?;
        Ok(())
    }

    /// 新しい順 N 件取得。M5-G の get_chat_log で使用。
    pub fn list_recent_chat_log(&self, limit: u32) -> Result<Vec<ChatLogRow>> {
        let conn = self.conn.lock().expect("db poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, ts, mode, role, text, pose FROM chat_log
                 ORDER BY id DESC LIMIT ?1",
            )
            .context("prepare list_recent_chat_log")?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                let role_str: String = row.get(3)?;
                let role = match role_str.as_str() {
                    "user" => ChatRole::User,
                    "main" => ChatRole::Main,
                    "sub" => ChatRole::Sub,
                    other => {
                        return Err(rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            format!("unknown role: {other}").into(),
                        ))
                    }
                };
                Ok(ChatLogRow {
                    id: row.get(0)?,
                    ts: row.get(1)?,
                    mode: row.get(2)?,
                    role,
                    text: row.get(4)?,
                    pose: row.get(5)?,
                })
            })
            .context("query_map list_recent_chat_log")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("row list_recent_chat_log")?);
        }
        Ok(out)
    }

    pub fn clear_chat_log(&self) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute("DELETE FROM chat_log", [])
            .context("clear chat_log")?;
        Ok(())
    }

    // ===== user_profile =====

    pub fn list_profile(&self) -> Result<Vec<ProfileEntry>> {
        let conn = self.conn.lock().expect("db poisoned");
        let mut stmt = conn
            .prepare("SELECT id, content, origin, source_keywords, ts FROM user_profile ORDER BY ts ASC")
            .context("prepare list_profile")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ProfileEntry {
                    id: row.get(0)?,
                    content: row.get(1)?,
                    origin: ProfileOrigin::parse(&row.get::<_, String>(2)?),
                    source_keywords: row.get(3)?,
                    ts: row.get(4)?,
                })
            })
            .context("query_map list_profile")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("row list_profile")?);
        }
        Ok(out)
    }

    pub fn insert_profile(
        &self,
        content: &str,
        origin: ProfileOrigin,
        source_keywords: Option<&str>,
        ts: i64,
    ) -> Result<i64> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "INSERT INTO user_profile (content, origin, source_keywords, ts) VALUES (?1, ?2, ?3, ?4)",
            params![content, origin.as_str(), source_keywords, ts],
        )
        .context("insert_profile")?;
        Ok(conn.last_insert_rowid())
    }

    pub fn delete_profile(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute("DELETE FROM user_profile WHERE id = ?1", params![id])
            .context("delete_profile")?;
        Ok(())
    }

    /// M5-E: user_profile を全件削除。戻り値は削除した行数。
    pub fn clear_user_profile(&self) -> Result<u64> {
        let conn = self.conn.lock().expect("db poisoned");
        let n = conn
            .execute("DELETE FROM user_profile", [])
            .context("clear_user_profile")?;
        Ok(n as u64)
    }

    pub fn count_profile_origin(&self, origin: ProfileOrigin) -> Result<u64> {
        let conn = self.conn.lock().expect("db poisoned");
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM user_profile WHERE origin = ?1",
                params![origin.as_str()],
                |row| row.get(0),
            )
            .context("count_profile_origin")?;
        Ok(n as u64)
    }

    /// origin=auto の中で最も古い `limit` 件を削除する。low モード時の容量管理。
    pub fn prune_oldest_auto(&self, limit: u64) -> Result<u64> {
        if limit == 0 {
            return Ok(0);
        }
        let conn = self.conn.lock().expect("db poisoned");
        let n = conn
            .execute(
                "DELETE FROM user_profile WHERE id IN (
                    SELECT id FROM user_profile WHERE origin = 'auto' ORDER BY ts ASC LIMIT ?1
                )",
                params![limit as i64],
            )
            .context("prune_oldest_auto")?;
        Ok(n as u64)
    }

    // ===== api_usage =====

    pub fn append_api_usage(&self, row: &ApiUsageRow) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "INSERT INTO api_usage (ts, provider, model, prompt_tokens, completion_tokens, cost_usd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                row.ts,
                row.provider,
                row.model,
                row.prompt_tokens,
                row.completion_tokens,
                row.cost_usd,
            ],
        )
        .context("append_api_usage")?;
        Ok(())
    }

    /// M5-E: api_usage の全件を新しい順で取得 (エクスポート用、上限 10000 行)。
    pub fn list_api_usage(&self) -> Result<Vec<ApiUsageRow>> {
        let conn = self.conn.lock().expect("db poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT ts, provider, model, prompt_tokens, completion_tokens, cost_usd
                 FROM api_usage ORDER BY ts DESC LIMIT 10000",
            )
            .context("prepare list_api_usage")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ApiUsageRow {
                    ts: row.get(0)?,
                    provider: row.get(1)?,
                    model: row.get(2)?,
                    prompt_tokens: row.get(3)?,
                    completion_tokens: row.get(4)?,
                    cost_usd: row.get(5)?,
                })
            })
            .context("query_map list_api_usage")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("row list_api_usage")?);
        }
        Ok(out)
    }

    /// 指定 unix 秒以降の合計コスト (USD)。月次集計に使う。
    pub fn sum_cost_since(&self, since_ts: i64) -> Result<f64> {
        let conn = self.conn.lock().expect("db poisoned");
        let total: f64 = conn
            .query_row(
                "SELECT COALESCE(SUM(cost_usd), 0.0) FROM api_usage WHERE ts >= ?1",
                params![since_ts],
                |row| row.get(0),
            )
            .context("sum_cost_since")?;
        Ok(total)
    }

    // ===== reminders (M5-B, M7 拡張) =====

    /// SELECT の共通列。pending は reminder_log の未処理 fired から導出する。
    const REMINDER_COLS: &'static str =
        "id, due_ts, text, created_ts, kind, weekday_mask, time_of_day, active, base_due_ts,
         EXISTS(SELECT 1 FROM reminder_log l WHERE l.reminder_id = reminders.id AND l.ack = 'fired') AS pending";

    fn map_reminder(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReminderRow> {
        let kind_str: String = row.get(4)?;
        Ok(ReminderRow {
            id: row.get(0)?,
            due_ts: row.get(1)?,
            text: row.get(2)?,
            created_ts: row.get(3)?,
            kind: ReminderKind::parse(&kind_str),
            weekday_mask: row.get::<_, i64>(5)? as u8,
            time_of_day: row.get::<_, i64>(6)? as i32,
            active: row.get::<_, i64>(7)? != 0,
            base_due_ts: row.get(8)?,
            pending: row.get::<_, i64>(9)? != 0,
        })
    }

    /// 単発 (once) リマインダーの登録。既存 M5-B 経路 (`add_reminder` コマンド) 互換。
    pub fn insert_reminder(&self, due_ts: i64, text: &str, created_ts: i64) -> Result<i64> {
        self.insert_reminder_ex(due_ts, text, created_ts, ReminderKind::Once, 0, 0)
    }

    /// 繰り返しメタ付きの登録 (M7)。
    pub fn insert_reminder_ex(
        &self,
        due_ts: i64,
        text: &str,
        created_ts: i64,
        kind: ReminderKind,
        weekday_mask: u8,
        time_of_day: i32,
    ) -> Result<i64> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "INSERT INTO reminders (due_ts, text, created_ts, kind, weekday_mask, time_of_day, active)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
            params![due_ts, text, created_ts, kind.as_str(), weekday_mask as i64, time_of_day as i64],
        )
        .context("insert_reminder_ex")?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_reminders(&self, filter: ReminderFilter) -> Result<Vec<ReminderRow>> {
        let conn = self.conn.lock().expect("db poisoned");
        let where_clause = match filter {
            ReminderFilter::Active => "WHERE active = 1 OR pending",
            ReminderFilter::Completed => "WHERE active = 0 AND NOT pending",
            ReminderFilter::All => "",
        };
        let sql = format!(
            "SELECT * FROM (SELECT {} FROM reminders) {} ORDER BY due_ts ASC",
            Self::REMINDER_COLS,
            where_clause
        );
        let mut stmt = conn.prepare(&sql).context("prepare list_reminders")?;
        let rows = stmt
            .query_map([], Self::map_reminder)
            .context("query_map list_reminders")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("row list_reminders")?);
        }
        Ok(out)
    }

    /// `active=1 AND due_ts <= now` のリマインダーを返す (発火対象、M7)。
    pub fn due_active_reminders(&self, now_ts: i64) -> Result<Vec<ReminderRow>> {
        let conn = self.conn.lock().expect("db poisoned");
        let sql = format!(
            "SELECT {} FROM reminders WHERE active = 1 AND due_ts <= ?1 ORDER BY due_ts ASC",
            Self::REMINDER_COLS
        );
        let mut stmt = conn.prepare(&sql).context("prepare due_active_reminders")?;
        let rows = stmt
            .query_map(params![now_ts], Self::map_reminder)
            .context("query_map due_active_reminders")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("row due_active_reminders")?);
        }
        Ok(out)
    }

    pub fn get_reminder(&self, id: i64) -> Result<Option<ReminderRow>> {
        let conn = self.conn.lock().expect("db poisoned");
        let sql = format!("SELECT {} FROM reminders WHERE id = ?1", Self::REMINDER_COLS);
        let row = conn.query_row(&sql, params![id], Self::map_reminder).optional()
            .context("get_reminder")?;
        Ok(row)
    }

    pub fn delete_reminder(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute("DELETE FROM reminders WHERE id = ?1", params![id])
            .context("delete_reminder")?;
        conn.execute("DELETE FROM reminder_log WHERE reminder_id = ?1", params![id])
            .context("delete_reminder_log")?;
        Ok(())
    }

    /// 本文・時刻の部分更新 (M7 の update_reminder コマンド)。None の項目は変更しない。
    pub fn update_reminder(&self, id: i64, text: Option<&str>, due_ts: Option<i64>) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "UPDATE reminders SET
                text   = COALESCE(?2, text),
                due_ts = COALESCE(?3, due_ts)
             WHERE id = ?1",
            params![id, text, due_ts],
        )
        .context("update_reminder")?;
        Ok(())
    }

    /// 繰り返しリマインダーの次回発火予定を設定する (M7)。
    /// スヌーズ由来の base_due_ts は次周期でリセットする。
    pub fn reschedule_reminder(&self, id: i64, next_due_ts: i64) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "UPDATE reminders SET due_ts = ?2, base_due_ts = NULL WHERE id = ?1",
            params![id, next_due_ts],
        )
        .context("reschedule_reminder")?;
        Ok(())
    }

    /// スヌーズ (M7): due を new_due に延ばし、本来時刻を base_due_ts に保持する。
    /// 多重スヌーズでは最初の本来時刻を保つ (COALESCE)。
    pub fn snooze_reminder(&self, id: i64, base_due_ts: i64, new_due_ts: i64) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "UPDATE reminders SET due_ts = ?3, base_due_ts = COALESCE(base_due_ts, ?2), active = 1
             WHERE id = ?1",
            params![id, base_due_ts, new_due_ts],
        )
        .context("snooze_reminder")?;
        Ok(())
    }

    /// once の再発火停止 (M7)。「完了」ではない (未完了のまま止まる、§2.1)。
    pub fn deactivate_reminder(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute("UPDATE reminders SET active = 0 WHERE id = ?1", params![id])
            .context("deactivate_reminder")?;
        Ok(())
    }

    // ===== reminder_log (M7) =====

    /// 発火 (配達到達) の履歴を 1 行記録する。delivery は DeliveryOutcome の小文字表記。
    pub fn log_fire(&self, reminder_id: i64, fired_ts: i64, delivery: &str) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "INSERT INTO reminder_log (reminder_id, fired_ts, ack, delivery) VALUES (?1, ?2, 'fired', ?3)",
            params![reminder_id, fired_ts, delivery],
        )
        .context("log_fire")?;
        Ok(())
    }

    /// 当該リマインダーの最新の未処理 fired ログへ ack を付ける (M7)。
    /// 対象が無ければ false (未発火のまま完了/無視された等)。
    pub fn set_ack(&self, reminder_id: i64, ack: &str, ack_ts: i64) -> Result<bool> {
        let conn = self.conn.lock().expect("db poisoned");
        let n = conn
            .execute(
                "UPDATE reminder_log SET ack = ?2, ack_ts = ?3 WHERE id = (
                    SELECT id FROM reminder_log
                    WHERE reminder_id = ?1 AND ack = 'fired'
                    ORDER BY id DESC LIMIT 1
                )",
                params![reminder_id, ack, ack_ts],
            )
            .context("set_ack")?;
        Ok(n > 0)
    }

    /// 通知履歴 (新しい順、最大 limit 件)。
    pub fn list_reminder_log(&self, reminder_id: i64, limit: u32) -> Result<Vec<ReminderLogRow>> {
        let conn = self.conn.lock().expect("db poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, reminder_id, fired_ts, ack, ack_ts, delivery FROM reminder_log
                 WHERE reminder_id = ?1 ORDER BY id DESC LIMIT ?2",
            )
            .context("prepare list_reminder_log")?;
        let rows = stmt
            .query_map(params![reminder_id, limit as i64], |row| {
                Ok(ReminderLogRow {
                    id: row.get(0)?,
                    reminder_id: row.get(1)?,
                    fired_ts: row.get(2)?,
                    ack: row.get(3)?,
                    ack_ts: row.get(4)?,
                    delivery: row.get(5)?,
                })
            })
            .context("query_map list_reminder_log")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("row list_reminder_log")?);
        }
        Ok(out)
    }

    /// reminder_log を新しい順 keep 件だけ残して古い順に削除する (M7、既定 500)。
    pub fn prune_reminder_log(&self, keep: u32) -> Result<u64> {
        let conn = self.conn.lock().expect("db poisoned");
        let n = conn
            .execute(
                "DELETE FROM reminder_log WHERE id NOT IN (
                    SELECT id FROM reminder_log ORDER BY id DESC LIMIT ?1
                )",
                params![keep as i64],
            )
            .context("prune_reminder_log")?;
        Ok(n as u64)
    }

    // ===== todos (M8) =====

    fn map_todo(row: &rusqlite::Row<'_>) -> rusqlite::Result<TodoRow> {
        Ok(TodoRow {
            id: row.get(0)?,
            text: row.get(1)?,
            bucket: row.get(2)?,
            priority: row.get::<_, i64>(3)? as i32,
            recurring: row.get(4)?,
            status: row.get(5)?,
            done_ts: row.get(6)?,
            created_ts: row.get(7)?,
            sort_order: row.get(8)?,
        })
    }

    const TODO_COLS: &'static str =
        "id, text, bucket, priority, recurring, status, done_ts, created_ts, sort_order";

    /// 追加。sort_order は同 bucket の末尾 (max+1)。値の検証は tools::todo が行う。
    pub fn insert_todo(
        &self,
        text: &str,
        bucket: &str,
        priority: i32,
        recurring: Option<&str>,
        created_ts: i64,
    ) -> Result<i64> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "INSERT INTO todos (text, bucket, priority, recurring, created_ts, sort_order)
             VALUES (?1, ?2, ?3, ?4, ?5,
                     (SELECT COALESCE(MAX(sort_order), 0) + 1 FROM todos WHERE bucket = ?2))",
            params![text, bucket, priority as i64, recurring, created_ts],
        )
        .context("insert_todo")?;
        Ok(conn.last_insert_rowid())
    }

    /// 一覧。bucket 指定で絞り込み (None は全件)。
    /// open が先・優先度高が先・sort_order 昇順 (パネルの表示順)。
    pub fn list_todos(&self, bucket: Option<&str>) -> Result<Vec<TodoRow>> {
        let conn = self.conn.lock().expect("db poisoned");
        let sql = format!(
            "SELECT {} FROM todos {} ORDER BY
                 CASE status WHEN 'open' THEN 0 ELSE 1 END,
                 priority DESC, sort_order ASC, id ASC",
            Self::TODO_COLS,
            if bucket.is_some() { "WHERE bucket = ?1" } else { "" }
        );
        let mut stmt = conn.prepare(&sql).context("prepare list_todos")?;
        let mut out = Vec::new();
        match bucket {
            Some(b) => {
                let rows = stmt
                    .query_map(params![b], Self::map_todo)
                    .context("query list_todos")?;
                for r in rows {
                    out.push(r.context("row list_todos")?);
                }
            }
            None => {
                let rows = stmt
                    .query_map([], Self::map_todo)
                    .context("query list_todos")?;
                for r in rows {
                    out.push(r.context("row list_todos")?);
                }
            }
        }
        Ok(out)
    }

    pub fn get_todo(&self, id: i64) -> Result<Option<TodoRow>> {
        let conn = self.conn.lock().expect("db poisoned");
        let sql = format!("SELECT {} FROM todos WHERE id = ?1", Self::TODO_COLS);
        let row = conn.query_row(&sql, params![id], Self::map_todo).optional()
            .context("get_todo")?;
        Ok(row)
    }

    /// status の変更。'done' なら done_ts を刻み、'open' なら done_ts をクリアする。
    pub fn set_todo_status(&self, id: i64, status: &str, now_ts: i64) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        let done_ts: Option<i64> = if status == "done" { Some(now_ts) } else { None };
        conn.execute(
            "UPDATE todos SET status = ?2, done_ts = ?3 WHERE id = ?1",
            params![id, status, done_ts],
        )
        .context("set_todo_status")?;
        Ok(())
    }

    pub fn delete_todo(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute("DELETE FROM todos WHERE id = ?1", params![id])
            .context("delete_todo")?;
        Ok(())
    }

    /// 部分更新 (M8 の update_todo コマンド)。None の項目は変更しない。
    /// recurring だけは「変更しない/クリア/設定」の三値が要るため二重 Option。
    pub fn update_todo(
        &self,
        id: i64,
        text: Option<&str>,
        bucket: Option<&str>,
        priority: Option<i32>,
        recurring: Option<Option<&str>>,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "UPDATE todos SET
                text     = COALESCE(?2, text),
                bucket   = COALESCE(?3, bucket),
                priority = COALESCE(?4, priority)
             WHERE id = ?1",
            params![id, text, bucket, priority.map(|p| p as i64)],
        )
        .context("update_todo")?;
        if let Some(rec) = recurring {
            conn.execute(
                "UPDATE todos SET recurring = ?2 WHERE id = ?1",
                params![id, rec],
            )
            .context("update_todo recurring")?;
        }
        Ok(())
    }

    /// 日課の復活 (M8, daily-support-design §2.2)。
    /// daily: 今日のローカル 0:00 (UTC 秒) より前に done → open へ。
    /// weekly: 今週月曜のローカル 0:00 より前に done → open へ。
    /// cutoff の計算はローカル TZ 契約 (§2.5) に従い呼び出し側 (tools::todo) が行う。
    pub fn reset_recurring_todos(&self, daily_cutoff_ts: i64, weekly_cutoff_ts: i64) -> Result<u64> {
        let conn = self.conn.lock().expect("db poisoned");
        let mut n = conn
            .execute(
                "UPDATE todos SET status = 'open', done_ts = NULL
                 WHERE status = 'done' AND recurring = 'daily'
                   AND (done_ts IS NULL OR done_ts < ?1)",
                params![daily_cutoff_ts],
            )
            .context("reset daily todos")?;
        n += conn
            .execute(
                "UPDATE todos SET status = 'open', done_ts = NULL
                 WHERE status = 'done' AND recurring = 'weekly'
                   AND (done_ts IS NULL OR done_ts < ?1)",
                params![weekly_cutoff_ts],
            )
            .context("reset weekly todos")?;
        Ok(n as u64)
    }

    // ===== calendar_cache (M10) =====

    fn map_calendar(row: &rusqlite::Row<'_>) -> rusqlite::Result<CalendarCacheRow> {
        Ok(CalendarCacheRow {
            source_id: row.get(0)?,
            uid: row.get(1)?,
            recurrence_id: row.get(2)?,
            summary: row.get(3)?,
            start_ts: row.get(4)?,
            end_ts: row.get(5)?,
            all_day: row.get::<_, i64>(6)? != 0,
            status: row.get(7)?,
            unsupported: row.get::<_, i64>(8)? != 0,
            notified: row.get::<_, i64>(9)? != 0,
        })
    }

    const CALENDAR_COLS: &'static str =
        "source_id, uid, recurrence_id, summary, start_ts, end_ts, all_day, status, unsupported, notified";

    /// 発生インスタンスを 1 件 UPSERT する (M10)。notify_key 差分規則 (§2.3):
    /// 既存行と notify_key が一致すれば notified を保持、変われば 0 にリセットする。
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_calendar_event(
        &self,
        source_id: i64,
        uid: &str,
        recurrence_id: Option<&str>,
        summary: &str,
        start_ts: i64,
        end_ts: Option<i64>,
        all_day: bool,
        status: &str,
        notify_key: &str,
        unsupported: bool,
        fetched_ts: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "INSERT INTO calendar_cache
                (source_id, uid, recurrence_id, summary, start_ts, end_ts, all_day, status,
                 notify_key, notified, unsupported, fetched_ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?11)
             ON CONFLICT(source_id, uid, start_ts) DO UPDATE SET
                recurrence_id = excluded.recurrence_id,
                summary       = excluded.summary,
                end_ts        = excluded.end_ts,
                all_day       = excluded.all_day,
                status        = excluded.status,
                unsupported   = excluded.unsupported,
                fetched_ts    = excluded.fetched_ts,
                notified = CASE WHEN calendar_cache.notify_key = excluded.notify_key
                                THEN calendar_cache.notified ELSE 0 END,
                notify_key = excluded.notify_key",
            params![
                source_id, uid, recurrence_id, summary, start_ts, end_ts, all_day as i64,
                status, notify_key, unsupported as i64, fetched_ts
            ],
        )
        .context("upsert_calendar_event")?;
        Ok(())
    }

    /// 取得時刻より古い (=今回の再取得で消えた) 発生行を削除する (M10)。
    /// notified 状態は生き残った行では保持される (fetched_ts を更新するため)。
    pub fn delete_stale_calendar(&self, source_id: i64, fetch_ts: i64) -> Result<u64> {
        let conn = self.conn.lock().expect("db poisoned");
        let n = conn
            .execute(
                "DELETE FROM calendar_cache WHERE source_id = ?1 AND fetched_ts < ?2",
                params![source_id, fetch_ts],
            )
            .context("delete_stale_calendar")?;
        Ok(n as u64)
    }

    /// 表示窓 [from, to) の confirmed な予定を開始順で返す (M10)。
    pub fn list_calendar(&self, from_ts: i64, to_ts: i64) -> Result<Vec<CalendarCacheRow>> {
        let conn = self.conn.lock().expect("db poisoned");
        let sql = format!(
            "SELECT {} FROM calendar_cache
             WHERE status = 'confirmed' AND start_ts >= ?1 AND start_ts < ?2
             ORDER BY start_ts ASC",
            Self::CALENDAR_COLS
        );
        let mut stmt = conn.prepare(&sql).context("prepare list_calendar")?;
        let rows = stmt
            .query_map(params![from_ts, to_ts], Self::map_calendar)
            .context("query list_calendar")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("row list_calendar")?);
        }
        Ok(out)
    }

    /// 開始前通知の候補 (未通知・confirmed) を返す (M10)。
    /// notify_at は終日と時刻付きで異なるため呼び出し側 (watcher) が計算し、
    /// ここでは候補を広めに返して watcher が絞る:
    /// - 時刻付き: まだ始まっていない (start_ts > now)
    /// - 終日: 当日中 (start_ts + 86400 > now)。通知時刻 (当日ローカル 8:00) は
    ///   start_ts (0:00) より後なので「開始前」条件だと一度も候補にならない
    ///   (M10 reviewer 指摘の修正)
    pub fn upcoming_calendar(&self, now_ts: i64, horizon_ts: i64) -> Result<Vec<CalendarCacheRow>> {
        let conn = self.conn.lock().expect("db poisoned");
        let sql = format!(
            "SELECT {} FROM calendar_cache
             WHERE status = 'confirmed' AND notified = 0
               AND ((all_day = 0 AND start_ts > ?1) OR (all_day = 1 AND start_ts + 86400 > ?1))
               AND start_ts <= ?2
             ORDER BY start_ts ASC",
            Self::CALENDAR_COLS
        );
        let mut stmt = conn.prepare(&sql).context("prepare upcoming_calendar")?;
        let rows = stmt
            .query_map(params![now_ts, horizon_ts], Self::map_calendar)
            .context("query upcoming_calendar")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("row upcoming_calendar")?);
        }
        Ok(out)
    }

    pub fn mark_calendar_notified(&self, source_id: i64, uid: &str, start_ts: i64) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "UPDATE calendar_cache SET notified = 1
             WHERE source_id = ?1 AND uid = ?2 AND start_ts = ?3",
            params![source_id, uid, start_ts],
        )
        .context("mark_calendar_notified")?;
        Ok(())
    }

    /// 過去の発生行を prune する (end_ts か start_ts が cutoff 未満、§2.3)。
    pub fn prune_calendar(&self, cutoff_ts: i64) -> Result<u64> {
        let conn = self.conn.lock().expect("db poisoned");
        let n = conn
            .execute(
                "DELETE FROM calendar_cache WHERE COALESCE(end_ts, start_ts) < ?1",
                params![cutoff_ts],
            )
            .context("prune_calendar")?;
        Ok(n as u64)
    }

    /// カレンダーキャッシュを全消去する (M10)。ソース構成の変更時に呼ぶ
    /// (index ベースの source_id がずれるため全 clear して再取得する、§11.1)。
    pub fn clear_calendar(&self) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute("DELETE FROM calendar_cache", [])
            .context("clear_calendar")?;
        Ok(())
    }

    /// 未完了件数 (朝の告知用)。bucket 指定で絞り込み (None は全 bucket)。
    pub fn count_open_todos(&self, bucket: Option<&str>) -> Result<u64> {
        let conn = self.conn.lock().expect("db poisoned");
        let n: i64 = match bucket {
            Some(b) => conn
                .query_row(
                    "SELECT COUNT(*) FROM todos WHERE status = 'open' AND bucket = ?1",
                    params![b],
                    |row| row.get(0),
                )
                .context("count_open_todos")?,
            None => conn
                .query_row(
                    "SELECT COUNT(*) FROM todos WHERE status = 'open'",
                    [],
                    |row| row.get(0),
                )
                .context("count_open_todos")?,
        };
        Ok(n as u64)
    }

    /// 指定 unix 秒以降に完了した件数 (夜の定例会話の完了実績用、M12、
    /// regular-talk-design §5.3/§10.4)。
    pub fn count_done_todos_since(&self, since_ts: i64) -> Result<u64> {
        let conn = self.conn.lock().expect("db poisoned");
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM todos WHERE status = 'done' AND done_ts >= ?1",
                params![since_ts],
                |row| row.get(0),
            )
            .context("count_done_todos_since")?;
        Ok(n as u64)
    }

    // ===== interest_topics (M5-C) =====

    /// 全件取得 (id 昇順)。
    pub fn list_interests(&self) -> Result<Vec<InterestTopic>> {
        let conn = self.conn.lock().expect("db poisoned");
        let mut stmt = conn
            .prepare("SELECT id, topic, enabled FROM interest_topics ORDER BY id ASC")
            .context("prepare list_interests")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(InterestTopic {
                    id: row.get(0)?,
                    topic: row.get(1)?,
                    enabled: row.get::<_, i64>(2)? != 0,
                })
            })
            .context("query_map list_interests")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("row list_interests")?);
        }
        Ok(out)
    }

    /// 一括置換: 引数の topic 文字列リストに揃える (既存はクリアして再作成)。
    /// enabled は常に true で挿入。空文字列とトリム後の重複は除外する。
    pub fn replace_interests(&self, topics: &[String]) -> Result<Vec<InterestTopic>> {
        {
            let mut guard = self.conn.lock().expect("db poisoned");
            let tx = guard.transaction().context("begin replace_interests")?;
            tx.execute("DELETE FROM interest_topics", [])
                .context("delete interest_topics")?;
            let mut seen = std::collections::HashSet::<String>::new();
            for t in topics {
                let trimmed = t.trim();
                if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
                    continue;
                }
                tx.execute(
                    "INSERT INTO interest_topics (topic, enabled) VALUES (?1, 1)",
                    params![trimmed],
                )
                .context("insert interest_topics")?;
            }
            tx.commit().context("commit replace_interests")?;
            // guard はここで drop される。次の self.list_interests() が再 lock するため、
            // std::sync::Mutex (non-reentrant) で自己デッドロックさせないこと。
        }
        self.list_interests()
    }

    /// 有効な topic 文字列のみ返す。
    pub fn list_enabled_topics(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().expect("db poisoned");
        let mut stmt = conn
            .prepare("SELECT topic FROM interest_topics WHERE enabled = 1 ORDER BY id ASC")
            .context("prepare list_enabled_topics")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .context("query_map list_enabled_topics")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("row list_enabled_topics")?);
        }
        Ok(out)
    }

    // ===== topics_cache (M5-C) =====

    /// 1 件追加。UNIQUE(topic, headline) で重複なら no-op。
    pub fn insert_topic_cache(
        &self,
        topic: &str,
        headline: &str,
        link: &str,
        ts: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "INSERT OR IGNORE INTO topics_cache (topic, headline, link, fetched_ts)
             VALUES (?1, ?2, ?3, ?4)",
            params![topic, headline, link, ts],
        )
        .context("insert_topic_cache")?;
        Ok(())
    }

    /// 最新 N 件取得 (新しい順)。advanced 独り言混入 (将来課題) で参照する想定。
    #[allow(dead_code)]
    pub fn list_recent_topics(&self, limit: u32) -> Result<Vec<TopicCacheRow>> {
        let conn = self.conn.lock().expect("db poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, topic, headline, link, fetched_ts FROM topics_cache
                 ORDER BY fetched_ts DESC LIMIT ?1",
            )
            .context("prepare list_recent_topics")?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(TopicCacheRow {
                    id: row.get(0)?,
                    topic: row.get(1)?,
                    headline: row.get(2)?,
                    link: row.get(3)?,
                    fetched_ts: row.get(4)?,
                })
            })
            .context("query_map list_recent_topics")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("row list_recent_topics")?);
        }
        Ok(out)
    }

    // ===== monologue_cache (M14, foundation-design §3.2) =====

    /// advanced 独り言のストックを 1 件積む (補充経路 `system::monologue` 専用)。
    pub fn push_monologue_cache(
        &self,
        ghost_id: &str,
        text: &str,
        pose: Option<&str>,
        topic_fetched_ts: Option<i64>,
        created_ts: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "INSERT INTO monologue_cache (ghost_id, text, pose, topic_fetched_ts, created_ts)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![ghost_id, text, pose, topic_fetched_ts, created_ts],
        )
        .context("push_monologue_cache")?;
        Ok(())
    }

    /// ストックから 1 件取り出して削除する。**取り出しと削除を 1 トランザクション**で行い、
    /// 同じ tx で使えない行をまとめて捨てる (foundation-design §3.2 / §3.4):
    ///
    /// - `ghost_id` が現在のゴーストと違う行 — 人格も pose もシェル依存で、切替後は使えない
    /// - 生成から `MONOLOGUE_MAX_AGE_SECS` (30 日) を超えた行
    /// - 時事ネタ入りで、見出しの取得から `TOPIC_MAX_AGE_SECS` (7 日) を超えた行 (**発話時失効**)
    ///
    /// 残った中から最古 (FIFO) を 1 件返す。空なら `None` — 呼び出し側は辞書の独り言へ落ちる。
    pub fn pop_monologue_cache(
        &self,
        ghost_id: &str,
        now: i64,
    ) -> Result<Option<MonologueCacheRow>> {
        let mut guard = self.conn.lock().expect("db poisoned");
        let tx = guard.transaction().context("begin pop_monologue_cache tx")?;
        tx.execute(
            "DELETE FROM monologue_cache
             WHERE ghost_id <> ?1
                OR created_ts < ?2
                OR (topic_fetched_ts IS NOT NULL AND topic_fetched_ts < ?3)",
            params![
                ghost_id,
                now - MONOLOGUE_MAX_AGE_SECS,
                now - TOPIC_MAX_AGE_SECS
            ],
        )
        .context("prune stale monologue_cache rows")?;
        let picked = tx
            .query_row(
                "SELECT id, text, pose, topic_fetched_ts FROM monologue_cache
                 ORDER BY id ASC LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        MonologueCacheRow {
                            text: row.get(1)?,
                            pose: row.get(2)?,
                            topic_fetched_ts: row.get(3)?,
                        },
                    ))
                },
            )
            .optional()
            .context("select oldest monologue_cache row")?;
        if let Some((id, _)) = &picked {
            tx.execute("DELETE FROM monologue_cache WHERE id = ?1", params![id])
                .context("delete popped monologue_cache row")?;
        }
        tx.commit().context("commit pop_monologue_cache tx")?;
        Ok(picked.map(|(_, row)| row))
    }

    /// **いま使える**ストック件数 (補充要否の判定用)。
    /// `pop_monologue_cache` と同じ条件で数える — 失効済み・別ゴーストの行を頭数に入れると、
    /// pop が全部捨てて辞書に落ちているのに「在庫あり」と見えて補充が止まるため。
    pub fn count_monologue_cache(&self, ghost_id: &str, now: i64) -> Result<u64> {
        let conn = self.conn.lock().expect("db poisoned");
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM monologue_cache
                 WHERE ghost_id = ?1
                   AND created_ts >= ?2
                   AND (topic_fetched_ts IS NULL OR topic_fetched_ts >= ?3)",
                params![
                    ghost_id,
                    now - MONOLOGUE_MAX_AGE_SECS,
                    now - TOPIC_MAX_AGE_SECS
                ],
                |row| row.get(0),
            )
            .context("count_monologue_cache")?;
        Ok(n.max(0) as u64)
    }

    /// ストックを全消去 (履歴クリア §4.5.5。`include_profile` に依らず常に消す)。
    pub fn clear_monologue_cache(&self) -> Result<u64> {
        let conn = self.conn.lock().expect("db poisoned");
        let n = conn
            .execute("DELETE FROM monologue_cache", [])
            .context("clear_monologue_cache")?;
        Ok(n as u64)
    }

    /// 時事ネタを織り込んだ行だけ消去 (時事ネタ同意の撤回時、foundation-design §3.6)。
    /// 同意を外したのに時事ネタ入りの文を喋り続けないための掃除。
    pub fn clear_monologue_cache_with_topics(&self) -> Result<u64> {
        let conn = self.conn.lock().expect("db poisoned");
        let n = conn
            .execute(
                "DELETE FROM monologue_cache WHERE topic_fetched_ts IS NOT NULL",
                [],
            )
            .context("clear_monologue_cache_with_topics")?;
        Ok(n as u64)
    }

    /// 古い topics_cache を削除 (since_ts 未満)。
    pub fn prune_topics_cache(&self, since_ts: i64) -> Result<u64> {
        let conn = self.conn.lock().expect("db poisoned");
        let n = conn
            .execute(
                "DELETE FROM topics_cache WHERE fetched_ts < ?1",
                params![since_ts],
            )
            .context("prune_topics_cache")?;
        Ok(n as u64)
    }

    // ===== voice_refs (M4c) =====

    /// `slot` ("main" | "sub") に紐づく参照音声を 1 件 upsert する。
    /// 既存行があれば file_path / caption / created_ts を上書きし、id は維持される。
    /// Phase D の `voice_ref_generate` コマンドが呼ぶ。
    #[allow(dead_code)]
    pub fn upsert_voice_ref(
        &self,
        slot: &str,
        caption: &str,
        file_path: &str,
        ts: i64,
    ) -> Result<i64> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "INSERT INTO voice_refs (slot, caption, file_path, created_ts)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(slot) DO UPDATE SET
                 caption    = excluded.caption,
                 file_path  = excluded.file_path,
                 created_ts = excluded.created_ts",
            params![slot, caption, file_path, ts],
        )
        .context("upsert_voice_ref")?;
        let id: i64 = conn
            .query_row(
                "SELECT id FROM voice_refs WHERE slot = ?1",
                params![slot],
                |row| row.get(0),
            )
            .context("read voice_refs.id after upsert")?;
        Ok(id)
    }

    pub fn get_voice_ref(&self, slot: &str) -> Result<Option<VoiceRefRow>> {
        let conn = self.conn.lock().expect("db poisoned");
        let row = conn
            .query_row(
                "SELECT id, slot, caption, file_path, created_ts FROM voice_refs WHERE slot = ?1",
                params![slot],
                |row| {
                    Ok(VoiceRefRow {
                        id: row.get(0)?,
                        slot: row.get(1)?,
                        caption: row.get(2)?,
                        file_path: row.get(3)?,
                        created_ts: row.get(4)?,
                    })
                },
            )
            .optional()
            .context("get_voice_ref")?;
        Ok(row)
    }

    /// `app_settings` の全キー。エクスポート（データの持ち出し・DB 退避）専用。
    ///
    /// v0.4.1 まで export は 12 テーブル中 3 つしか出しておらず、
    /// **リマインダー・ToDo・設定が持ち出せなかった**。DB を退避・作り直す機能を
    /// 入れる前提として全件出せるようにする。
    pub fn list_all_settings(&self) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().expect("db poisoned");
        let mut stmt = conn
            .prepare("SELECT key, value FROM app_settings ORDER BY key")
            .context("list_all_settings prepare")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .context("list_all_settings query")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("list_all_settings row")?);
        }
        Ok(out)
    }

    /// `reminder_log` の全件（新しい順、上限つき）。エクスポート専用。
    /// 既存の `list_reminder_log` は 1 件のリマインダーに紐づく分しか返さない。
    pub fn list_all_reminder_log(&self, limit: u32) -> Result<Vec<ReminderLogRow>> {
        let conn = self.conn.lock().expect("db poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, reminder_id, fired_ts, ack, ack_ts, delivery FROM reminder_log ORDER BY id DESC LIMIT ?1",
            )
            .context("list_all_reminder_log prepare")?;
        let rows = stmt
            .query_map([limit], |r| {
                Ok(ReminderLogRow {
                    id: r.get(0)?,
                    reminder_id: r.get(1)?,
                    fired_ts: r.get(2)?,
                    ack: r.get(3)?,
                    ack_ts: r.get(4)?,
                    delivery: r.get(5)?,
                })
            })
            .context("list_all_reminder_log query")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("list_all_reminder_log row")?);
        }
        Ok(out)
    }

    pub fn list_voice_refs(&self) -> Result<Vec<VoiceRefRow>> {
        let conn = self.conn.lock().expect("db poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, slot, caption, file_path, created_ts FROM voice_refs ORDER BY slot ASC",
            )
            .context("prepare list_voice_refs")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(VoiceRefRow {
                    id: row.get(0)?,
                    slot: row.get(1)?,
                    caption: row.get(2)?,
                    file_path: row.get(3)?,
                    created_ts: row.get(4)?,
                })
            })
            .context("query_map list_voice_refs")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("row list_voice_refs")?);
        }
        Ok(out)
    }

    pub fn delete_voice_ref(&self, slot: &str) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute("DELETE FROM voice_refs WHERE slot = ?1", params![slot])
            .context("delete_voice_ref")?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("db poisoned");
        let value = conn
            .query_row(
                "SELECT value FROM app_settings WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("get_setting")?;
        Ok(value)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .with_context(|| format!("set_setting('{key}') 失敗"))?;
        Ok(())
    }
}

#[cfg(test)]
mod integrity_tests {
    use super::*;

    /// 健全な DB では ok=true、退避ファイルを作らないこと。
    /// （正常時に毎起動コピーを撒き散らすと、それ自体が容量問題になる）
    #[test]
    fn healthy_db_reports_ok_and_leaves_no_copies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("companion.db");
        let db = Db::open(&path).unwrap();
        db.migrate().unwrap();
        let h = db.integrity();
        assert!(h.ok, "健全な DB が ok でない: {}", h.detail);
        assert_eq!(h.detail, "ok");
        assert!(h.backup_path.is_none(), "正常時に退避を作ってはいけない");
        assert!(h.salvaged_path.is_none(), "正常時に救出コピーを作ってはいけない");
        // ディレクトリに余計なファイルが増えていないこと
        let extra: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("corrupt-") || n.contains("salvaged-"))
            .collect();
        assert!(extra.is_empty(), "余計なファイル: {extra:?}");
    }

    /// **破損しても DB を作り直さない**こと（既存データが消えない）。
    ///
    /// v0.5.1 の設計判断。破損を理由に消すとリマインダー・ToDo・記憶が一緒に消える。
    #[test]
    fn open_never_recreates_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("companion.db");
        {
            let db = Db::open(&path).unwrap();
            db.migrate().unwrap();
            db.set_setting("marker", "keep-me").unwrap();
        }
        // 開き直しても中身が残っていること。
        let db = Db::open(&path).unwrap();
        assert_eq!(
            db.get_setting("marker").unwrap().as_deref(),
            Some("keep-me"),
            "再オープンで既存データが失われた"
        );
    }

    /// 破損した DB を作る。ヘッダ (1 ページ目) は壊さず、データが載っている
    /// 後続ページを潰す。こうしないと `Connection::open` 自体が失敗して
    /// 「破損したまま起動を続ける」という検証対象の経路に入れない。
    fn make_corrupt_db(path: &std::path::Path) {
        {
            let db = Db::open(path).unwrap();
            db.migrate().unwrap();
            // 複数ページに跨る量を書く（1 ページ目だけだと潰す先が無い）。
            for i in 0..400 {
                db.set_setting(&format!("k{i}"), &"v".repeat(200)).unwrap();
            }
        }
        use std::io::{Seek, SeekFrom, Write};
        let mut f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        let len = f.metadata().unwrap().len();
        assert!(len > 4096 * 4, "テスト用 DB が小さすぎる: {len}");
        f.seek(SeekFrom::Start(4096 * 2)).unwrap();
        f.write_all(&[0xA5u8; 4096 * 2]).unwrap();
        f.flush().unwrap();
    }

    /// **破損を検知しても DB は作り直さない。** テスト名が主張している内容を
    /// 実際に破損経路で確かめる（v0.5.1 のリリース前監査で、破損を一度も
    /// 作らないまま「作り直さない」を名乗っていたのを是正）。
    #[test]
    fn corrupt_db_is_detected_preserved_and_never_recreated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("companion.db");
        make_corrupt_db(&path);
        let before = std::fs::metadata(&path).unwrap().len();

        let db = Db::open(&path).unwrap();
        let h = db.integrity().clone();
        assert!(!h.ok, "破損を検知できていない: {}", h.detail);
        assert_ne!(h.detail, "ok");
        let backup = h.backup_path.clone().expect("原本の退避が作られていない");
        assert!(std::path::Path::new(&backup).is_file());

        // 原本は消されても作り直されてもいない。
        assert!(path.is_file(), "原本が消えた");
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            before,
            "原本が作り直された（サイズが変わった）"
        );
    }

    /// **保全は破損 1 件につき 1 回だけ。** 実ユーザーの DB は既に破損しており、
    /// 毎起動コピーすると起動のたびにディスクを食い潰す。
    #[test]
    fn corrupt_db_is_preserved_only_once_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("companion.db");
        make_corrupt_db(&path);

        let count = || {
            std::fs::read_dir(dir.path())
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.contains(".corrupt-") || n.contains(".salvaged-")
                })
                .count()
        };

        // **既存の退避を先に置く。** 同一プロセス内で連続オープンしても
        // タイムスタンプが秒単位で同じになり、guard が無くても同じファイル名を
        // 上書きするだけで件数が増えない = 素通りする（実際に変異テストで
        // 素通りした）。日をまたいだ再起動を再現するため、古い世代を先に置く。
        let old_backup = dir.path().join("companion.corrupt-20200101-000000.db");
        let old_salvaged = dir.path().join("companion.salvaged-20200101-000000.db");
        std::fs::write(&old_backup, b"older-generation").unwrap();
        std::fs::write(&old_salvaged, b"older-generation").unwrap();
        assert_eq!(count(), 2);

        for _ in 0..3 {
            let h = Db::open(&path).unwrap().integrity().clone();
            assert!(!h.ok, "破損を検知できていない");
            assert_eq!(
                h.backup_path.as_deref(),
                Some(old_backup.to_string_lossy().as_ref()),
                "既存の退避があるのに新しく作っている"
            );
            assert_eq!(
                h.salvaged_path.as_deref(),
                Some(old_salvaged.to_string_lossy().as_ref()),
                "既存の救出コピーがあるのに作り直している"
            );
            assert_eq!(count(), 2, "再起動のたびに退避が増えている");
        }
        // 先に作られた世代のほうが原本に近いので、上書きもしない。
        assert_eq!(std::fs::read(&old_backup).unwrap(), b"older-generation");
    }

    /// **退避は WAL サイドカーも運ぶ。** 実機では -wal が本体より大きいことがあり、
    /// 本体だけコピーすると直近のコミット済みデータが退避から丸ごと抜ける。
    #[test]
    fn corrupt_db_backup_includes_wal_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("companion.db");
        make_corrupt_db(&path);
        // クリーンに閉じた後なので -wal は残っていない。中身の分かる印を置く。
        let wal = dir.path().join("companion.db-wal");
        std::fs::write(&wal, b"sentinel-wal").unwrap();

        let h = Db::open(&path).unwrap().integrity().clone();
        let backup = h.backup_path.expect("原本の退避が作られていない");
        let backup_wal = format!("{backup}-wal");
        assert!(
            std::path::Path::new(&backup_wal).is_file(),
            "WAL が退避されていない: {backup_wal}"
        );
        assert_eq!(std::fs::read(&backup_wal).unwrap(), b"sentinel-wal");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn make_db() -> Db {
        let file = NamedTempFile::new().expect("temp db file");
        let path = file.path().to_path_buf();
        // NamedTempFile はスコープ終了で消えるが、open は内部で別 fd を握るので順番上問題ない。
        drop(file);
        let db = Db::open(&path).expect("open db");
        db.migrate().expect("migrate");
        db
    }

    // ===== monologue_cache (M14, foundation-design §3.2/§3.4) =====

    const NOW: i64 = 1_800_000_000;

    /// 素の 1 件 (時事ネタ無し・生成時刻 = now) を積む。
    fn push_plain(db: &Db, ghost: &str, text: &str) {
        db.push_monologue_cache(ghost, text, None, None, NOW)
            .expect("push");
    }

    #[test]
    fn monologue_cache_pops_oldest_first_and_removes_it() {
        let db = make_db();
        push_plain(&db, "default", "1 件目");
        push_plain(&db, "default", "2 件目");
        assert_eq!(db.count_monologue_cache("default", NOW).unwrap(), 2);

        let first = db.pop_monologue_cache("default", NOW).unwrap().unwrap();
        assert_eq!(first.text, "1 件目", "FIFO で最古から出す");
        assert_eq!(db.count_monologue_cache("default", NOW).unwrap(), 1);

        let second = db.pop_monologue_cache("default", NOW).unwrap().unwrap();
        assert_eq!(second.text, "2 件目");
        // 取り出しと削除は同じ tx なので、空になったら None。
        assert!(db.pop_monologue_cache("default", NOW).unwrap().is_none());
    }

    #[test]
    fn monologue_cache_roundtrips_pose_and_topic_ts() {
        let db = make_db();
        db.push_monologue_cache("default", "ねむい", Some("happy"), Some(NOW - 100), NOW)
            .expect("push");
        let row = db.pop_monologue_cache("default", NOW).unwrap().unwrap();
        assert_eq!(
            row,
            MonologueCacheRow {
                text: "ねむい".into(),
                pose: Some("happy".into()),
                topic_fetched_ts: Some(NOW - 100),
            }
        );
    }

    #[test]
    fn monologue_cache_discards_other_ghosts_rows() {
        // ゴースト切替後に前の人格の台詞を喋らない (pose もシェル依存で成立しない)。
        let db = make_db();
        push_plain(&db, "old_ghost", "前のゴーストの独り言");
        assert_eq!(db.count_monologue_cache("new_ghost", NOW).unwrap(), 0);
        assert!(db.pop_monologue_cache("new_ghost", NOW).unwrap().is_none());
        // 使えない行はその場で捨てられ、以後は old_ghost 側から見ても残っていない。
        assert_eq!(db.count_monologue_cache("old_ghost", NOW).unwrap(), 0);
    }

    #[test]
    fn monologue_cache_expires_stale_topic_rows_at_speech_time() {
        // 発話時失効 (spec §4.4.6 の 2 段目): 見出しの取得から 7 日超なら使わない。
        let db = make_db();
        let stale_topic_ts = NOW - TOPIC_MAX_AGE_SECS - 1;
        db.push_monologue_cache("default", "古いネタ入り", None, Some(stale_topic_ts), NOW)
            .expect("push");
        db.push_monologue_cache("default", "新しいネタ入り", None, Some(NOW - 60), NOW)
            .expect("push");

        assert_eq!(
            db.count_monologue_cache("default", NOW).unwrap(),
            1,
            "失効済みは在庫に数えない (数えると補充が止まる)"
        );
        let row = db.pop_monologue_cache("default", NOW).unwrap().unwrap();
        assert_eq!(row.text, "新しいネタ入り", "古いネタは飛ばして次を見る");
        assert!(db.pop_monologue_cache("default", NOW).unwrap().is_none());
    }

    #[test]
    fn monologue_cache_keeps_topic_row_exactly_at_the_boundary() {
        // ちょうど 7 日は「超えていない」ので使う (織り込み側の判定と同じ向き)。
        let db = make_db();
        db.push_monologue_cache("default", "境界", None, Some(NOW - TOPIC_MAX_AGE_SECS), NOW)
            .expect("push");
        assert_eq!(db.count_monologue_cache("default", NOW).unwrap(), 1);
        assert!(db.pop_monologue_cache("default", NOW).unwrap().is_some());
    }

    #[test]
    fn monologue_cache_expires_plain_rows_after_thirty_days() {
        // 時事ネタ無しの文も生成から 30 日を超えたら使わない (永久に残さない)。
        let db = make_db();
        let old_created = NOW - MONOLOGUE_MAX_AGE_SECS - 1;
        db.push_monologue_cache("default", "大昔の独り言", None, None, old_created)
            .expect("push");
        assert_eq!(db.count_monologue_cache("default", NOW).unwrap(), 0);
        assert!(db.pop_monologue_cache("default", NOW).unwrap().is_none());
    }

    #[test]
    fn monologue_cache_plain_row_survives_topic_ttl() {
        // 時事ネタ無しの文は、7 日を過ぎても 30 日までは使える (7 日で消えないこと)。
        let db = make_db();
        db.push_monologue_cache("default", "純粋な独り言", None, None, NOW - 10 * 24 * 3600)
            .expect("push");
        assert_eq!(db.count_monologue_cache("default", NOW).unwrap(), 1);
        assert!(db.pop_monologue_cache("default", NOW).unwrap().is_some());
    }

    #[test]
    fn clear_monologue_cache_removes_everything() {
        // 履歴クリア (§4.5.5) は include_profile に依らず全件消す。
        let db = make_db();
        push_plain(&db, "default", "a");
        db.push_monologue_cache("default", "b", None, Some(NOW), NOW)
            .expect("push");
        assert_eq!(db.clear_monologue_cache().unwrap(), 2);
        assert_eq!(db.count_monologue_cache("default", NOW).unwrap(), 0);
    }

    #[test]
    fn clear_monologue_cache_with_topics_spares_plain_rows() {
        // 時事ネタ同意の撤回では、時事ネタ入りだけ消して素の独り言は残す。
        let db = make_db();
        push_plain(&db, "default", "素の独り言");
        db.push_monologue_cache("default", "ネタ入り", None, Some(NOW), NOW)
            .expect("push");
        assert_eq!(db.clear_monologue_cache_with_topics().unwrap(), 1);
        let row = db.pop_monologue_cache("default", NOW).unwrap().unwrap();
        assert_eq!(row.text, "素の独り言");
        assert!(db.pop_monologue_cache("default", NOW).unwrap().is_none());
    }

    #[test]
    fn migration_is_idempotent_at_v9() {
        // 既存 DB への再 migrate で v9 が壊れない (テーブルもデータも残る)。
        let db = make_db();
        push_plain(&db, "default", "残るはず");
        db.migrate().expect("re-migrate");
        assert_eq!(db.count_monologue_cache("default", NOW).unwrap(), 1);
        let version = db.get_setting("db_schema_version").unwrap();
        assert_eq!(version.as_deref(), Some("9"));
    }

    #[test]
    fn voice_refs_upsert_and_get() {
        let db = make_db();
        // 初回 insert
        let id1 = db
            .upsert_voice_ref("main", "明るい女性の声", "C:/refs/main_1.wav", 100)
            .expect("upsert");
        let row = db.get_voice_ref("main").expect("get").expect("some");
        assert_eq!(row.id, id1);
        assert_eq!(row.caption, "明るい女性の声");
        assert_eq!(row.file_path, "C:/refs/main_1.wav");
        assert_eq!(row.created_ts, 100);

        // 再生成 (UPSERT): id は維持され caption/path/ts が上書き
        let id2 = db
            .upsert_voice_ref("main", "落ち着いた女性の声", "C:/refs/main_2.wav", 200)
            .expect("upsert again");
        assert_eq!(id1, id2);
        let row2 = db.get_voice_ref("main").expect("get").expect("some");
        assert_eq!(row2.caption, "落ち着いた女性の声");
        assert_eq!(row2.file_path, "C:/refs/main_2.wav");
        assert_eq!(row2.created_ts, 200);
    }

    #[test]
    fn reminders_v6_defaults_and_pending_flow() {
        let db = make_db();
        // 旧経路 (once) の登録: v6 既定値が入る
        let id = db.insert_reminder(1000, "お茶", 900).unwrap();
        let rows = db.list_reminders(ReminderFilter::All).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.id, id);
        assert_eq!(r.kind, ReminderKind::Once);
        assert!(r.active);
        assert!(!r.pending);
        assert_eq!(r.base_due_ts, None);

        // 発火対象: due_ts <= now かつ active=1
        assert_eq!(db.due_active_reminders(999).unwrap().len(), 0);
        assert_eq!(db.due_active_reminders(1000).unwrap().len(), 1);

        // 発火到達 → ログ + once は active=0 (発火済み・未完了)
        db.log_fire(id, 1000, "ghost").unwrap();
        db.deactivate_reminder(id).unwrap();
        let r = &db.list_reminders(ReminderFilter::All).unwrap()[0];
        assert!(!r.active);
        assert!(r.pending, "fired ログが残っている間は未完了");
        // 未完了は Active フィルタに残る (要対応)
        assert_eq!(db.list_reminders(ReminderFilter::Active).unwrap().len(), 1);
        assert_eq!(db.list_reminders(ReminderFilter::Completed).unwrap().len(), 0);

        // 完了 ack で pending が消え Completed へ移る
        assert!(db.set_ack(id, "completed", 1010).unwrap());
        let r = &db.list_reminders(ReminderFilter::All).unwrap()[0];
        assert!(!r.pending);
        assert_eq!(db.list_reminders(ReminderFilter::Active).unwrap().len(), 0);
        assert_eq!(db.list_reminders(ReminderFilter::Completed).unwrap().len(), 1);
        // 未処理 fired が無い状態での再 ack は false
        assert!(!db.set_ack(id, "completed", 1020).unwrap());

        let log = db.list_reminder_log(id, 10).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].ack, "completed");
        assert_eq!(log[0].ack_ts, Some(1010));
        assert_eq!(log[0].delivery, "ghost");
    }

    #[test]
    fn reminders_recurring_reschedule_and_snooze() {
        let db = make_db();
        let id = db
            .insert_reminder_ex(2000, "薬", 1900, ReminderKind::Daily, 0, 9 * 3600)
            .unwrap();
        let r = &db.list_reminders(ReminderFilter::All).unwrap()[0];
        assert_eq!(r.kind, ReminderKind::Daily);
        assert_eq!(r.time_of_day, 9 * 3600);

        // スヌーズ: 本来時刻を base_due_ts に保持。多重スヌーズでも最初の値を保つ
        db.snooze_reminder(id, 2000, 2600).unwrap();
        let r = &db.list_reminders(ReminderFilter::All).unwrap()[0];
        assert_eq!(r.due_ts, 2600);
        assert_eq!(r.base_due_ts, Some(2000));
        db.snooze_reminder(id, 2600, 3200).unwrap();
        let r = &db.list_reminders(ReminderFilter::All).unwrap()[0];
        assert_eq!(r.due_ts, 3200);
        assert_eq!(r.base_due_ts, Some(2000), "多重スヌーズは最初の本来時刻を保持");

        // 繰り返しの reschedule で base_due_ts はリセット
        db.reschedule_reminder(id, 88400).unwrap();
        let r = &db.list_reminders(ReminderFilter::All).unwrap()[0];
        assert_eq!(r.due_ts, 88400);
        assert_eq!(r.base_due_ts, None);
        assert!(r.active);
    }

    #[test]
    fn reminder_log_prune_keeps_newest() {
        let db = make_db();
        let id = db.insert_reminder(100, "x", 90).unwrap();
        for i in 0..10 {
            db.log_fire(id, 100 + i, "ghost").unwrap();
        }
        let removed = db.prune_reminder_log(3).unwrap();
        assert_eq!(removed, 7);
        let log = db.list_reminder_log(id, 100).unwrap();
        assert_eq!(log.len(), 3);
        // 新しい順に残る
        assert_eq!(log[0].fired_ts, 109);
        assert_eq!(log[2].fired_ts, 107);
    }

    #[test]
    fn delete_reminder_removes_log_too() {
        let db = make_db();
        let id = db.insert_reminder(100, "x", 90).unwrap();
        db.log_fire(id, 100, "ghost").unwrap();
        db.delete_reminder(id).unwrap();
        assert_eq!(db.list_reminders(ReminderFilter::All).unwrap().len(), 0);
        assert_eq!(db.list_reminder_log(id, 10).unwrap().len(), 0);
    }

    #[test]
    fn todos_crud_and_ordering() {
        let db = make_db();
        let a = db.insert_todo("洗い物", "today", 0, None, 100).unwrap();
        let b = db.insert_todo("資料作成", "today", 1, None, 110).unwrap();
        let c = db.insert_todo("旅行計画", "someday", 0, None, 120).unwrap();

        // bucket 絞り込み + 優先度高が先
        let today = db.list_todos(Some("today")).unwrap();
        assert_eq!(today.len(), 2);
        assert_eq!(today[0].id, b, "優先度 1 が先頭");
        assert_eq!(today[1].id, a);
        assert_eq!(db.list_todos(Some("someday")).unwrap().len(), 1);
        assert_eq!(db.list_todos(None).unwrap().len(), 3);

        // 完了 → done_ts が刻まれ、open が先に並ぶ
        db.set_todo_status(a, "done", 200).unwrap();
        let row = db.get_todo(a).unwrap().unwrap();
        assert_eq!(row.status, "done");
        assert_eq!(row.done_ts, Some(200));
        let today = db.list_todos(Some("today")).unwrap();
        assert_eq!(today[0].id, b, "open が先");
        assert_eq!(today[1].id, a);
        assert_eq!(db.count_open_todos(Some("today")).unwrap(), 1);
        assert_eq!(db.count_open_todos(None).unwrap(), 2);

        // open へ戻すと done_ts はクリア
        db.set_todo_status(a, "open", 300).unwrap();
        assert_eq!(db.get_todo(a).unwrap().unwrap().done_ts, None);

        // 部分更新 (recurring の三値: 設定 → クリア)
        db.update_todo(a, Some("食器洗い"), Some("week"), Some(1), Some(Some("daily")))
            .unwrap();
        let row = db.get_todo(a).unwrap().unwrap();
        assert_eq!(row.text, "食器洗い");
        assert_eq!(row.bucket, "week");
        assert_eq!(row.priority, 1);
        assert_eq!(row.recurring.as_deref(), Some("daily"));
        db.update_todo(a, None, None, None, Some(None)).unwrap();
        let row = db.get_todo(a).unwrap().unwrap();
        assert_eq!(row.recurring, None, "recurring だけクリアされる");
        assert_eq!(row.text, "食器洗い", "他フィールドは維持");

        db.delete_todo(c).unwrap();
        assert!(db.get_todo(c).unwrap().is_none());
    }

    #[test]
    fn count_done_todos_since_filters_by_done_ts() {
        let db = make_db();
        let a = db.insert_todo("洗い物", "today", 0, None, 10).unwrap();
        let b = db.insert_todo("資料作成", "today", 0, None, 10).unwrap();
        let c = db.insert_todo("旅行計画", "someday", 0, None, 10).unwrap();
        db.set_todo_status(a, "done", 999).unwrap(); // 昨日完了 → since=1000 では数えない
        db.set_todo_status(b, "done", 1500).unwrap(); // 今日完了 → 数える
        db.set_todo_status(c, "done", 2000).unwrap(); // 今日完了 (bucket 問わず全件対象)

        assert_eq!(db.count_done_todos_since(1000).unwrap(), 2);
        assert_eq!(db.count_done_todos_since(0).unwrap(), 3);
        assert_eq!(db.count_done_todos_since(2001).unwrap(), 0);
    }

    #[test]
    fn todos_recurring_reset_boundaries() {
        let db = make_db();
        // daily_cutoff=1000 (今日 0:00)、weekly_cutoff=500 (今週月曜 0:00) と見立てる
        let d_old = db.insert_todo("薬", "today", 0, Some("daily"), 10).unwrap();
        let d_new = db.insert_todo("体操", "today", 0, Some("daily"), 10).unwrap();
        let w_old = db.insert_todo("掃除", "week", 0, Some("weekly"), 10).unwrap();
        let w_new = db.insert_todo("買い出し", "week", 0, Some("weekly"), 10).unwrap();
        let plain = db.insert_todo("単発", "today", 0, None, 10).unwrap();

        db.set_todo_status(d_old, "done", 999).unwrap(); // 昨日完了 → 復活
        db.set_todo_status(d_new, "done", 1000).unwrap(); // 今日完了 → 維持
        db.set_todo_status(w_old, "done", 499).unwrap(); // 先週完了 → 復活
        db.set_todo_status(w_new, "done", 500).unwrap(); // 今週完了 → 維持
        db.set_todo_status(plain, "done", 1).unwrap(); // 日課でない → 維持

        let n = db.reset_recurring_todos(1000, 500).unwrap();
        assert_eq!(n, 2);
        assert_eq!(db.get_todo(d_old).unwrap().unwrap().status, "open");
        assert_eq!(db.get_todo(d_old).unwrap().unwrap().done_ts, None);
        assert_eq!(db.get_todo(d_new).unwrap().unwrap().status, "done");
        assert_eq!(db.get_todo(w_old).unwrap().unwrap().status, "open");
        assert_eq!(db.get_todo(w_new).unwrap().unwrap().status, "done");
        assert_eq!(db.get_todo(plain).unwrap().unwrap().status, "done");

        // 冪等: もう一度実行しても変化なし
        assert_eq!(db.reset_recurring_todos(1000, 500).unwrap(), 0);
    }

    #[test]
    fn calendar_upsert_notify_key_diff_and_stale() {
        let db = make_db();
        // 初回挿入
        db.upsert_calendar_event(0, "u1", None, "会議", 1000, Some(1100), false, "confirmed", "k-会議-1000-1100", false, 5000).unwrap();
        db.upsert_calendar_event(0, "u2", None, "歯医者", 2000, Some(2100), false, "confirmed", "k-歯医者-2000-2100", false, 5000).unwrap();
        // u1 を通知済みにする
        db.mark_calendar_notified(0, "u1", 1000).unwrap();
        assert!(db.list_calendar(0, 9999).unwrap().iter().find(|e| e.uid == "u1").unwrap().notified);

        // 再取得 (fetch_ts=6000)。u1 は notify_key 不変 → notified 保持。u2 は summary 変更 → リセット。
        db.upsert_calendar_event(0, "u1", None, "会議", 1000, Some(1100), false, "confirmed", "k-会議-1000-1100", false, 6000).unwrap();
        db.mark_calendar_notified(0, "u2", 2000).unwrap(); // 一旦通知済みに
        db.upsert_calendar_event(0, "u2", None, "歯医者(変更)", 2000, Some(2100), false, "confirmed", "k-歯医者変更-2000-2100", false, 6000).unwrap();
        let rows = db.list_calendar(0, 9999).unwrap();
        assert!(rows.iter().find(|e| e.uid == "u1").unwrap().notified, "notify_key 不変で notified 保持");
        assert!(!rows.iter().find(|e| e.uid == "u2").unwrap().notified, "notify_key 変更で notified リセット");

        // 別ソースの同一 start は複合キーで別行 (相互上書きしない)
        db.upsert_calendar_event(1, "u1", None, "別ソース", 1000, None, false, "confirmed", "k-別-1000", false, 6000).unwrap();
        assert_eq!(db.list_calendar(500, 1500).unwrap().len(), 2);

        // stale 削除: source 0 を古い fetch_ts=6000 で再取得しなかった u3 は消える
        db.upsert_calendar_event(0, "u3", None, "消える予定", 1200, None, false, "confirmed", "k-u3", false, 5000).unwrap();
        let removed = db.delete_stale_calendar(0, 6000).unwrap();
        assert_eq!(removed, 1, "fetch_ts<6000 の u3 のみ削除");
        assert!(db.list_calendar(0, 9999).unwrap().iter().all(|e| e.uid != "u3"));
    }

    #[test]
    fn calendar_upcoming_prune_clear() {
        let db = make_db();
        db.upsert_calendar_event(0, "a", None, "近い", 1000, Some(1100), false, "confirmed", "ka", false, 1).unwrap();
        db.upsert_calendar_event(0, "b", None, "遠い", 5000, None, false, "confirmed", "kb", false, 1).unwrap();
        db.upsert_calendar_event(0, "c", None, "キャンセル", 1050, None, false, "cancelled", "kc", false, 1).unwrap();
        // now=900, horizon=1200 → a のみ (b は範囲外、c は cancelled)
        let up = db.upcoming_calendar(900, 1200).unwrap();
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].uid, "a");
        // 通知済みは対象外
        db.mark_calendar_notified(0, "a", 1000).unwrap();
        assert_eq!(db.upcoming_calendar(900, 1200).unwrap().len(), 0);
        // 終日 (all_day=1): 開始 (0:00) を過ぎても当日中 (start+86400) は候補に残る
        // — 通知時刻 8:00 は start より後のため (M10 reviewer 指摘の回帰テスト)
        db.upsert_calendar_event(0, "d", None, "終日", 100, None, true, "confirmed", "kd", false, 1).unwrap();
        let up = db.upcoming_calendar(500, 90_000).unwrap();
        assert!(up.iter().any(|e| e.uid == "d"), "開始後でも当日中の終日予定は候補");
        assert_eq!(db.upcoming_calendar(100 + 86_400, 200_000).unwrap().len(), 0, "翌日には消える");
        // prune: end/start < 1200 の過去分 (a=end1100, c=start1050, d=start100) が消える
        assert_eq!(db.prune_calendar(1200).unwrap(), 3);
        // clear
        db.clear_calendar().unwrap();
        assert_eq!(db.list_calendar(0, 99999).unwrap().len(), 0);
    }

    #[test]
    fn voice_refs_list_and_delete() {
        let db = make_db();
        db.upsert_voice_ref("main", "M", "C:/refs/main.wav", 1).unwrap();
        db.upsert_voice_ref("sub", "S", "C:/refs/sub.wav", 2).unwrap();
        let list = db.list_voice_refs().unwrap();
        assert_eq!(list.len(), 2);
        // ORDER BY slot ASC → main, sub
        assert_eq!(list[0].slot, "main");
        assert_eq!(list[1].slot, "sub");

        db.delete_voice_ref("main").unwrap();
        let list2 = db.list_voice_refs().unwrap();
        assert_eq!(list2.len(), 1);
        assert_eq!(list2[0].slot, "sub");
        assert!(db.get_voice_ref("main").unwrap().is_none());
    }
}

/// 起動時の整合性検査と、破損時の非破壊的な保全 (v0.5.1)。
///
/// **DB は作り直さない。** 破損を理由に消すと、リマインダー・ToDo・記憶・設定が
/// 一緒に消える。ここでやるのは次の 3 つだけ:
/// 1. `PRAGMA quick_check` で破損を検知して記録する（以前は誰も気づけなかった）
/// 2. 原本を **WAL/SHM ごと** タイムスタンプ付きでコピーする（以後の操作で悪化しても戻せる）
/// 3. `VACUUM INTO` で読み出せた分だけの健全なコピーを作る（救出。失敗しても続行）
///
/// どう復旧するかはユーザーが決める。アプリは通常どおり起動を続ける
/// （起動できないより、壊れたまま動くほうがデータを取り出せる）。
///
/// **保全は破損 1 件につき 1 回だけ。** 既に退避がある場合は作り直さず、その
/// パスを返す。毎起動コピーすると、破損した DB を抱えたまま使い続けるユーザー
/// （＝まさにこの機能が対象にしている人）のディスクを起動のたびに食い潰す。
/// 先に作られたコピーのほうが原本に近いので、古いものを残すのが正しい。
fn check_integrity_and_preserve(conn: &Connection, path: &Path) -> DbIntegrity {
    let detail = match conn.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)) {
        Ok(v) => v,
        Err(err) => format!("quick_check を実行できませんでした: {err}"),
    };
    if detail == "ok" {
        return DbIntegrity {
            ok: true,
            detail,
            ..Default::default()
        };
    }

    crate::ulog!("[db] 整合性検査に失敗しました: {detail}");
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");

    // 1) 原本のコピー。壊れていても「今の状態」を保全する。
    let backup_path = match find_preserved(path, "corrupt-") {
        Some(existing) => {
            crate::ulog!("[db] 退避済みのため作り直しません: {}", existing.display());
            Some(existing.to_string_lossy().into_owned())
        }
        None => {
            let backup = path.with_extension(format!("corrupt-{stamp}.db"));
            match std::fs::copy(path, &backup) {
                Ok(_) => {
                    // **WAL / SHM も一緒に運ぶ。** これが無いと、まだ本体へ
                    // チェックポイントされていない書き込みが退避から丸ごと抜ける
                    // (実測で -wal が本体の数倍あることがある)。命名を
                    // `<backup>-wal` に揃えてあるので、退避 DB をそのまま開ける。
                    copy_sidecars(path, &backup);
                    crate::ulog!("[db] 原本を退避しました: {}", backup.display());
                    Some(backup.to_string_lossy().into_owned())
                }
                Err(err) => {
                    crate::ulog!("[db] 原本の退避に失敗: {err}");
                    None
                }
            }
        }
    };

    // 2) 読み出せた分だけの健全なコピー。VACUUM INTO は元 DB を書き換えない。
    //    開いている接続を通すので WAL の内容も反映される。
    let salvaged_path = match find_preserved(path, "salvaged-") {
        Some(existing) => {
            crate::ulog!("[db] 救出済みのため作り直しません: {}", existing.display());
            Some(existing.to_string_lossy().into_owned())
        }
        None => {
            let salvaged = path.with_extension(format!("salvaged-{stamp}.db"));
            match conn.execute("VACUUM INTO ?1", [salvaged.to_string_lossy().as_ref()]) {
                Ok(_) => {
                    crate::ulog!("[db] 救出コピーを作成しました: {}", salvaged.display());
                    Some(salvaged.to_string_lossy().into_owned())
                }
                Err(err) => {
                    crate::ulog!("[db] 救出コピーの作成に失敗 (読み出せない範囲がある): {err}");
                    None
                }
            }
        }
    };

    DbIntegrity {
        ok: false,
        detail,
        backup_path,
        salvaged_path,
    }
}

/// 既に作られている退避/救出コピー (`<stem>.<prefix><stamp>.db`) を探す。
/// 複数あればファイル名順で最も古いもの＝最初に作られたものを返す
/// (原本に最も近い世代を正とする)。
fn find_preserved(path: &Path, prefix: &str) -> Option<std::path::PathBuf> {
    let dir = path.parent()?;
    let stem = path.file_stem()?.to_str()?;
    let head = format!("{stem}.{prefix}");
    let mut hits: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&head) && n.ends_with(".db"))
        })
        .collect();
    hits.sort();
    hits.into_iter().next()
}

/// SQLite のサイドカー (`-wal` / `-shm`) を退避先へ同じ規則でコピーする。
/// 無ければ何もしない。失敗しても本体の退避は成立しているので続行する。
fn copy_sidecars(src_db: &Path, dst_db: &Path) {
    for suffix in ["-wal", "-shm"] {
        let src = sidecar(src_db, suffix);
        if !src.is_file() {
            continue;
        }
        let dst = sidecar(dst_db, suffix);
        if let Err(err) = std::fs::copy(&src, &dst) {
            crate::ulog!("[db] {suffix} の退避に失敗 (本体は退避済み): {err}");
        }
    }
}

fn sidecar(db: &Path, suffix: &str) -> std::path::PathBuf {
    let mut s = db.as_os_str().to_os_string();
    s.push(suffix);
    std::path::PathBuf::from(s)
}

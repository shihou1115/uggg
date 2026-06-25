use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

pub struct Db {
    conn: Mutex<Connection>,
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

/// リマインダー 1 件 (M5-B)。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReminderRow {
    pub id: i64,
    pub due_ts: i64,
    pub text: String,
    pub created_ts: i64,
}

/// 興味分野 1 件 (M5-C, architecture §2.2)。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InterestTopic {
    pub id: i64,
    pub topic: String,
    pub enabled: bool,
}

/// 時事ネタキャッシュ 1 件 (M5-C, advanced 独り言混入用)。
/// advanced 独り言経路への結線は将来課題 (現状はキャッシュに蓄積するのみ)。
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Serialize)]
pub struct TopicCacheRow {
    pub id: i64,
    pub topic: String,
    pub headline: String,
    pub link: String,
    pub fetched_ts: i64,
}

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
        Ok(Self {
            conn: Mutex::new(conn),
        })
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

    // ===== reminders (M5-B) =====

    pub fn insert_reminder(&self, due_ts: i64, text: &str, created_ts: i64) -> Result<i64> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "INSERT INTO reminders (due_ts, text, created_ts) VALUES (?1, ?2, ?3)",
            params![due_ts, text, created_ts],
        )
        .context("insert_reminder")?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_reminders(&self) -> Result<Vec<ReminderRow>> {
        let conn = self.conn.lock().expect("db poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, due_ts, text, created_ts FROM reminders ORDER BY due_ts ASC",
            )
            .context("prepare list_reminders")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ReminderRow {
                    id: row.get(0)?,
                    due_ts: row.get(1)?,
                    text: row.get(2)?,
                    created_ts: row.get(3)?,
                })
            })
            .context("query_map list_reminders")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("row list_reminders")?);
        }
        Ok(out)
    }

    /// `due_ts <= now` のリマインダーを返す (発火対象)。
    pub fn due_reminders(&self, now_ts: i64) -> Result<Vec<ReminderRow>> {
        let conn = self.conn.lock().expect("db poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, due_ts, text, created_ts FROM reminders
                 WHERE due_ts <= ?1 ORDER BY due_ts ASC",
            )
            .context("prepare due_reminders")?;
        let rows = stmt
            .query_map(params![now_ts], |row| {
                Ok(ReminderRow {
                    id: row.get(0)?,
                    due_ts: row.get(1)?,
                    text: row.get(2)?,
                    created_ts: row.get(3)?,
                })
            })
            .context("query_map due_reminders")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("row due_reminders")?);
        }
        Ok(out)
    }

    pub fn delete_reminder(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute("DELETE FROM reminders WHERE id = ?1", params![id])
            .context("delete_reminder")?;
        Ok(())
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
            .ok();
        Ok(row)
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
            .ok();
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

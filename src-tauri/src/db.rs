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

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct UsageSummary {
    pub month_usd: f64,
    pub limit_usd: f64,
    pub limited: bool,
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

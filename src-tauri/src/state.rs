use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tokio::sync::Semaphore;

use crate::db::Db;
use crate::ghost::{self, GhostBundle};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogueMode {
    Low,
    Advanced,
}

impl Default for DialogueMode {
    fn default() -> Self {
        DialogueMode::Low
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TalkSpeed {
    Slow,
    Normal,
    Fast,
    Instant,
}

impl Default for TalkSpeed {
    fn default() -> Self {
        // M1 検証用に瞬時表示。実機運用では Normal が既定。
        TalkSpeed::Instant
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub mode: DialogueMode,
    pub ghost_id: String,
    pub shell_id: String,
    pub display_scale: f64,
    pub quiet_mode: bool,
    pub talk_speed: TalkSpeed,
    // === LLM (advanced モード) ===
    /// provider 名 (keyring user としても使う)。spec §4.2.2 で OpenAI 互換のみサポート。
    /// 既定 "openai" (公式)、LMStudio/Ollama 等は別名で keyring を分ける。
    #[serde(default = "default_llm_provider")]
    pub llm_provider: String,
    /// モデル名。例: "gpt-4o-mini" / "local-model"。
    #[serde(default = "default_llm_model")]
    pub llm_model: String,
    /// base_url オーバーライド。None なら OpenAI 公式 (`https://api.openai.com/v1`)。
    /// LMStudio: `http://localhost:1234/v1`、Ollama: `http://localhost:11434/v1` 等。
    #[serde(default)]
    pub llm_base_url: Option<String>,
    /// 月次コスト上限 (USD)。0 以下なら無制限扱い。
    #[serde(default = "default_monthly_limit_usd")]
    pub monthly_limit_usd: f64,
    /// user_profile の件数上限 (origin=auto のみ要約/削除対象)。
    #[serde(default = "default_profile_max_count")]
    pub profile_max_count: u32,
    // === 存在感 (M3) ===
    /// フルスクリーンアプリ前面時に自動静音 (spec §4.4.9、既定 OFF)。
    #[serde(default)]
    pub auto_quiet_fullscreen: bool,
    /// ランダムトーク間隔 (分)。0 で無効 (spec §4.4.4、既定 10)。
    #[serde(default = "default_monologue_interval_min")]
    pub monologue_interval_min: u32,
    /// ポモドーロ集中 (分)。
    #[serde(default = "default_pomodoro_work_min")]
    pub pomodoro_work_min: u32,
    /// ポモドーロ休憩 (分)。
    #[serde(default = "default_pomodoro_break_min")]
    pub pomodoro_break_min: u32,
    /// ポモドーロのラウンド数。
    #[serde(default = "default_pomodoro_rounds")]
    pub pomodoro_rounds: u32,
}

fn default_llm_provider() -> String {
    "openai".to_string()
}

fn default_llm_model() -> String {
    "gpt-4o-mini".to_string()
}

fn default_monthly_limit_usd() -> f64 {
    5.0
}

fn default_profile_max_count() -> u32 {
    200
}

fn default_monologue_interval_min() -> u32 {
    10
}

fn default_pomodoro_work_min() -> u32 {
    25
}

fn default_pomodoro_break_min() -> u32 {
    5
}

fn default_pomodoro_rounds() -> u32 {
    4
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: DialogueMode::Low,
            ghost_id: "default".to_string(),
            shell_id: "default".to_string(),
            display_scale: 1.0,
            quiet_mode: false,
            talk_speed: TalkSpeed::Normal,
            llm_provider: default_llm_provider(),
            llm_model: default_llm_model(),
            llm_base_url: None,
            monthly_limit_usd: default_monthly_limit_usd(),
            profile_max_count: default_profile_max_count(),
            auto_quiet_fullscreen: false,
            monologue_interval_min: default_monologue_interval_min(),
            pomodoro_work_min: default_pomodoro_work_min(),
            pomodoro_break_min: default_pomodoro_break_min(),
            pomodoro_rounds: default_pomodoro_rounds(),
        }
    }
}

impl Settings {
    /// 値を仕様の許容範囲に丸める。set_settings 経路で必ず通す。
    pub fn clamp(&mut self) {
        if !self.display_scale.is_finite() || self.display_scale < 0.5 {
            self.display_scale = 0.5;
        }
        if self.display_scale > 2.0 {
            self.display_scale = 2.0;
        }
        if !self.monthly_limit_usd.is_finite() || self.monthly_limit_usd < 0.0 {
            self.monthly_limit_usd = 0.0;
        }
        if self.profile_max_count == 0 {
            self.profile_max_count = 1;
        }
        if self.llm_model.trim().is_empty() {
            self.llm_model = default_llm_model();
        }
        if self.llm_provider.trim().is_empty() {
            self.llm_provider = default_llm_provider();
        }
        if let Some(url) = &self.llm_base_url {
            if url.trim().is_empty() {
                self.llm_base_url = None;
            }
        }
        // ポモドーロは 1 分以上、ラウンドは 1 以上
        if self.pomodoro_work_min == 0 {
            self.pomodoro_work_min = default_pomodoro_work_min();
        }
        if self.pomodoro_break_min == 0 {
            self.pomodoro_break_min = default_pomodoro_break_min();
        }
        if self.pomodoro_rounds == 0 {
            self.pomodoro_rounds = default_pomodoro_rounds();
        }
        // monologue_interval_min は 0 (無効) を許容、上限のみ常識的に丸める
        if self.monologue_interval_min > 1440 {
            self.monologue_interval_min = 1440;
        }
    }
}

pub struct AppState {
    pub db: Db,
    pub settings: Mutex<Settings>,
    // ghost/shell の読み込みに失敗した場合は Err を保持し、get_boot_payload が
    // フロントへその文字列を返す（boot 時に panic せず、ウインドウ上でエラーを表示する）。
    pub ghost: Mutex<Result<GhostBundle, String>>,
    pub window: WindowState,
    pub dialogue: DialogueState,
    pub presence: PresenceState,
    pub pomodoro: PomodoroState,
}

/// 存在感系サブ状態 (放置反応・ウインドウ位置)。
pub struct PresenceState {
    /// 現放置期間に idle を発火済みか (操作でリセット)。
    pub idle_fired: AtomicBool,
    /// 最後にウインドウ位置が変わった unix 秒 (3 秒デバウンス保存用)。
    pub pos_dirty_since: AtomicI64,
}

impl Default for PresenceState {
    fn default() -> Self {
        Self {
            idle_fired: AtomicBool::new(false),
            pos_dirty_since: AtomicI64::new(0),
        }
    }
}

/// ポモドーロ状態機械。phase: 0=idle, 1=focus, 2=break。
pub struct PomodoroState {
    /// 集中中フラグ (静音判定が参照)。
    pub focus: AtomicBool,
    /// 世代カウンタ。stop / 新規 start で +1 し、古いタスクを失効させる。
    pub gen: AtomicU64,
    /// 0=idle, 1=focus, 2=break。
    pub phase: AtomicU32,
    /// 現フェーズの残り秒。
    pub remaining: AtomicU32,
    /// 現ラウンド (1-based)。
    pub round: AtomicU32,
    /// 総ラウンド数。
    pub rounds: AtomicU32,
}

impl Default for PomodoroState {
    fn default() -> Self {
        Self {
            focus: AtomicBool::new(false),
            gen: AtomicU64::new(0),
            phase: AtomicU32::new(0),
            remaining: AtomicU32::new(0),
            round: AtomicU32::new(0),
            rounds: AtomicU32::new(0),
        }
    }
}

/// 対話進行のサブ状態。
pub struct DialogueState {
    /// 同時に走らせない: send_user_message / random_talk が同じ semaphore を取る。permits=1。
    pub busy: Arc<Semaphore>,
    /// 最後にユーザー操作があった unix 秒。idle / 降格復帰の判定に使う。
    pub last_interaction: AtomicI64,
    /// 一時降格期限の unix 秒。0 なら降格なし。
    pub degraded_until: AtomicI64,
    /// 連続 API エラー回数。閾値超過で降格させる。
    pub error_streak: AtomicI64,
    /// コスト 80% 警告告知済みフラグ (月内一度きり)。
    pub cost_warning_80_emitted: AtomicBool,
    /// コスト上限超過の告知済みフラグ (月内一度きり告知のため)。
    pub cost_limited_emitted: AtomicBool,
    /// 起動挨拶済みフラグ (二重発火防止)。
    pub greeted: AtomicBool,
}

impl Default for DialogueState {
    fn default() -> Self {
        Self {
            busy: Arc::new(Semaphore::new(1)),
            last_interaction: AtomicI64::new(0),
            degraded_until: AtomicI64::new(0),
            error_streak: AtomicI64::new(0),
            cost_warning_80_emitted: AtomicBool::new(false),
            cost_limited_emitted: AtomicBool::new(false),
            greeted: AtomicBool::new(false),
        }
    }
}

#[derive(Default)]
pub struct WindowState {
    pub alpha_mask: Mutex<DecodedMask>,
}

/// フロントから来るアルファマスク。
/// cell_size_css は 8 (CSS px / cell)、cols×rows のグリッドで `data[idx]==0` なら透過セル、
/// それ以外なら不透明セル。
#[derive(Debug, Default)]
pub struct DecodedMask {
    pub cols: u32,
    pub rows: u32,
    pub cell_size_css: u32,
    pub data: Vec<u8>,
}

impl AppState {
    pub fn initialize(app: &AppHandle) -> Result<Self> {
        let data_dir = resolve_app_data_dir(app)?;
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("create app data dir: {}", data_dir.display()))?;

        let db = Db::open(&data_dir.join("companion.db"))?;
        db.migrate()?;

        // 永続化された Settings を優先。無ければデフォルトを使う。
        let settings = crate::commands::settings::load_persisted_settings(&db, Settings::default());

        let assets_dir = resolve_assets_dir(app)?;
        let ghost = ghost::load_bundle(&assets_dir, &settings.ghost_id, &settings.shell_id)
            .map_err(|err| format!("{err:#}"));

        Ok(Self {
            db,
            settings: Mutex::new(settings),
            ghost: Mutex::new(ghost),
            window: WindowState::default(),
            dialogue: DialogueState::default(),
            presence: PresenceState::default(),
            pomodoro: PomodoroState::default(),
        })
    }
}

fn resolve_app_data_dir(_app: &AppHandle) -> Result<PathBuf> {
    // architecture.md §2.4: ファイル資産は `%APPDATA%\ugg\` 配下。
    // Tauri 既定の app_data_dir はバンドル識別子（io.ugg.app）を使うので使わず、
    // %APPDATA% を直接参照する（本アプリは Windows 専用）。
    let appdata = std::env::var_os("APPDATA")
        .ok_or_else(|| anyhow!("環境変数 APPDATA が設定されていません"))?;
    Ok(PathBuf::from(appdata).join("ugg"))
}

fn resolve_assets_dir(app: &AppHandle) -> Result<PathBuf> {
    // 1) dev: workspace root (tauri.conf.json's parent's parent).
    // 2) prod: alongside the executable.
    // For M0 we look beside the resource dir first, then walk up.
    let candidates: Vec<PathBuf> = vec![
        app.path()
            .resource_dir()
            .map(|p| p.to_path_buf())
            .unwrap_or_default(),
        std::env::current_dir().unwrap_or_default(),
        std::env::current_dir()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_default(),
    ];

    for base in candidates {
        if base.as_os_str().is_empty() {
            continue;
        }
        if base.join("ghosts").is_dir() && base.join("shells").is_dir() {
            return Ok(base);
        }
    }

    anyhow::bail!(
        "ghosts/ and shells/ directories were not found near the executable or current directory"
    )
}

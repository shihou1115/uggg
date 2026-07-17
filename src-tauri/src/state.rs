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
    // === TTS (M4) ===
    /// TTS 有効化 (既定 false; 資産 DL 前は声なしで動く)。
    #[serde(default)]
    pub tts_enabled: bool,
    /// TTS エンジン種別 (現状 "voicevox_core" のみ。M4c で "irodori" を追加予定)。
    #[serde(default = "default_tts_engine")]
    pub tts_engine: String,
    /// メインキャラの話者(style)ID。
    #[serde(default = "default_tts_speaker_main")]
    pub tts_speaker_main: u32,
    /// サブキャラの話者(style)ID。
    #[serde(default = "default_tts_speaker_sub")]
    pub tts_speaker_sub: u32,
    /// 話速 (voicevox の speedScale 相当。0.5〜2.0 clamp)。
    #[serde(default = "default_tts_speed")]
    pub tts_speed: f64,
    /// 音量 (voicevox の volumeScale 相当。0.0〜2.0 clamp)。
    #[serde(default = "default_tts_volume")]
    pub tts_volume: f64,
    /// Irodori-TTS で実モデル推論を使うか (false ならモック wav)。
    /// M4c Phase G 時点では既定 false (実 Aratako/Irodori-TTS モデルの結線は実機検証で確定する)。
    #[serde(default)]
    pub tts_irodori_use_real_model: bool,
    /// M5-H: OS ログイン時の自動起動 (既定 false、spec §4.5.4)。
    /// 値変更時にフロントから `set_autostart` を呼んでプラグイン側の状態と同期する。
    #[serde(default)]
    pub autostart: bool,
    /// M5-D: 更新情報の取得元 URL (JSON フィード、`{ latest, url, notes }` 形式)。
    /// 未設定なら更新チェックを行わない (spec §4.5.6)。
    #[serde(default)]
    pub update_feed_url: Option<String>,
    /// M5-C: 時事ネタを advanced 独り言に混ぜるか (既定 false、spec §4.5)。
    /// オンボーディングで同意済みのときに true になる想定だが、設定パネルからも切替可能。
    #[serde(default)]
    pub topics_enabled: bool,
    /// M5-B: ツール (時刻 / クリップボード) の有効化 (既定 false、spec §4.5.3)。
    /// M7 以降は「クリップボード 📋 + advanced 時刻注入」用途に縮小
    /// (リマインダーは daily_support_enabled 配下の常時ローカル機能へ移行、
    /// daily-support-design §4.5.3 移行 / spec §4.2.1 不変条件)。
    #[serde(default)]
    pub tools_enabled: bool,
    // === 日常支援 Tier S (M7、daily-support-design §5) ===
    /// Tier S マスタスイッチ (既定 true、v0.2 の目玉)。自然文リマインダー登録・
    /// Tier S 通知の大元。個別カテゴリ (situation_* 等) は既定 OFF のオプトイン。
    #[serde(default = "default_true")]
    pub daily_support_enabled: bool,
    // --- 発話ガバナンス (§4.6.3。カテゴリ発火の実装は M9、設定は共通基盤として M7 で導入) ---
    /// 休憩促し (連続利用) 発話 (既定 false)。
    #[serde(default)]
    pub situation_break_enabled: bool,
    /// 深夜利用の声かけ (既定 false)。
    #[serde(default)]
    pub situation_late_night_enabled: bool,
    /// バッテリー低下の声かけ (既定 false)。
    #[serde(default)]
    pub situation_battery_enabled: bool,
    /// ToDo/リマインダー未完了フォロー発話 (既定 false)。
    #[serde(default)]
    pub todo_follow_enabled: bool,
    /// 状況発話系 (Situation*) Ambient の最低発話間隔 (分、既定 30)。
    /// monologue/idle には適用しない (それぞれ既存の間隔設定を維持、設計 §4.2)。
    #[serde(default = "default_min_speak_interval_min")]
    pub min_speak_interval_min: u32,
    /// 夜間静音の有効化 (既定 false)。時刻値は番兵にせず独立フラグで持つ。
    #[serde(default)]
    pub night_quiet_enabled: bool,
    /// 夜間静音の開始 (0:00 からの分、既定 1380 = 23:00)。0 も有効値 (=0:00)。
    #[serde(default = "default_night_quiet_from")]
    pub night_quiet_from: u16,
    /// 夜間静音の終了 (既定 420 = 07:00)。from > to は日跨ぎ、from == to は終日。
    #[serde(default = "default_night_quiet_to")]
    pub night_quiet_to: u16,
    // --- リマインダー (§4.6.1) ---
    /// リマインダー発火通知の on/off (既定 true)。登録自体は常時可能。
    #[serde(default = "default_true")]
    pub reminder_notify_enabled: bool,
}

fn default_true() -> bool {
    true
}

fn default_min_speak_interval_min() -> u32 {
    30
}

fn default_night_quiet_from() -> u16 {
    1380 // 23:00
}

fn default_night_quiet_to() -> u16 {
    420 // 07:00
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

fn default_tts_engine() -> String {
    "voicevox_core".to_string()
}

fn default_tts_speaker_main() -> u32 {
    2
}

fn default_tts_speaker_sub() -> u32 {
    3
}

fn default_tts_speed() -> f64 {
    1.0
}

fn default_tts_volume() -> f64 {
    1.0
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
            tts_enabled: false,
            tts_engine: default_tts_engine(),
            tts_speaker_main: default_tts_speaker_main(),
            tts_speaker_sub: default_tts_speaker_sub(),
            tts_speed: default_tts_speed(),
            tts_volume: default_tts_volume(),
            tts_irodori_use_real_model: false,
            autostart: false,
            update_feed_url: None,
            topics_enabled: false,
            tools_enabled: false,
            daily_support_enabled: true,
            situation_break_enabled: false,
            situation_late_night_enabled: false,
            situation_battery_enabled: false,
            todo_follow_enabled: false,
            min_speak_interval_min: default_min_speak_interval_min(),
            night_quiet_enabled: false,
            night_quiet_from: default_night_quiet_from(),
            night_quiet_to: default_night_quiet_to(),
            reminder_notify_enabled: true,
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
        if let Some(url) = &self.update_feed_url {
            if url.trim().is_empty() {
                self.update_feed_url = None;
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
        // TTS パラメータ clamp
        if !self.tts_speed.is_finite() || self.tts_speed < 0.5 {
            self.tts_speed = 0.5;
        }
        if self.tts_speed > 2.0 {
            self.tts_speed = 2.0;
        }
        if !self.tts_volume.is_finite() || self.tts_volume < 0.0 {
            self.tts_volume = 0.0;
        }
        if self.tts_volume > 2.0 {
            self.tts_volume = 2.0;
        }
        if self.tts_engine.trim().is_empty() {
            self.tts_engine = default_tts_engine();
        }
        // 日常支援 (M7): 分単位の時刻は 0..=1439、間隔は上限 1440 に丸める
        if self.night_quiet_from > 1439 {
            self.night_quiet_from = default_night_quiet_from();
        }
        if self.night_quiet_to > 1439 {
            self.night_quiet_to = default_night_quiet_to();
        }
        if self.min_speak_interval_min > 1440 {
            self.min_speak_interval_min = 1440;
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
    pub tts: TtsState,
    /// 発話ガバナンスのインメモリ状態 (M7、daily-support-design §4.2。DB 化しない)。
    pub governance: crate::system::governance::GovernanceState,
}

/// TTS エンジンの遅延初期化を抱えるサブ状態。
/// - voicevox: 初期化が重い (数秒) ので Mutex<Option<...>> で AppState に保持して使い回す。
/// - irodori: HTTP クライアント本体は軽量で &self メソッドのみ。サイドカープロセスや
///   ベース URL のような可変状態は IrodoriClient 内部の Mutex で隔離する (M4c 以降)。
pub struct TtsState {
    pub voicevox: Mutex<Option<crate::tts::voicevox::VoicevoxEngine>>,
    pub irodori: crate::tts::irodori::IrodoriClient,
}

impl Default for TtsState {
    fn default() -> Self {
        Self {
            voicevox: Mutex::new(None),
            irodori: crate::tts::irodori::IrodoriClient::new(),
        }
    }
}

/// 存在感系サブ状態 (放置反応・読み上げ)。
pub struct PresenceState {
    /// 現放置期間に idle を発火済みか (操作でリセット)。
    pub idle_fired: AtomicBool,
    /// テキスト読み上げ中フラグ (docs/text-reader-spec.md K6)。
    /// true の間は自発発話 (独り言・放置反応) を抑制する。永続化しない一時状態。
    pub reading: AtomicBool,
}

impl Default for PresenceState {
    fn default() -> Self {
        Self {
            idle_fired: AtomicBool::new(false),
            reading: AtomicBool::new(false),
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
    /// 一時停止中フラグ (spec §4.4.5)。true の間はカウントダウンを止める。
    pub paused: AtomicBool,
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
            paused: AtomicBool::new(false),
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
        let data_dir = resolve_app_data_dir()?;
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
            tts: TtsState::default(),
            governance: crate::system::governance::GovernanceState::default(),
        })
    }
}

pub fn resolve_app_data_dir() -> Result<PathBuf> {
    // architecture.md §2.4: ファイル資産は `%APPDATA%\ugg\` 配下。
    // Tauri 既定の app_data_dir はバンドル識別子（io.ugg.app）を使うので使わず、
    // %APPDATA% を直接参照する（本アプリは Windows 専用）。
    let appdata = std::env::var_os("APPDATA")
        .ok_or_else(|| anyhow!("環境変数 APPDATA が設定されていません"))?;
    Ok(PathBuf::from(appdata).join("ugg"))
}

/// voicevox_core の資産ディレクトリ (`%APPDATA%\ugg\voicevox`)。
pub fn voicevox_asset_dir() -> Result<PathBuf> {
    Ok(resolve_app_data_dir()?.join("voicevox"))
}

pub fn resolve_assets_dir(app: &AppHandle) -> Result<PathBuf> {
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

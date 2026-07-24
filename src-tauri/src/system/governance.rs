//! 発話ガバナンス (M7、daily-support-design §4。M11 で 9→12 カテゴリへ拡張、
//! regular-talk-design §4)。
//!
//! すべての自発発話 (独り言・idle・通知・催促・状況発話) の可否を一元判定する。
//! **判定 (`can_deliver`) と記録 (`record_delivered`) は分離**し、両方を
//! `deliver::deliver_event` だけが直列化されたクリティカルセクション内で呼ぶ
//! (二重ゲート禁止。呼び出し側 watcher はどちらも直接呼ばない)。
//! ユーザー起点の応答 (`send_user_message` の戻り値経路) はゲートしない。
//!
//! 判定表 (§4.2。Notice は「必ず届く」を保証し全段を越える):
//!
//! | 段 | 条件 | Notice | Ambient |
//! |---|---|---|---|
//! | 1 ハード静音 | should_stay_quiet (集中/読み上げ/quiet_mode/フルスクリーン) | 通す | ブロック |
//! | 2 夜間静音 | night_quiet 帯内 | 通す | ブロック |
//! | 3 カテゴリ OFF | 当該カテゴリ設定が OFF | 免除 | ブロック |
//! | 4 最低間隔 | 直近発話から min_speak_interval 未満 (**Situation* のみ**) | 免除 | ブロック |
//! | 5 連投回避 | 同カテゴリが 120 分 × (1+backoff) 以内 (**Situation* のみ**、M9) | 免除 | ブロック |
//!
//! 🔕 フィードバック (M9、§4.3): 「邪魔だった」1 回ごとにカテゴリの backoff を +1 し、
//! 段 5 の間隔を線形に延長する (指数学習はしない)。3 回でカテゴリトグル自体を OFF。
//! backoff は `app_settings` の `governance_backoff:<category>` に永続化し、
//! 起動時に `load_backoff` でインメモリへ復元する。
//! それ以外のガバナンス状態は DB 化せずインメモリ (`GovernanceState`、§2.4 で speech_log 撤回)。

use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Timelike;

use crate::presence::quiet;
use crate::state::{AppState, Settings};

/// 自発発話のカテゴリ (daily-support-design §4.2)。
/// M7 で発火するのは Monologue / Idle / Reminder。残りは設計 §4.2 判定表の契約の
/// 一部として M7 で定義済みで、発火元は M8 (Todo) / M9 (Situation*) / M10 (Calendar)
/// が結線する (それまで未構築の variant を許容)。
/// M11 で 3 variant を一括追加 (regular-talk-design §4.1、同じ「未構築 variant を許容」
/// 先例に従う): SituationRain は M11 (daily watcher) が結線、RegularMorning/RegularEvening
/// は Settings フィールドのみ M11 で追加し発火は M12 が結線する (既定 false で inert)。
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechCategory {
    Monologue,
    Idle,
    Reminder,
    Todo,
    Calendar,
    SituationBreak,
    SituationLateNight,
    SituationBattery,
    SituationTodoFollow,
    /// 降雨の一言 (M11、spec §4.7.2)。is_situation() 対象 (段 4/5 適用)。
    SituationRain,
    /// 朝の定例会話 (M12 で発火結線)。is_situation() 対象外 (間隔バックオフ非適用)。
    RegularMorning,
    /// 夜の定例会話 (M12 で発火結線)。同上。
    RegularEvening,
}

impl SpeechCategory {
    pub const COUNT: usize = 12;

    pub fn index(self) -> usize {
        match self {
            SpeechCategory::Monologue => 0,
            SpeechCategory::Idle => 1,
            SpeechCategory::Reminder => 2,
            SpeechCategory::Todo => 3,
            SpeechCategory::Calendar => 4,
            SpeechCategory::SituationBreak => 5,
            SpeechCategory::SituationLateNight => 6,
            SpeechCategory::SituationBattery => 7,
            SpeechCategory::SituationTodoFollow => 8,
            SpeechCategory::SituationRain => 9,
            SpeechCategory::RegularMorning => 10,
            SpeechCategory::RegularEvening => 11,
        }
    }

    /// payload の `category` / `governance_backoff:<key>` / feedback_speech の引数に使う識別子。
    pub fn as_str(self) -> &'static str {
        match self {
            SpeechCategory::Monologue => "monologue",
            SpeechCategory::Idle => "idle",
            SpeechCategory::Reminder => "reminder",
            SpeechCategory::Todo => "todo",
            SpeechCategory::Calendar => "calendar",
            SpeechCategory::SituationBreak => "situation_break",
            SpeechCategory::SituationLateNight => "situation_late_night",
            SpeechCategory::SituationBattery => "situation_battery",
            SpeechCategory::SituationTodoFollow => "situation_todo_follow",
            SpeechCategory::SituationRain => "situation_rain",
            SpeechCategory::RegularMorning => "regular_morning",
            SpeechCategory::RegularEvening => "regular_evening",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "monologue" => SpeechCategory::Monologue,
            "idle" => SpeechCategory::Idle,
            "reminder" => SpeechCategory::Reminder,
            "todo" => SpeechCategory::Todo,
            "calendar" => SpeechCategory::Calendar,
            "situation_break" => SpeechCategory::SituationBreak,
            "situation_late_night" => SpeechCategory::SituationLateNight,
            "situation_battery" => SpeechCategory::SituationBattery,
            "situation_todo_follow" => SpeechCategory::SituationTodoFollow,
            "situation_rain" => SpeechCategory::SituationRain,
            "regular_morning" => SpeechCategory::RegularMorning,
            "regular_evening" => SpeechCategory::RegularEvening,
            _ => return None,
        })
    }

    /// 段 4/5 (最低間隔・連投回避) と 🔕 の間隔延長 (backoff) の適用対象 (§4.2/§4.3)。
    /// Monologue/Idle は既存の間隔設定 (monologue_interval_min / idle 30 分) を維持する。
    /// Regular* は 1 日 1〜2 回の定例のため対象外 (spec §4.7.1 裁定)。
    pub fn is_situation(self) -> bool {
        matches!(
            self,
            SpeechCategory::SituationBreak
                | SpeechCategory::SituationLateNight
                | SpeechCategory::SituationBattery
                | SpeechCategory::SituationTodoFollow
                | SpeechCategory::SituationRain
        )
    }

    /// 🔕 フィードバック (backoff カウント + 3 回でトグル OFF) の対象 (M11、§4.2)。
    /// is_situation() (段 4/5 の間隔延長つき) に加え Regular* 2 種 (カウントのみ。
    /// 間隔延長は is_situation() ゲートの段 5 にしか無いので構造上自然にそうなる)。
    pub fn feedback_target(self) -> bool {
        self.is_situation()
            || matches!(self, SpeechCategory::RegularMorning | SpeechCategory::RegularEvening)
    }

    /// 🔕 で OFF に落とすカテゴリトグルを反転する。トグルを持つカテゴリなら true。
    pub fn disable_toggle(self, s: &mut Settings) -> bool {
        match self {
            SpeechCategory::SituationBreak => {
                s.situation_break_enabled = false;
                true
            }
            SpeechCategory::SituationLateNight => {
                s.situation_late_night_enabled = false;
                true
            }
            SpeechCategory::SituationBattery => {
                s.situation_battery_enabled = false;
                true
            }
            SpeechCategory::SituationTodoFollow => {
                s.todo_follow_enabled = false;
                true
            }
            SpeechCategory::SituationRain => {
                s.situation_rain_enabled = false;
                true
            }
            SpeechCategory::RegularMorning => {
                s.regular_morning_enabled = false;
                true
            }
            SpeechCategory::RegularEvening => {
                s.regular_evening_enabled = false;
                true
            }
            _ => false,
        }
    }

    /// 段 3: カテゴリ設定が有効か。
    /// Tier S 系は daily_support_enabled (マスタ) との AND。
    /// Todo/Calendar の個別設定は M8/M10 で追加され、それまではマスタのみ。
    /// 定例会話 2 枠も同じマスタと AND する (§4.1: 材料が Tier S データのため)。
    fn enabled(self, s: &Settings) -> bool {
        match self {
            SpeechCategory::Monologue => s.monologue_interval_min > 0,
            SpeechCategory::Idle => true,
            SpeechCategory::Reminder => s.daily_support_enabled && s.reminder_notify_enabled,
            SpeechCategory::Todo => s.daily_support_enabled,
            SpeechCategory::Calendar => s.daily_support_enabled,
            SpeechCategory::SituationBreak => s.daily_support_enabled && s.situation_break_enabled,
            SpeechCategory::SituationLateNight => {
                s.daily_support_enabled && s.situation_late_night_enabled
            }
            SpeechCategory::SituationBattery => {
                s.daily_support_enabled && s.situation_battery_enabled
            }
            SpeechCategory::SituationTodoFollow => {
                s.daily_support_enabled && s.todo_follow_enabled
            }
            SpeechCategory::SituationRain => s.daily_support_enabled && s.situation_rain_enabled,
            SpeechCategory::RegularMorning => {
                s.daily_support_enabled && s.regular_morning_enabled
            }
            SpeechCategory::RegularEvening => {
                s.daily_support_enabled && s.regular_evening_enabled
            }
        }
    }
}

/// 発話の優先度 (§4.2)。
/// Notice = 登録/予定した「必ず届く」通知 (リマインダー・カレンダー)。全段を越える。
/// Ambient = 気配り系 (独り言・idle・催促・状況発話)。全段を適用する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Notice,
    Ambient,
}

impl Priority {
    /// payload の `priority` に使う識別子 (M9)。
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Notice => "notice",
            Priority::Ambient => "ambient",
        }
    }
}

/// 段 5 連投回避の基準間隔 (Situation* 同カテゴリ、120 分)。backoff で線形に延びる。
/// 各カテゴリの検知側 (1 晩 1 回等) の二重の安全網 (§11.1-3 実装確定)。
pub const SITUATION_REPEAT_BASE_SECS: i64 = 120 * 60;
/// 🔕 がこの回数に達したらカテゴリトグル自体を OFF にする (§4.3)。
pub const BACKOFF_OFF_THRESHOLD: u32 = 3;

/// ガバナンスのインメモリ状態 (§4.2)。backoff のみ app_settings に永続化する。
pub struct GovernanceState {
    /// 最後に自発発話が到達 (Ghost|Toast) した unix 秒。カテゴリ横断。
    pub last_spoke: AtomicI64,
    /// カテゴリ別の最終到達 unix 秒。添字は `SpeechCategory::index()`。段 5 が参照。
    pub last_by_category: [AtomicI64; SpeechCategory::COUNT],
    /// 🔕 フィードバック回数 (カテゴリ別)。起動時に `load_backoff` で復元。
    pub backoff: [AtomicU32; SpeechCategory::COUNT],
    /// speech_id の連番 (M9、🔕 の誤適用防止)。
    pub speech_seq: AtomicU64,
    /// 最後にタグ付き発話を配達した (speech_id, カテゴリ)。feedback_speech が照合する。
    pub last_speech: Mutex<Option<(u64, SpeechCategory)>>,
}

impl Default for GovernanceState {
    fn default() -> Self {
        Self {
            last_spoke: AtomicI64::new(0),
            last_by_category: std::array::from_fn(|_| AtomicI64::new(0)),
            backoff: std::array::from_fn(|_| AtomicU32::new(0)),
            speech_seq: AtomicU64::new(0),
            last_speech: Mutex::new(None),
        }
    }
}

fn backoff_key(cat: SpeechCategory) -> String {
    format!("governance_backoff:{}", cat.as_str())
}

/// 起動時に app_settings から 🔕 backoff を復元する (main の setup で 1 回呼ぶ)。
pub fn load_backoff(state: &Arc<AppState>) {
    for i in 0..SpeechCategory::COUNT {
        let cat = ALL_CATEGORIES[i];
        if let Ok(Some(v)) = state.db.get_setting(&backoff_key(cat)) {
            if let Ok(n) = v.parse::<u32>() {
                state.governance.backoff[i].store(n, Ordering::SeqCst);
            }
        }
    }
}

const ALL_CATEGORIES: [SpeechCategory; SpeechCategory::COUNT] = [
    SpeechCategory::Monologue,
    SpeechCategory::Idle,
    SpeechCategory::Reminder,
    SpeechCategory::Todo,
    SpeechCategory::Calendar,
    SpeechCategory::SituationBreak,
    SpeechCategory::SituationLateNight,
    SpeechCategory::SituationBattery,
    SpeechCategory::SituationTodoFollow,
    SpeechCategory::SituationRain,
    SpeechCategory::RegularMorning,
    SpeechCategory::RegularEvening,
];

/// 🔕 フィードバックを 1 回記録する: backoff を +1 して永続化し、新しい回数を返す。
pub fn record_feedback(state: &Arc<AppState>, cat: SpeechCategory) -> u32 {
    let n = state.governance.backoff[cat.index()].fetch_add(1, Ordering::SeqCst) + 1;
    let _ = state.db.set_setting(&backoff_key(cat), &n.to_string());
    n
}

/// 🔕 backoff を 0 に戻す (カテゴリ再有効化時、reviewer 指摘)。
/// 恒久 throttle が再有効化後も残り、理由の見えないまま間隔が絞られるのを防ぐ。
pub fn reset_backoff(state: &Arc<AppState>, cat: SpeechCategory) {
    state.governance.backoff[cat.index()].store(0, Ordering::SeqCst);
    let _ = state.db.set_setting(&backoff_key(cat), "0");
}

/// OFF→ON に切り替わった feedback_target() カテゴリを返す (set_settings が backoff
/// リセットに使う)。対象は Situation*4 + SituationRain + Regular*2 = 7 (§4.2)。
/// M11 で `situation_reenabled` から改名・対象拡張 (旧名は Situation* 4 種のみだった)。
/// リセットしないと、🔕×3 で OFF になった枠を再有効化した直後の 🔕 1 回で
/// 即 OFF に落ちる (counter が 3 のまま残るため)。
pub fn feedback_reenabled(old: &Settings, new: &Settings) -> Vec<SpeechCategory> {
    let checks = [
        (
            old.situation_break_enabled,
            new.situation_break_enabled,
            SpeechCategory::SituationBreak,
        ),
        (
            old.situation_late_night_enabled,
            new.situation_late_night_enabled,
            SpeechCategory::SituationLateNight,
        ),
        (
            old.situation_battery_enabled,
            new.situation_battery_enabled,
            SpeechCategory::SituationBattery,
        ),
        (
            old.todo_follow_enabled,
            new.todo_follow_enabled,
            SpeechCategory::SituationTodoFollow,
        ),
        (
            old.situation_rain_enabled,
            new.situation_rain_enabled,
            SpeechCategory::SituationRain,
        ),
        (
            old.regular_morning_enabled,
            new.regular_morning_enabled,
            SpeechCategory::RegularMorning,
        ),
        (
            old.regular_evening_enabled,
            new.regular_evening_enabled,
            SpeechCategory::RegularEvening,
        ),
    ];
    checks
        .into_iter()
        .filter(|(was, now, _)| !was && *now)
        .map(|(_, _, cat)| cat)
        .collect()
}

/// 純粋判定 (副作用なし): 今このカテゴリの自発発話を配達してよいか。
/// `deliver::deliver_event` 以外から呼ばないこと (二重ゲート禁止)。
pub fn can_deliver(state: &Arc<AppState>, cat: SpeechCategory, prio: Priority) -> bool {
    // Notice は全段免除 (§4.2 判定表)。カテゴリ通知の on/off は発火元 (watcher) が
    // 機能スイッチとして判定する (例: reminder_notify_enabled は reminder watcher が見る)。
    if prio == Priority::Notice {
        return true;
    }
    // settings は 1 回スナップショットして即ロックを離す (Mutex 自己再ロック回避、§4.2)。
    // should_stay_quiet も内部で settings をロックするため、保持したまま呼ばない。
    let settings = state.settings.lock().expect("settings poisoned").clone();
    // 段 1: ハード静音 (集中 / 読み上げ / quiet_mode / フルスクリーン自動静音)
    if quiet::should_stay_quiet(state) {
        return false;
    }
    let now = chrono::Local::now();
    let minutes = now.time().hour() as u16 * 60 + now.time().minute() as u16;
    let last_spoke = state.governance.last_spoke.load(Ordering::SeqCst);
    let last_cat = state.governance.last_by_category[cat.index()].load(Ordering::SeqCst);
    let backoff = state.governance.backoff[cat.index()].load(Ordering::SeqCst);
    ambient_allowed(&settings, cat, minutes, last_spoke, last_cat, backoff, now.timestamp())
}

/// 段 2〜5 の純粋判定 (テスト対象)。段 1 (ハード静音) は呼び出し側で判定済みの前提。
fn ambient_allowed(
    s: &Settings,
    cat: SpeechCategory,
    minutes_of_day: u16,
    last_spoke_ts: i64,
    last_category_ts: i64,
    backoff: u32,
    now_ts: i64,
) -> bool {
    // 段 2: 夜間静音
    if night_quiet_active(s.night_quiet_enabled, s.night_quiet_from, s.night_quiet_to, minutes_of_day) {
        return false;
    }
    // 段 3: カテゴリ OFF
    if !cat.enabled(s) {
        return false;
    }
    // 段 4/5 は Situation* のみ (§4.2。Monologue/Idle は既存間隔を watcher 側が管理)
    if cat.is_situation() {
        // 段 4: 最低間隔 (カテゴリ横断の直近発話から)
        let min_secs = (s.min_speak_interval_min as i64) * 60;
        if min_secs > 0 && now_ts - last_spoke_ts < min_secs {
            return false;
        }
        // 段 5: 連投回避 (同カテゴリ 120 分 × (1+backoff)。🔕 で線形に延びる)
        let repeat_secs = SITUATION_REPEAT_BASE_SECS * (1 + backoff as i64);
        if now_ts - last_category_ts < repeat_secs {
            return false;
        }
    }
    true
}

/// 夜間静音帯の判定。from > to は日跨ぎ、from == to は終日 (§5)。
fn night_quiet_active(enabled: bool, from: u16, to: u16, minutes_of_day: u16) -> bool {
    if !enabled {
        return false;
    }
    if from == to {
        return true; // 終日
    }
    if from < to {
        from <= minutes_of_day && minutes_of_day < to
    } else {
        minutes_of_day >= from || minutes_of_day < to
    }
}

/// 配達成功 (Ghost|Toast) 後に呼ぶ: 最終発話時刻を更新する (間隔会計、§3.1)。
/// Deferred|Failed では呼ばない (空振りで間隔が狂わないように)。
/// `deliver::deliver_event` 以外から呼ばないこと。
pub fn record_delivered(state: &Arc<AppState>, cat: SpeechCategory, at_ts: i64) {
    state.governance.last_spoke.store(at_ts, Ordering::SeqCst);
    state.governance.last_by_category[cat.index()].store(at_ts, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        let mut s = Settings::default();
        // 既定: daily_support=true, reminder_notify=true, night_quiet=false,
        // monologue_interval_min=10, situation_* = false
        s.situation_break_enabled = true; // 段 4 テスト用に 1 カテゴリだけ有効化
        s
    }

    #[test]
    fn night_quiet_window_variants() {
        // 無効なら常に false
        assert!(!night_quiet_active(false, 1380, 420, 1400));
        // 通常帯 (10:00-12:00)
        assert!(night_quiet_active(true, 600, 720, 600));
        assert!(night_quiet_active(true, 600, 720, 719));
        assert!(!night_quiet_active(true, 600, 720, 720)); // 半開
        assert!(!night_quiet_active(true, 600, 720, 599));
        // 日跨ぎ (23:00-07:00)
        assert!(night_quiet_active(true, 1380, 420, 1380));
        assert!(night_quiet_active(true, 1380, 420, 0));
        assert!(night_quiet_active(true, 1380, 420, 419));
        assert!(!night_quiet_active(true, 1380, 420, 420));
        assert!(!night_quiet_active(true, 1380, 420, 720));
        // from == to は終日
        assert!(night_quiet_active(true, 300, 300, 0));
        assert!(night_quiet_active(true, 300, 300, 1439));
    }

    #[test]
    fn ambient_blocked_by_night_quiet() {
        let mut s = settings();
        s.night_quiet_enabled = true;
        s.night_quiet_from = 1380;
        s.night_quiet_to = 420;
        // 帯内 (0:00) はブロック、帯外 (12:00) は通る
        assert!(!ambient_allowed(&s, SpeechCategory::Monologue, 0, 0, 0, 0, 100_000));
        assert!(ambient_allowed(&s, SpeechCategory::Monologue, 720, 0, 0, 0, 100_000));
    }

    #[test]
    fn ambient_blocked_by_category_off() {
        let mut s = settings();
        // monologue_interval_min = 0 で Monologue カテゴリ OFF
        s.monologue_interval_min = 0;
        assert!(!ambient_allowed(&s, SpeechCategory::Monologue, 720, 0, 0, 0, 100_000));
        assert!(ambient_allowed(&s, SpeechCategory::Idle, 720, 0, 0, 0, 100_000));
        // situation はカテゴリフラグと daily_support の AND
        s.situation_break_enabled = false;
        assert!(!ambient_allowed(&s, SpeechCategory::SituationBreak, 720, 0, 0, 0, 0));
        s.situation_break_enabled = true;
        s.daily_support_enabled = false;
        assert!(!ambient_allowed(&s, SpeechCategory::SituationBreak, 720, 0, 0, 0, 0));
    }

    #[test]
    fn min_interval_applies_only_to_situation() {
        let s = settings(); // min_speak_interval_min = 30
        let last = 10_000;
        let within = last + 29 * 60;
        let after = last + 30 * 60;
        // Situation* は間隔内でブロック、経過後は通る (同カテゴリ実績なし = 段 5 は素通り)
        assert!(!ambient_allowed(&s, SpeechCategory::SituationBreak, 720, last, 0, 0, within));
        assert!(ambient_allowed(&s, SpeechCategory::SituationBreak, 720, last, 0, 0, after));
        // Monologue / Idle には適用しない (既存間隔は watcher 側が管理)
        assert!(ambient_allowed(&s, SpeechCategory::Monologue, 720, last, 0, 0, within));
        assert!(ambient_allowed(&s, SpeechCategory::Idle, 720, last, 0, 0, within));
    }

    #[test]
    fn repeat_window_applies_per_category_with_backoff() {
        let s = settings();
        let cat = SpeechCategory::SituationBreak;
        let last_cat = 100_000;
        // 同カテゴリ 120 分以内はブロック、経過後は通る (段 4 は last_spoke=0 で素通し)
        let within = last_cat + SITUATION_REPEAT_BASE_SECS - 1;
        let after = last_cat + SITUATION_REPEAT_BASE_SECS;
        assert!(!ambient_allowed(&s, cat, 720, 0, last_cat, 0, within));
        assert!(ambient_allowed(&s, cat, 720, 0, last_cat, 0, after));
        // backoff=1 で窓が 2 倍に延びる (線形、§4.3)
        assert!(!ambient_allowed(&s, cat, 720, 0, last_cat, 1, after));
        assert!(ambient_allowed(&s, cat, 720, 0, last_cat, 1, last_cat + SITUATION_REPEAT_BASE_SECS * 2));
        // 別カテゴリの直近発話は段 5 に影響しない (last_category_ts=0 なら通る)
        let mut s2 = settings();
        s2.situation_battery_enabled = true;
        assert!(ambient_allowed(&s2, SpeechCategory::SituationBattery, 720, 0, 0, 0, after));
    }

    #[test]
    fn feedback_reenabled_detects_off_to_on() {
        let old = Settings::default(); // Situation*/Regular* すべて false
        let mut new = old.clone();
        new.situation_break_enabled = true;
        new.todo_follow_enabled = true;
        new.situation_rain_enabled = true;
        new.regular_morning_enabled = true;
        let r = feedback_reenabled(&old, &new);
        assert_eq!(r.len(), 4);
        assert!(r.contains(&SpeechCategory::SituationBreak));
        assert!(r.contains(&SpeechCategory::SituationTodoFollow));
        assert!(r.contains(&SpeechCategory::SituationRain));
        assert!(r.contains(&SpeechCategory::RegularMorning));
        // ON→ON (据置) と ON→OFF は対象外
        let mut prev_on = Settings::default();
        prev_on.situation_break_enabled = true;
        assert!(feedback_reenabled(&prev_on, &prev_on).is_empty());
        assert!(feedback_reenabled(&prev_on, &Settings::default()).is_empty());
    }

    #[test]
    fn category_count_is_twelve() {
        assert_eq!(SpeechCategory::COUNT, 12);
        assert_eq!(ALL_CATEGORIES.len(), 12);
    }

    #[test]
    fn category_str_roundtrip_and_disable() {
        for cat in ALL_CATEGORIES {
            assert_eq!(SpeechCategory::parse(cat.as_str()), Some(cat));
        }
        assert_eq!(SpeechCategory::parse("unknown"), None);
        // Situation*/Regular* はトグルを持ち、🔕 で OFF に落ちる
        let mut s = Settings::default();
        s.situation_break_enabled = true;
        assert!(SpeechCategory::SituationBreak.disable_toggle(&mut s));
        assert!(!s.situation_break_enabled);
        let mut s2 = Settings::default();
        s2.todo_follow_enabled = true;
        assert!(SpeechCategory::SituationTodoFollow.disable_toggle(&mut s2));
        assert!(!s2.todo_follow_enabled);
        let mut s3 = Settings::default();
        s3.situation_rain_enabled = true;
        s3.regular_morning_enabled = true;
        s3.regular_evening_enabled = true;
        assert!(SpeechCategory::SituationRain.disable_toggle(&mut s3));
        assert!(!s3.situation_rain_enabled);
        assert!(SpeechCategory::RegularMorning.disable_toggle(&mut s3));
        assert!(!s3.regular_morning_enabled);
        assert!(SpeechCategory::RegularEvening.disable_toggle(&mut s3));
        assert!(!s3.regular_evening_enabled);
        assert!(!SpeechCategory::Monologue.disable_toggle(&mut Settings::default()));
    }

    #[test]
    fn feedback_target_covers_situation_and_regular_only() {
        for cat in [
            SpeechCategory::SituationBreak,
            SpeechCategory::SituationLateNight,
            SpeechCategory::SituationBattery,
            SpeechCategory::SituationTodoFollow,
            SpeechCategory::SituationRain,
            SpeechCategory::RegularMorning,
            SpeechCategory::RegularEvening,
        ] {
            assert!(cat.feedback_target(), "{cat:?} は feedback_target であるべき");
        }
        for cat in [
            SpeechCategory::Monologue,
            SpeechCategory::Idle,
            SpeechCategory::Reminder,
            SpeechCategory::Todo,
            SpeechCategory::Calendar,
        ] {
            assert!(!cat.feedback_target(), "{cat:?} は feedback_target ではないべき");
        }
    }

    #[test]
    fn regular_categories_are_not_situation() {
        // Regular* は間隔バックオフ (段 4/5) の対象外 (spec §4.7.1 裁定)
        assert!(!SpeechCategory::RegularMorning.is_situation());
        assert!(!SpeechCategory::RegularEvening.is_situation());
        assert!(SpeechCategory::SituationRain.is_situation());
    }
}

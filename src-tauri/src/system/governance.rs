//! 発話ガバナンス (M7、daily-support-design §4)。
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
//! | 5 連投回避 | 同カテゴリの連投 (Situation* のみ) | 免除 | (M9 で導入) |
//!
//! 段 5 のウィンドウ・回数は未決 (§11.1) のため M9 の状況発話実装と同時に入れる。
//! ガバナンス状態は DB 化せずインメモリ (`GovernanceState`、§2.4 で speech_log 撤回)。

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use chrono::Timelike;

use crate::presence::quiet;
use crate::state::{AppState, Settings};

/// 自発発話のカテゴリ (daily-support-design §4.2)。
/// M7 で発火するのは Monologue / Idle / Reminder。残りは設計 §4.2 判定表の契約の
/// 一部として M7 で定義済みで、発火元は M8 (Todo) / M9 (Situation*) / M10 (Calendar)
/// が結線する (それまで未構築の variant を許容)。
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
}

impl SpeechCategory {
    pub const COUNT: usize = 9;

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
        }
    }

    /// 段 4 (最低間隔) の適用対象 (Situation* のみ、§4.2)。
    /// Monologue/Idle は既存の間隔設定 (monologue_interval_min / idle 30 分) を維持する。
    fn is_situation(self) -> bool {
        matches!(
            self,
            SpeechCategory::SituationBreak
                | SpeechCategory::SituationLateNight
                | SpeechCategory::SituationBattery
                | SpeechCategory::SituationTodoFollow
        )
    }

    /// 段 3: カテゴリ設定が有効か。
    /// Tier S 系は daily_support_enabled (マスタ) との AND。
    /// Todo/Calendar の個別設定は M8/M10 で追加され、それまではマスタのみ。
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

/// ガバナンスのインメモリ状態 (§4.2。永続化しない)。
pub struct GovernanceState {
    /// 最後に自発発話が到達 (Ghost|Toast) した unix 秒。カテゴリ横断。
    pub last_spoke: AtomicI64,
    /// カテゴリ別の最終到達 unix 秒。添字は `SpeechCategory::index()`。
    /// M7 では記録のみ (段 5 連投回避と 🔕 フィードバックが M9 で参照する)。
    pub last_by_category: [AtomicI64; SpeechCategory::COUNT],
}

impl Default for GovernanceState {
    fn default() -> Self {
        Self {
            last_spoke: AtomicI64::new(0),
            last_by_category: std::array::from_fn(|_| AtomicI64::new(0)),
        }
    }
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
    ambient_allowed(&settings, cat, minutes, last_spoke, now.timestamp())
}

/// 段 2〜4 の純粋判定 (テスト対象)。段 1 (ハード静音) は呼び出し側で判定済みの前提。
fn ambient_allowed(
    s: &Settings,
    cat: SpeechCategory,
    minutes_of_day: u16,
    last_spoke_ts: i64,
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
    // 段 4: 最低間隔 (Situation* のみ)
    if cat.is_situation() {
        let min_secs = (s.min_speak_interval_min as i64) * 60;
        if min_secs > 0 && now_ts - last_spoke_ts < min_secs {
            return false;
        }
    }
    // 段 5 (連投回避) は M9 で導入 (ウィンドウ・回数が未決、設計 §11.1)。
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
        assert!(!ambient_allowed(&s, SpeechCategory::Monologue, 0, 0, 100_000));
        assert!(ambient_allowed(&s, SpeechCategory::Monologue, 720, 0, 100_000));
    }

    #[test]
    fn ambient_blocked_by_category_off() {
        let mut s = settings();
        // monologue_interval_min = 0 で Monologue カテゴリ OFF
        s.monologue_interval_min = 0;
        assert!(!ambient_allowed(&s, SpeechCategory::Monologue, 720, 0, 100_000));
        assert!(ambient_allowed(&s, SpeechCategory::Idle, 720, 0, 100_000));
        // situation はカテゴリフラグと daily_support の AND
        s.situation_break_enabled = false;
        assert!(!ambient_allowed(&s, SpeechCategory::SituationBreak, 720, 0, 0));
        s.situation_break_enabled = true;
        s.daily_support_enabled = false;
        assert!(!ambient_allowed(&s, SpeechCategory::SituationBreak, 720, 0, 0));
    }

    #[test]
    fn min_interval_applies_only_to_situation() {
        let s = settings(); // min_speak_interval_min = 30
        let last = 10_000;
        let within = last + 29 * 60;
        let after = last + 30 * 60;
        // Situation* は間隔内でブロック、経過後は通る
        assert!(!ambient_allowed(&s, SpeechCategory::SituationBreak, 720, last, within));
        assert!(ambient_allowed(&s, SpeechCategory::SituationBreak, 720, last, after));
        // Monologue / Idle には適用しない (既存間隔は watcher 側が管理)
        assert!(ambient_allowed(&s, SpeechCategory::Monologue, 720, last, within));
        assert!(ambient_allowed(&s, SpeechCategory::Idle, 720, last, within));
    }
}

//! OS レベルの状況検知 (M9、spec §4.6.3 / daily-support-design §7.3)。
//!
//! - `os_idle_secs`: GetLastInputInfo による OS 全体の無操作秒。
//!   既存の擬似アイドル (`DialogueState.last_interaction` = チャット送信のみ) とは
//!   別系統で、状況発話 (連続利用・深夜) はこちらを使う。idle 反応 §4.4.3 は従来のまま。
//! - `battery`: GetSystemPowerStatus によるバッテリー残量・AC 接続。非搭載機は None。
//! - 音量/ミュート検知 (`system_muted`) は見送り (§11.1-4、無くても成立)。
//!
//! 閾値・判定は純関数に切り出し (`update_session` / `break_due` / `night_key` /
//! `battery_transition`)、watcher (`tasks::spawn_context_watcher`) は OS 値の取得と
//! 配達だけを行う。閾値の確定値 (§11.1-2 実装確定):
//! アイドル境界 5 分 / 休憩促し 90 分ごと / 深夜帯 23:00-5:00 で 30 分利用・1 晩 1 回 /
//! バッテリー 15% 以下 (20% 超 or AC で解除)。

use chrono::{Duration, NaiveDate};

/// 連続利用セッションの境界とみなす無操作秒 (5 分)。
pub const IDLE_BOUNDARY_SECS: i64 = 5 * 60;
/// 休憩促しの連続利用閾値 (90 分)。2 回目以降も同じ間隔で繰り返す (90/180/270 分…)。
pub const BREAK_THRESHOLD_SECS: i64 = 90 * 60;
/// 深夜帯 (23:00-5:00) と、声かけに必要な連続利用秒 (30 分)。
pub const LATE_NIGHT_MIN_USE_SECS: i64 = 30 * 60;
/// バッテリー低下の通知閾値と解除閾値 (ヒステリシス)。
pub const BATTERY_LOW_PERCENT: u8 = 15;
pub const BATTERY_RESET_PERCENT: u8 = 20;
/// ToDo フォロー (未完了の思い出し) の時間帯: 14:00-18:00。
pub const TODO_FOLLOW_FROM_HOUR: u32 = 14;
pub const TODO_FOLLOW_TO_HOUR: u32 = 18;
/// ToDo 滞留 (再整理提案) の時間帯: 18:00-22:00 と、滞留とみなす日数。
pub const TODO_STALE_FROM_HOUR: u32 = 18;
pub const TODO_STALE_TO_HOUR: u32 = 22;
pub const TODO_STALE_DAYS: i64 = 3;

/// 連続利用セッションの 1 tick 分の更新 (純関数)。
/// `prev_start` は前回までのセッション開始 unix 秒 (0 = セッションなし)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionTick {
    /// 更新後のセッション開始 (0 = アイドル中でセッションなし)。
    pub session_start: i64,
    /// 現在の連続利用秒 (セッションなしなら 0)。
    pub continuous_secs: i64,
    /// この tick でセッションが切れた (アイドル境界を跨いだ) か。
    pub reset: bool,
}

pub fn update_session(prev_start: i64, idle_secs: i64, now_ts: i64) -> SessionTick {
    if idle_secs >= IDLE_BOUNDARY_SECS {
        return SessionTick {
            session_start: 0,
            continuous_secs: 0,
            reset: prev_start != 0,
        };
    }
    // 操作中: セッションが無ければ「最後の入力時点」から開始したとみなす
    let start = if prev_start == 0 {
        now_ts - idle_secs
    } else {
        prev_start
    };
    SessionTick {
        session_start: start,
        continuous_secs: (now_ts - start).max(0),
        reset: false,
    }
}

/// 休憩促しを出すべきか。`last_prompted_secs` はこのセッションで前回促した時点の
/// 連続利用秒 (0 = 未促し)。90 分ごとに繰り返す (連投は gate 段 5 が二重に抑える)。
pub fn break_due(continuous_secs: i64, last_prompted_secs: i64) -> bool {
    continuous_secs >= BREAK_THRESHOLD_SECS
        && continuous_secs - last_prompted_secs >= BREAK_THRESHOLD_SECS
}

/// 深夜帯 (23:00-5:00) か。
pub fn in_late_night(hour: u32) -> bool {
    hour >= 23 || hour < 5
}

/// 「同じ夜」を表す日付キー。0-5 時は前日の夜の続きとして前日日付に丸める
/// (23:30 と翌 0:30 で二重に声かけしないため)。正午前はすべて前日扱いで安全側。
pub fn night_key(date: NaiveDate, hour: u32) -> NaiveDate {
    if hour < 12 {
        date - Duration::days(1)
    } else {
        date
    }
}

/// バッテリー情報 (OS から取得)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatteryInfo {
    pub percent: u8,
    /// AC 電源に接続中か。
    pub ac: bool,
}

/// バッテリー低下通知の状態遷移 (純関数)。戻り値は (今 tick で通知するか, 新しい通知済みフラグ)。
/// 15% 以下かつ非 AC で 1 回だけ通知し、AC 接続 or 20% 超回復で再通知可能に戻す。
pub fn battery_transition(info: Option<BatteryInfo>, notified: bool) -> (bool, bool) {
    match info {
        None => (false, notified), // 情報が取れない (非搭載等) 間は状態維持
        Some(b) => {
            if b.ac || b.percent > BATTERY_RESET_PERCENT {
                (false, false)
            } else if !notified && b.percent <= BATTERY_LOW_PERCENT {
                (true, true)
            } else {
                (false, notified)
            }
        }
    }
}

// ===== OS 検知 (#[cfg(windows)]) =====

/// OS 全体の最終入力からの経過秒。取得失敗時は None (呼び出し側は tick をスキップ)。
#[cfg(windows)]
pub fn os_idle_secs() -> Option<i64> {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    unsafe {
        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if !GetLastInputInfo(&mut info).as_bool() {
            return None;
        }
        // GetTickCount は 49.7 日で u32 ラップする。wrapping_sub で差分は正しく出る。
        let elapsed_ms = GetTickCount().wrapping_sub(info.dwTime);
        Some((elapsed_ms / 1000) as i64)
    }
}

#[cfg(not(windows))]
pub fn os_idle_secs() -> Option<i64> {
    None
}

/// バッテリー残量と AC 接続。バッテリー非搭載 (デスクトップ) や取得失敗は None。
#[cfg(windows)]
pub fn battery() -> Option<BatteryInfo> {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    unsafe {
        let mut st = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut st).is_err() {
            return None;
        }
        // BatteryFlag: 128 = バッテリー非搭載、255 = 不明。BatteryLifePercent: 255 = 不明。
        if st.BatteryFlag == 128 || st.BatteryFlag == 255 || st.BatteryLifePercent == 255 {
            return None;
        }
        Some(BatteryInfo {
            percent: st.BatteryLifePercent,
            ac: st.ACLineStatus == 1,
        })
    }
}

#[cfg(not(windows))]
pub fn battery() -> Option<BatteryInfo> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn session_starts_from_last_input_and_accumulates() {
        // セッションなし + 操作中 (idle 10 秒) → 開始 = now - 10
        let t = update_session(0, 10, 1000);
        assert_eq!(t.session_start, 990);
        assert_eq!(t.continuous_secs, 10);
        assert!(!t.reset);
        // 次 tick: 継続
        let t2 = update_session(t.session_start, 30, 1060);
        assert_eq!(t2.session_start, 990);
        assert_eq!(t2.continuous_secs, 70);
    }

    #[test]
    fn session_resets_on_idle_boundary() {
        let t = update_session(990, IDLE_BOUNDARY_SECS, 2000);
        assert_eq!(t.session_start, 0);
        assert_eq!(t.continuous_secs, 0);
        assert!(t.reset);
        // 既にセッションなしのままアイドル継続 → reset は立てない
        let t2 = update_session(0, IDLE_BOUNDARY_SECS + 100, 2100);
        assert!(!t2.reset);
    }

    #[test]
    fn break_fires_every_threshold() {
        assert!(!break_due(BREAK_THRESHOLD_SECS - 1, 0));
        assert!(break_due(BREAK_THRESHOLD_SECS, 0));
        // 促し済み (90 分時点) → 179 分では出ない、180 分で再度
        assert!(!break_due(BREAK_THRESHOLD_SECS * 2 - 60, BREAK_THRESHOLD_SECS));
        assert!(break_due(BREAK_THRESHOLD_SECS * 2, BREAK_THRESHOLD_SECS));
    }

    #[test]
    fn late_night_band_and_key() {
        assert!(in_late_night(23));
        assert!(in_late_night(0));
        assert!(in_late_night(4));
        assert!(!in_late_night(5));
        assert!(!in_late_night(12));
        // 23 時はその日、翌 1 時は前日扱い → 同じ夜キー
        let d17 = NaiveDate::from_ymd_opt(2026, 7, 17).unwrap();
        let d18 = NaiveDate::from_ymd_opt(2026, 7, 18).unwrap();
        assert_eq!(night_key(d17, 23), d17);
        assert_eq!(night_key(d18, 1), d17);
    }

    #[test]
    fn battery_hysteresis() {
        let low = |p| Some(BatteryInfo { percent: p, ac: false });
        // 15% 以下 & 非 AC & 未通知 → 通知
        assert_eq!(battery_transition(low(15), false), (true, true));
        // 通知済みのまま低下継続 → 再通知しない
        assert_eq!(battery_transition(low(10), true), (false, true));
        // 16-20% は通知も解除もしない (ヒステリシス帯)
        assert_eq!(battery_transition(low(18), true), (false, true));
        assert_eq!(battery_transition(low(18), false), (false, false));
        // 20% 超回復 or AC 接続で解除
        assert_eq!(battery_transition(low(21), true), (false, false));
        assert_eq!(
            battery_transition(Some(BatteryInfo { percent: 10, ac: true }), true),
            (false, false)
        );
        // 情報なしは状態維持
        assert_eq!(battery_transition(None, true), (false, true));
    }
}

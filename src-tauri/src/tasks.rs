//! バックグラウンドの自発挙動タスク (spec §4.4.3 / §4.4.4 / §4.6.1 / §4.6.2)。
//!
//! - ランダムトーク: monologue_interval_min 分ごとに独り言。0 で無効。
//! - 放置監視: 60 秒チェック、最終操作から 30 分で 1 回 idle。
//! - リマインダー watcher: 10 秒間隔で発火・起動時回収 (M7)。
//! - daily watcher: 日課の復活 (日付変更検知) + 朝の ToDo 件数告知 (M8)。
//! - context watcher: OS 状況検知 → 状況発話 4 カテゴリ (M9、spec §4.6.3)。
//! - Irodori サイドカーのアイドル監視: 60 秒チェック、最終使用から 5 分で自動 shutdown (M4c Phase E)。
//!
//! M7 以降、自発発話はすべて `system::deliver::deliver_event` を通す
//! (静音・夜間静音・busy 直列化はその中の単一ゲートが判定する。呼び出し側は
//! `should_stay_quiet` や busy を直接見ない。daily-support-design §3/§4)。

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tauri::{AppHandle, Emitter};

use crate::db::{ReminderKind, ReminderRow};
use crate::presence::{context, idle};
use crate::state::AppState;
use crate::system::deliver::{self, DeliveryOutcome};
use crate::system::governance::{Priority, SpeechCategory};
use crate::tools::reminder::next_recurring_due_ts;

/// ランダムトークタスク。1 分ごとに「前回発話からの経過 >= 設定間隔」を判定。
/// 間隔は従来どおり本タスクが管理し、静音系の可否は deliver 内のゲートに委ねる。
pub fn spawn_random_talk(app: AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let mut last_talk = Utc::now().timestamp();
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;

            let interval_min = {
                let s = state.settings.lock().expect("settings poisoned");
                s.monologue_interval_min
            };
            if interval_min == 0 {
                // 無効: タイマーをリセットしておく (有効化後すぐ撃たないように)
                last_talk = Utc::now().timestamp();
                continue;
            }
            let now = Utc::now().timestamp();
            if now - last_talk < (interval_min as i64) * 60 {
                continue;
            }
            // 静音・busy なら Deferred が返り持ち越し (last_talk を進めない)
            let outcome = deliver::deliver_event(
                &app,
                &state,
                SpeechCategory::Monologue,
                Priority::Ambient,
                "monologue",
                &[],
                None,
            )
            .await;
            match outcome {
                DeliveryOutcome::Ghost => last_talk = now,
                // 辞書に monologue が無い等の Failed は次の間隔まで再試行しない
                // (毎分の空振り辞書引きを防ぐ)。Deferred (静音・busy) は持ち越し。
                DeliveryOutcome::Failed => last_talk = now,
                DeliveryOutcome::Toast | DeliveryOutcome::Deferred => {}
            }
        }
    });
}

/// 放置監視タスク。60 秒ごとにチェックし、30 分無操作で 1 回だけ idle を発火。
pub fn spawn_idle_watcher(app: AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;

            let now = Utc::now().timestamp();
            if !idle::idle_due(&state, now) {
                continue;
            }
            // この放置期間で既に撃っていればスキップ
            if state.presence.idle_fired.load(Ordering::SeqCst) {
                continue;
            }
            // 静音・busy なら Deferred (idle_fired を立てない → 次のチェックで再挑戦)
            let outcome = deliver::deliver_event(
                &app,
                &state,
                SpeechCategory::Idle,
                Priority::Ambient,
                "idle",
                &[],
                None,
            )
            .await;
            if outcome == DeliveryOutcome::Ghost {
                state.presence.idle_fired.store(true, Ordering::SeqCst);
            }
        }
    });
}

/// Irodori サイドカーのアイドル監視 (M4c Phase E, architecture §8.4)。
/// 60 秒ごとに `IrodoriClient` の `last_used` を確認し、5 分以上未使用なら shutdown する。
/// メモリ常駐の Python + torch + モデルが大きいので、放置中は積極的に解放する。
pub fn spawn_irodori_idle_watcher(state: Arc<AppState>) {
    const CHECK_INTERVAL_SECS: u64 = 60;
    const IDLE_THRESHOLD_SECS: i64 = 5 * 60;
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(CHECK_INTERVAL_SECS)).await;
            let now = Utc::now().timestamp();
            // 失敗は無視 (次のティックで再試行)
            let _ = state
                .tts
                .irodori
                .shutdown_if_idle(now, IDLE_THRESHOLD_SECS)
                .await;
        }
    });
}

/// M7 (spec §4.6.1): リマインダー watcher。
/// 10 秒間隔で `due_active_reminders(now)` をポーリングし、`deliver_event` (Notice、
/// ハード静音・夜間静音を越える) で配達する。**発火 ≠ 完了** (daily-support-design §2.1):
/// - 到達 (Ghost|Toast) → `reminder_log` に記録し、once は active=0 (未完了のまま停止)、
///   繰り返しは次回 due へ reschedule。
/// - 未達 (Deferred|Failed) → active を維持し次ポーリングで再試行 (ログは残さない。
///   静音中に 10 秒ごとの再試行が積もって reminder_log を押し流すのを防ぐ)。
/// 起動時は一度だけ回収パスを実行し、アプリ停止中に過ぎた期限を拾う (複数は集約)。
pub fn spawn_reminder_watcher(app: AppHandle, state: Arc<AppState>) {
    const CHECK_INTERVAL_SECS: u64 = 10;
    /// フロント (webview) の準備を待ってから回収する。起動直後に emit しても
    /// リスナー不在で発話が消えるため (update watcher の起動遅延と同じ手当)。
    const BOOT_RECOVER_DELAY_SECS: u64 = 20;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(BOOT_RECOVER_DELAY_SECS)).await;
        recover_overdue_on_boot(&app, &state).await;
        loop {
            tokio::time::sleep(Duration::from_secs(CHECK_INTERVAL_SECS)).await;
            if !reminder_notify_allowed(&state) {
                // 機能スイッチ OFF 中は発火を保留する (期限は消えず、ON に戻すと届く)
                continue;
            }
            let now = Utc::now().timestamp();
            let due = match state.db.due_active_reminders(now) {
                Ok(v) => v,
                Err(err) => {
                    eprintln!("[reminder] due_active_reminders failed: {err:#}");
                    continue;
                }
            };
            for r in due {
                let outcome = fire_reminder(&app, &state, &r).await;
                if outcome.reached() {
                    advance_after_delivery(&app, &state, &r, now, outcome);
                }
            }
        }
    });
}

/// 発火通知の機能スイッチ (設定)。gate のカテゴリ判定とは別で、Notice が
/// ゲート全段免除でも「通知そのものを止めたい」意思をここで尊重する。
fn reminder_notify_allowed(state: &Arc<AppState>) -> bool {
    let s = state.settings.lock().expect("settings poisoned");
    s.daily_support_enabled && s.reminder_notify_enabled
}

async fn fire_reminder(
    app: &AppHandle,
    state: &Arc<AppState>,
    r: &ReminderRow,
) -> DeliveryOutcome {
    let body = if r.text.is_empty() {
        "リマインダー".to_string()
    } else {
        r.text.clone()
    };
    let fallback = format!("リマインダー: {body}");
    deliver::deliver_event(
        app,
        state,
        SpeechCategory::Reminder,
        Priority::Notice,
        "reminder_fired",
        &[("body", body.as_str())],
        Some(fallback),
    )
    .await
}

/// 到達後の状態遷移 (§7.1): ログ記録 → once は停止 / 繰り返しは次回へ。
fn advance_after_delivery(
    app: &AppHandle,
    state: &Arc<AppState>,
    r: &ReminderRow,
    now: i64,
    outcome: DeliveryOutcome,
) {
    const LOG_KEEP: u32 = 500;
    if let Err(err) = state.db.log_fire(r.id, now, outcome.as_str()) {
        eprintln!("[reminder] log_fire({}) failed: {err:#}", r.id);
    }
    match r.kind {
        ReminderKind::Once => {
            if let Err(err) = state.db.deactivate_reminder(r.id) {
                eprintln!("[reminder] deactivate({}) failed: {err:#}", r.id);
            }
        }
        ReminderKind::Daily | ReminderKind::Weekly => {
            let now_local = chrono::Local::now().naive_local();
            match next_recurring_due_ts(r.kind, r.weekday_mask, r.time_of_day, now_local) {
                Some(next_due) => {
                    if let Err(err) = state.db.reschedule_reminder(r.id, next_due) {
                        eprintln!("[reminder] reschedule({}) failed: {err:#}", r.id);
                    }
                }
                // weekly で mask=0 等、次回が計算できない行は無限再発火を避けて停止
                None => {
                    if let Err(err) = state.db.deactivate_reminder(r.id) {
                        eprintln!("[reminder] deactivate({}) failed: {err:#}", r.id);
                    }
                }
            }
        }
    }
    if let Err(err) = state.db.prune_reminder_log(LOG_KEEP) {
        eprintln!("[reminder] prune_reminder_log failed: {err:#}");
    }
    let _ = app.emit("reminders-changed", ());
}

/// 起動時回収 (§7.1-5): アプリ停止中に過ぎた期限を拾う。
/// 2 件以上溜まっていたら、大量の連続発話を避けるため直近 1 件に集約して 1 回だけ
/// 通知し、状態遷移 (ログ・停止/再スケジュール) は全件に適用する。
async fn recover_overdue_on_boot(app: &AppHandle, state: &Arc<AppState>) {
    if !reminder_notify_allowed(state) {
        return;
    }
    let now = Utc::now().timestamp();
    let due = match state.db.due_active_reminders(now) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("[reminder] boot recover query failed: {err:#}");
            return;
        }
    };
    if due.is_empty() {
        return;
    }
    if due.len() == 1 {
        let r = &due[0];
        let outcome = fire_reminder(app, state, r).await;
        if outcome.reached() {
            advance_after_delivery(app, state, r, now, outcome);
        }
        return;
    }
    // due_ts ASC で返るので末尾が直近。「『X』ほか N-1 件」に集約する。
    let latest = due.last().expect("non-empty");
    let body = aggregate_overdue_body(latest, due.len());
    let fallback = format!("リマインダー: {body}");
    let outcome = deliver::deliver_event(
        app,
        state,
        SpeechCategory::Reminder,
        Priority::Notice,
        "reminder_fired",
        &[("body", body.as_str())],
        Some(fallback),
    )
    .await;
    if outcome.reached() {
        for r in &due {
            advance_after_delivery(app, state, r, now, outcome);
        }
    }
    // 未達なら何もしない: active のまま残り、通常ループが個別に再試行する
}

/// M9 (spec §4.6.3): context watcher。OS 状況検知 → 状況発話 4 カテゴリ。
/// 60 秒間隔で連続利用セッションを計測し、条件を満たしたカテゴリを
/// `deliver_event` (Ambient・ゲート下) で配達する。判定は `presence::context` の
/// 純関数群、閾値も同モジュールの定数 (§11.1-2 実装確定)。
/// - 休憩促し: 連続利用 90 分ごと (セッション内で繰り返し)
/// - 深夜利用: 23-5 時に 30 分以上利用、1 晩 1 回 (`situation_late_night_date`)
/// - バッテリー低下: 15% 以下かつ非 AC、1 回 (AC or 20% 超回復で解除)
/// - ToDo フォロー: 14-18 時・1 日 1 回、today に未完了があれば思い出し
/// - ToDo 滞留: 18-22 時・1 日 1 回、today に 3 日以上残っていれば再整理提案
/// per-day dedup は app_settings に永続化 (todo_morning_date と同型)。
/// 到達 (Ghost) と辞書なし (Failed) は消化し、Deferred (静音・busy) は次 tick 再挑戦。
pub fn spawn_context_watcher(app: AppHandle, state: Arc<AppState>) {
    const CHECK_INTERVAL_SECS: u64 = 60;
    /// 他 watcher (リマインダー回収 20s / daily 30s) と起動タイミングをずらす。
    const BOOT_DELAY_SECS: u64 = 45;
    const LATE_NIGHT_DATE_KEY: &str = "situation_late_night_date";
    const TODO_FOLLOW_DATE_KEY: &str = "todo_follow_date";
    const TODO_STALE_DATE_KEY: &str = "todo_stale_date";
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(BOOT_DELAY_SECS)).await;
        loop {
            let settings = state.settings.lock().expect("settings poisoned").clone();
            if !settings.daily_support_enabled {
                tokio::time::sleep(Duration::from_secs(CHECK_INTERVAL_SECS)).await;
                continue;
            }

            // 連続利用セッションの更新 (OS アイドルが取れない環境では 0 のまま)
            let now = Utc::now().timestamp();
            let continuous = match context::os_idle_secs() {
                Some(idle_secs) => {
                    let prev = state.context.session_start.load(Ordering::SeqCst);
                    let tick = context::update_session(prev, idle_secs, now);
                    state
                        .context
                        .session_start
                        .store(tick.session_start, Ordering::SeqCst);
                    if tick.reset {
                        state.context.break_prompted_secs.store(0, Ordering::SeqCst);
                    }
                    tick.continuous_secs
                }
                None => 0,
            };
            let now_local = chrono::Local::now();
            let hour = {
                use chrono::Timelike;
                now_local.hour()
            };
            let today = now_local.date_naive();

            // 1) 休憩促し (90 分ごと)
            if settings.situation_break_enabled {
                let prompted = state.context.break_prompted_secs.load(Ordering::SeqCst);
                if context::break_due(continuous, prompted) {
                    let time_label = format!("{}分", continuous / 60);
                    let outcome = deliver::deliver_event(
                        &app,
                        &state,
                        SpeechCategory::SituationBreak,
                        Priority::Ambient,
                        "situation_break",
                        &[("time", time_label.as_str())],
                        None,
                    )
                    .await;
                    // Ghost=促した / Failed=辞書なし → このセッション分は消化。
                    // Deferred (静音・busy) は prompted を進めず次 tick 再挑戦。
                    if matches!(outcome, DeliveryOutcome::Ghost | DeliveryOutcome::Failed) {
                        state
                            .context
                            .break_prompted_secs
                            .store(continuous, Ordering::SeqCst);
                    }
                }
            }

            // 2) 深夜利用の声かけ (1 晩 1 回)
            if settings.situation_late_night_enabled
                && context::in_late_night(hour)
                && continuous >= context::LATE_NIGHT_MIN_USE_SECS
            {
                let key = context::night_key(today, hour).to_string();
                let announced = state
                    .db
                    .get_setting(LATE_NIGHT_DATE_KEY)
                    .ok()
                    .flatten()
                    .map(|v| v == key)
                    .unwrap_or(false);
                if !announced {
                    let outcome = deliver::deliver_event(
                        &app,
                        &state,
                        SpeechCategory::SituationLateNight,
                        Priority::Ambient,
                        "situation_late_night",
                        &[],
                        None,
                    )
                    .await;
                    if matches!(outcome, DeliveryOutcome::Ghost | DeliveryOutcome::Failed) {
                        let _ = state.db.set_setting(LATE_NIGHT_DATE_KEY, &key);
                    }
                }
            }

            // 3) バッテリー低下 (1 回、AC/回復で解除)
            if settings.situation_battery_enabled {
                let info = context::battery();
                let notified = state.context.battery_notified.load(Ordering::SeqCst);
                let (should_notify, new_flag) = context::battery_transition(info, notified);
                if should_notify {
                    let percent = info.map(|b| b.percent).unwrap_or(0).to_string();
                    let outcome = deliver::deliver_event(
                        &app,
                        &state,
                        SpeechCategory::SituationBattery,
                        Priority::Ambient,
                        "situation_battery",
                        &[("count", percent.as_str())],
                        None,
                    )
                    .await;
                    // 未達 (Deferred) はフラグを立てず次 tick で再挑戦
                    if matches!(outcome, DeliveryOutcome::Ghost | DeliveryOutcome::Failed) {
                        state.context.battery_notified.store(true, Ordering::SeqCst);
                    }
                } else {
                    state.context.battery_notified.store(new_flag, Ordering::SeqCst);
                }
            }

            // 4) ToDo フォロー (14-18 時・1 日 1 回) / 5) ToDo 滞留 (18-22 時・1 日 1 回)
            if settings.todo_follow_enabled {
                if (context::TODO_FOLLOW_FROM_HOUR..context::TODO_FOLLOW_TO_HOUR).contains(&hour) {
                    follow_open_todos(&app, &state, TODO_FOLLOW_DATE_KEY, &today, FollowKind::Follow)
                        .await;
                }
                if (context::TODO_STALE_FROM_HOUR..context::TODO_STALE_TO_HOUR).contains(&hour) {
                    follow_open_todos(&app, &state, TODO_STALE_DATE_KEY, &today, FollowKind::Stale)
                        .await;
                }
            }

            tokio::time::sleep(Duration::from_secs(CHECK_INTERVAL_SECS)).await;
        }
    });
}

#[derive(Clone, Copy, PartialEq)]
enum FollowKind {
    /// 未完了の思い出し (todo_follow)。
    Follow,
    /// 3 日以上滞留の再整理提案 (todo_stale)。
    Stale,
}

/// ToDo フォロー/滞留の共通処理: 1 日 1 回、today バケットの open な対象があれば
/// 辞書キーで配達する。対象が無い日は発話なしで消化する (午後に追加された分で鳴らさない)。
async fn follow_open_todos(
    app: &AppHandle,
    state: &Arc<AppState>,
    date_key: &str,
    today: &chrono::NaiveDate,
    kind: FollowKind,
) {
    let done = state
        .db
        .get_setting(date_key)
        .ok()
        .flatten()
        .map(|v| v == today.to_string())
        .unwrap_or(false);
    if done {
        return;
    }
    let list = match state.db.list_todos(Some("today")) {
        Ok(v) => v,
        Err(err) => {
            // 取得失敗は消化せず次 tick で再試行
            eprintln!("[context] list_todos failed: {err:#}");
            return;
        }
    };
    let now = Utc::now().timestamp();
    let target = list.iter().find(|t| {
        t.status == "open"
            && match kind {
                FollowKind::Follow => true,
                FollowKind::Stale => now - t.created_ts >= context::TODO_STALE_DAYS * 86_400,
            }
    });
    let Some(target) = target else {
        // 対象なし → 今日は消化
        let _ = state.db.set_setting(date_key, &today.to_string());
        return;
    };
    let dict_key = match kind {
        FollowKind::Follow => "todo_follow",
        FollowKind::Stale => "todo_stale",
    };
    let outcome = deliver::deliver_event(
        app,
        state,
        SpeechCategory::SituationTodoFollow,
        Priority::Ambient,
        dict_key,
        &[("body", target.text.as_str())],
        None,
    )
    .await;
    if matches!(outcome, DeliveryOutcome::Ghost | DeliveryOutcome::Failed) {
        let _ = state.db.set_setting(date_key, &today.to_string());
    }
}

/// M10 (spec §4.6.4): カレンダー watcher。
/// 既定 30 分間隔で全 ICS ソースを取得して calendar_cache へ反映し、開始前通知を配達する。
/// 通知は `calendar_notify_min` 分前（終日は当日ローカル 8:00）に達したら
/// `calendar_upcoming`（Notice、静音を越える）で 1 回だけ。到達で notified=1。
/// カレンダーは既定オフ（ソース未設定なら何もしない）。取得失敗は既存キャッシュ維持。
pub fn spawn_calendar_watcher(app: AppHandle, state: Arc<AppState>) {
    const CHECK_INTERVAL_SECS: u64 = 60;
    const FETCH_INTERVAL_SECS: i64 = 30 * 60;
    const BOOT_DELAY_SECS: u64 = 25;
    /// near-term 展開の表示窓（今日〜7 日）。
    const DISPLAY_DAYS: i64 = 7;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(BOOT_DELAY_SECS)).await;
        let mut last_fetch: i64 = 0;
        loop {
            // 他の Tier S watcher と同様、マスタスイッチ OFF 中は取得も通知もしない
            // (Notice は gate 段 3 を素通りするため、機能スイッチはここで見る。M10 reviewer 指摘)
            let has_sources = {
                let s = state.settings.lock().expect("settings poisoned");
                s.daily_support_enabled && !s.calendar_sources.is_empty()
            };
            if has_sources {
                let now = Utc::now().timestamp();
                // 定期取得（起動直後 + FETCH_INTERVAL ごと）
                if now - last_fetch >= FETCH_INTERVAL_SECS {
                    let n = fetch_all_calendars(&state, DISPLAY_DAYS).await;
                    last_fetch = now;
                    if n > 0 {
                        let _ = app.emit("calendar-changed", ());
                    }
                    // prune（前日より前の過去発生行）
                    let cutoff = now - 86_400;
                    if let Err(err) = state.db.prune_calendar(cutoff) {
                        eprintln!("[calendar] prune failed: {err:#}");
                    }
                }
                // 開始前通知（毎 tick 判定）
                notify_upcoming(&app, &state).await;
            }
            tokio::time::sleep(Duration::from_secs(CHECK_INTERVAL_SECS)).await;
        }
    });
}

/// 全ソースを取得して calendar_cache に反映する。取得件数の合計を返す。
/// ソース失敗は個別にログして続行（1 ソースの障害で他を巻き込まない）。
async fn fetch_all_calendars(state: &Arc<AppState>, display_days: i64) -> usize {
    let sources = state
        .settings
        .lock()
        .expect("settings poisoned")
        .calendar_sources
        .clone();
    let mut total = 0;
    for (idx, source) in sources.iter().enumerate() {
        match crate::system::calendar::fetch_source_into_cache(state, idx as i64, source, display_days)
            .await
        {
            Ok(n) => total += n,
            Err(err) => eprintln!("[calendar] source {idx} fetch failed: {err:#}"),
        }
    }
    total
}

/// refresh_calendar コマンドの実体（表示窓は watcher と同じ 7 日）。
pub async fn refresh_all_calendars(state: &Arc<AppState>) -> usize {
    fetch_all_calendars(state, 7).await
}

/// 開始前通知の対象を配達する。notify_at（時刻付き=start-notify_min、終日=当日 8:00）を
/// 過ぎ、まだ start 前で未通知の予定を `calendar_upcoming` で 1 回配達する。
async fn notify_upcoming(app: &AppHandle, state: &Arc<AppState>) {
    use crate::system::calendar;
    let notify_min = state
        .settings
        .lock()
        .expect("settings poisoned")
        .calendar_notify_min as i64;
    let now = Utc::now().timestamp();
    // 候補は「これから始まる未通知の予定」を広めに取り、notify_at を個別判定する
    let horizon = now + 2 * 86_400;
    let candidates = match state.db.upcoming_calendar(now, horizon) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("[calendar] upcoming query failed: {err:#}");
            return;
        }
    };
    for ev in candidates {
        let notify_at = if ev.all_day {
            calendar::all_day_notify_ts(ev.start_ts)
        } else {
            ev.start_ts - notify_min * 60
        };
        if now < notify_at {
            continue; // まだ通知時刻前
        }
        let time_label = calendar::time_label(ev.start_ts, ev.all_day);
        let fallback = format!("まもなく予定: {}（{}）", ev.summary, time_label);
        let outcome = deliver::deliver_event(
            app,
            state,
            SpeechCategory::Calendar,
            Priority::Notice,
            "calendar_upcoming",
            &[("summary", ev.summary.as_str()), ("time", time_label.as_str())],
            Some(fallback),
        )
        .await;
        if outcome.reached() {
            if let Err(err) = state.db.mark_calendar_notified(ev.source_id, &ev.uid, ev.start_ts) {
                eprintln!("[calendar] mark_notified failed: {err:#}");
            }
            let _ = app.emit("calendar-changed", ());
        }
        // 未達（Deferred/Failed）は notified を立てず次 tick で再試行
    }
}

/// 集約通知の本文。辞書側の「『{body}』の時間だよ」等に埋め込まれる前提で名詞句にする。
fn aggregate_overdue_body(latest: &ReminderRow, total: usize) -> String {
    let name = if latest.text.is_empty() {
        "リマインダー".to_string()
    } else {
        latest.text.clone()
    };
    format!("{name}（ほか{}件）", total - 1)
}

/// M8 (spec §4.6.2): daily watcher。
/// - 日課の復活: 起動時 + ローカル日付の変更検知で `reset_recurring` (冪等) を実行し、
///   復活があれば `todos-changed` を emit。
/// - 朝の件数告知: 朝の時間帯 (5:00-11:00、辞書 boot の朝帯と同じ) に 1 日 1 回、
///   today バケットの未完了が 1 件以上なら `todo_morning` を配達 (Ambient・ゲート下)。
///   告知済みのローカル日付は app_settings に保持し、再起動で二重告知しない。
/// リマインダー watcher とは分離する (reminder_notify_enabled と結合させない)。
pub fn spawn_daily_watcher(app: AppHandle, state: Arc<AppState>) {
    const CHECK_INTERVAL_SECS: u64 = 60;
    /// フロント準備待ち。リマインダーの起動時回収 (20 秒) より後にして起動直後の連続発話を避ける
    /// (busy 直列化があるので同時でも壊れないが、間隔を空けたほうが読みやすい)。
    const BOOT_DELAY_SECS: u64 = 30;
    const MORNING_FROM_HOUR: u32 = 5;
    const MORNING_TO_HOUR: u32 = 11;
    const MORNING_DATE_KEY: &str = "todo_morning_date";
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(BOOT_DELAY_SECS)).await;
        let mut last_date: Option<chrono::NaiveDate> = None;
        loop {
            let now_local = chrono::Local::now();
            let today = now_local.date_naive();

            // 1) 日課の復活 (起動直後の初回 tick + 日付変更時)。
            //    DB エラー時は last_date を進めず次 tick で再試行する (失敗を消化しない)。
            if last_date != Some(today) {
                match crate::tools::todo::reset_recurring(&state) {
                    Ok(n) => {
                        if n > 0 {
                            let _ = app.emit("todos-changed", ());
                        }
                        last_date = Some(today);
                    }
                    Err(err) => eprintln!("[daily] reset_recurring failed: {err:#}"),
                }
            }

            // 2) 朝の件数告知 (1 日 1 回・朝帯のみ・daily_support 有効時)
            let hour = {
                use chrono::Timelike;
                now_local.hour()
            };
            let daily_enabled = state
                .settings
                .lock()
                .expect("settings poisoned")
                .daily_support_enabled;
            if daily_enabled && (MORNING_FROM_HOUR..MORNING_TO_HOUR).contains(&hour) {
                let announced = state
                    .db
                    .get_setting(MORNING_DATE_KEY)
                    .ok()
                    .flatten()
                    .map(|v| v == today.to_string())
                    .unwrap_or(false);
                if !announced {
                    // 件数取得の失敗は消化せず次 tick で再試行 (「0 件の朝」と混同しない)
                    match state.db.count_open_todos(Some("today")) {
                        Err(err) => eprintln!("[daily] count_open_todos failed: {err:#}"),
                        Ok(0) => {
                            // 0 件の朝は告知なしで消化 (昼に追加された分で鳴らさない)
                            let _ = state.db.set_setting(MORNING_DATE_KEY, &today.to_string());
                        }
                        Ok(count) => {
                            let count_str = count.to_string();
                            let outcome = deliver::deliver_event(
                                &app,
                                &state,
                                SpeechCategory::Todo,
                                Priority::Ambient,
                                "todo_morning",
                                &[("count", count_str.as_str())],
                                None,
                            )
                            .await;
                            match outcome {
                                // 発話できた or 辞書が無い (今日はもう試さない) → 告知済みに
                                DeliveryOutcome::Ghost | DeliveryOutcome::Failed => {
                                    let _ =
                                        state.db.set_setting(MORNING_DATE_KEY, &today.to_string());
                                }
                                // 静音・busy は later tick で再挑戦
                                DeliveryOutcome::Toast | DeliveryOutcome::Deferred => {}
                            }
                        }
                    }
                }
            }

            tokio::time::sleep(Duration::from_secs(CHECK_INTERVAL_SECS)).await;
        }
    });
}

/// M5-C: `topics_enabled` が ON の間、1 時間おきに enabled な interest_topics の RSS を取得して
/// topics_cache に蓄積する。topics_enabled=false の周期は何もしない (キャッシュ取得しない)。
pub fn spawn_topics_watcher(state: Arc<AppState>) {
    const PERIOD_SECS: u64 = 60 * 60;
    const FIRST_DELAY_SECS: u64 = 60;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(FIRST_DELAY_SECS)).await;
        loop {
            let enabled = state.settings.lock().expect("settings poisoned").topics_enabled;
            if enabled {
                if let Err(err) = crate::system::topics::fetch_all_into_cache(&state).await {
                    eprintln!("[topics] fetch failed: {err:#}");
                }
            }
            tokio::time::sleep(Duration::from_secs(PERIOD_SECS)).await;
        }
    });
}

/// M5-D: 起動時に 1 回 + 24 時間ごとに更新フィードを確認する。
/// `update_feed_url` が未設定なら check 内で no-op。
pub fn spawn_update_watcher(app: AppHandle, state: Arc<AppState>) {
    const ONCE_DELAY_SECS: u64 = 30;
    const PERIOD_SECS: u64 = 24 * 60 * 60;
    tauri::async_runtime::spawn(async move {
        // 起動直後は他の boot 処理と被らせないため少し待つ
        tokio::time::sleep(Duration::from_secs(ONCE_DELAY_SECS)).await;
        loop {
            if let Err(err) = crate::system::update::check_update_once(&app, &state).await {
                eprintln!("[update] check failed: {err:#}");
            }
            tokio::time::sleep(Duration::from_secs(PERIOD_SECS)).await;
        }
    });
}

/// `spawn_irodori_health_watcher` の連続失敗カウンタ判定 (pure)。
/// off-by-one バグの温床なので、UI 経路と分離してテスト可能な形に切り出している。
#[derive(Debug, PartialEq)]
pub(crate) struct HealthTick {
    pub fails_after: u32,
    pub should_trigger: bool,
}

pub(crate) fn next_health_tick(prev_fails: u32, ok: bool, threshold: u32) -> HealthTick {
    if ok {
        return HealthTick {
            fails_after: 0,
            should_trigger: false,
        };
    }
    let f = prev_fails.saturating_add(1);
    if f >= threshold {
        HealthTick {
            fails_after: 0, // 通知発火後はリセット
            should_trigger: true,
        }
    } else {
        HealthTick {
            fails_after: f,
            should_trigger: false,
        }
    }
}

/// Irodori サイドカーのヘルスチェック (M4c Phase G)。
/// 30 秒ごとに `/health` を ping し、3 回連続失敗で `IrodoriUnavailable` を通知 + サイドカー shutdown。
/// 次回 `ensure_sidecar_running` で再起動される。サイドカー未起動なら no-op。
pub fn spawn_irodori_health_watcher(app: AppHandle, state: Arc<AppState>) {
    const CHECK_INTERVAL_SECS: u64 = 30;
    const FAIL_THRESHOLD: u32 = 3;
    const DISABLE_SECS: i64 = 20 * 60;
    tauri::async_runtime::spawn(async move {
        let mut fails: u32 = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(CHECK_INTERVAL_SECS)).await;
            let ok = state.tts.irodori.health_ping().await;
            let tick = next_health_tick(fails, ok, FAIL_THRESHOLD);
            fails = tick.fails_after;
            if !tick.should_trigger {
                continue;
            }
            // 3 回連続失敗: shutdown + 20 分の sticky cooldown + 通知 (5 分クールダウン)。
            // disable_for() で次の synthesize_voice が ensure_sidecar_running を即エラー化し、
            // GPU 永続不在環境での 90 秒 churn を止める。voicevox 経路は引き続き動く。
            let _ = state.tts.irodori.shutdown().await;
            state.tts.irodori.disable_for(DISABLE_SECS);
            if state.tts.irodori.should_notify_unavailable() {
                crate::system::notify::notify(
                    &app,
                    &state,
                    crate::system::notify::NoticeKind::IrodoriUnavailable {
                        reason: format!(
                            "ヘルスチェックが {FAIL_THRESHOLD} 回連続失敗。{} 分は再起動を抑制します",
                            DISABLE_SECS / 60
                        ),
                    },
                )
                .await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_health_tick_resets_on_success() {
        let t = next_health_tick(2, true, 3);
        assert_eq!(t, HealthTick { fails_after: 0, should_trigger: false });
    }

    #[test]
    fn next_health_tick_three_failures_trigger_and_reset() {
        let t1 = next_health_tick(0, false, 3);
        assert_eq!(t1, HealthTick { fails_after: 1, should_trigger: false });
        let t2 = next_health_tick(t1.fails_after, false, 3);
        assert_eq!(t2, HealthTick { fails_after: 2, should_trigger: false });
        let t3 = next_health_tick(t2.fails_after, false, 3);
        assert_eq!(t3, HealthTick { fails_after: 0, should_trigger: true });
    }

    #[test]
    fn next_health_tick_intermittent_failures_dont_trigger() {
        let mut fails: u32 = 0;
        for ok in [false, true, false, true, false] {
            let t = next_health_tick(fails, ok, 3);
            assert!(!t.should_trigger);
            fails = t.fails_after;
        }
    }

    #[test]
    fn next_health_tick_threshold_one_triggers_immediately() {
        let t = next_health_tick(0, false, 1);
        assert!(t.should_trigger);
        assert_eq!(t.fails_after, 0);
    }
}

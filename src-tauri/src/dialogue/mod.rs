pub mod advanced;
pub mod banter;
pub mod llm;
pub mod low;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;
use tauri::AppHandle;

use crate::db::ChatRole;
use crate::ghost::dict::SpeechTurn;
use crate::state::{AppState, DialogueMode};
use crate::system::cost;
use crate::system::notify::{self, DegradeReason, NoticeKind};
use crate::system::secrets;

/// フロントへの発話 1 ターン分。
#[derive(Debug, Clone, Serialize)]
pub struct DialogueResponse {
    /// "reply" (ユーザー入力に対する応答) / "event" (起動挨拶等) / "system_message" (notify 経由)
    pub kind: &'static str,
    /// "low" / "advanced"
    pub mode: &'static str,
    /// 掛け合いパターン 1..=4。M2 初期は常に 1、M2-J で 2-4 拡張。
    pub pattern: u8,
    pub main: SpeechTurn,
    pub sub: Option<SpeechTurn>,
    /// 掛け合いパターン3/4 の3ターン目 (spec §4.2.4)。パターン3は main の再発話、
    /// パターン4は sub の再発話。`#balloon-extra` に独立表示する (spec §4.1.3)。
    /// パターン1/2、または LLM が3ターン目を返さなかった場合は None
    /// (その場合 pattern は 3→1・4→2 に縮退済み、`dialogue::banter::assemble_advanced` 参照)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<SpeechTurn>,
    // === M9: バック起点発話のメタ (🔕 フィードバック用、daily-support-design §4.3/§8.2) ===
    // `system::deliver::deliver_event` だけが付与する。ユーザー起点の応答
    // (`send_user_message` の戻り値) には付けない (None のままシリアライズから消える)。
    /// 発話ごとの一意 id (連番文字列)。フロントは表示中発話の id を保持し、
    /// 🔕 クリック時に `feedback_speech(speech_id, category)` で送り返す (誤適用防止)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speech_id: Option<String>,
    /// `SpeechCategory::as_str()` の値。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<&'static str>,
    /// "notice" | "ambient"。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<&'static str>,
    /// 🔕 を表示してよい発話か (Situation* の Ambient のみ true、§4.3)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_allowed: Option<bool>,
}

/// バックエンド起点の発話を chat_log に保存しつつフロントへ emit する共通ヘルパ。
/// ランダムトーク・放置反応・ポモドーロ・起動/終了挨拶など、ユーザー入力を伴わない発話で使う。
/// 戻り値は発話 (dialogue emit) が成立したか。M7 の通知配達 (`system::deliver`) が
/// トーストフォールバック判定に使う。既存呼び出しは無視してよい。
pub fn persist_and_speak(app: &AppHandle, state: &Arc<AppState>, resp: &DialogueResponse) -> bool {
    use tauri::Emitter;
    let now = Utc::now().timestamp();
    let _ = state
        .db
        .append_chat(now, resp.mode, ChatRole::Main, &resp.main.text, resp.main.pose.as_deref());
    if let Some(sub) = &resp.sub {
        let _ = state
            .db
            .append_chat(now, resp.mode, ChatRole::Sub, &sub.text, sub.pose.as_deref());
    }
    if let Err(err) = app.emit("dialogue", resp) {
        crate::ulog!("[persist_and_speak] dialogue emit failed: {err}");
        return false;
    }
    true
}

// ===== オーケストレーション =====
//
// send_user_message から呼ばれる: モード判定・降格チェック・busy ゲート・
// 失敗時 fallback ・ chat_log 永続化を 1 か所に集約する。

/// M7 (spec §4.6.1): `tools::reminder::parse_reminder` が抽出した予定を DB へ登録し、
/// 確認台詞を main 単独の発話として返す。LLM は呼ばない (常時ローカル、advanced 非依存)。
/// chat_log には user と main を保存する。
fn handle_reminder_request(
    app: &AppHandle,
    state: &Arc<AppState>,
    user_text: &str,
    parsed: &crate::tools::reminder::ParsedReminder,
) -> Result<DialogueResponse, String> {
    // 本文が省略された場合は元の発話をそのまま使う (例「5分後」)
    let default_body = format!("「{user_text}」より");
    crate::tools::reminder::register(state, parsed, &default_body)
        .map_err(|e| format!("リマインダー登録に失敗: {e:#}"))?;
    {
        use tauri::Emitter;
        let _ = app.emit("reminders-changed", ());
    }

    let body = if parsed.body.is_empty() {
        default_body
    } else {
        parsed.body.clone()
    };
    let now = Utc::now().timestamp();
    let now_local = chrono::Local::now().naive_local();
    let confirm_text = format_confirmation(&parsed.schedule, now_local, &body);
    let _ = state.db.append_chat(now, "low", ChatRole::User, user_text, None);
    let _ = state
        .db
        .append_chat(now, "low", ChatRole::Main, &confirm_text, None);
    Ok(DialogueResponse {
        kind: "reply",
        mode: "low",
        pattern: 1,
        main: SpeechTurn {
            text: confirm_text,
            pose: None,
        },
        sub: None,
        extra: None,
        speech_id: None,
        category: None,
        priority: None,
        feedback_allowed: None,
    })
}

fn format_confirmation(
    schedule: &crate::tools::reminder::Schedule,
    now_local: chrono::NaiveDateTime,
    body: &str,
) -> String {
    use crate::tools::reminder::{weekday_mask_names, Schedule};
    let fmt_tod = |tod: i32| format!("{}:{:02}", tod / 3600, (tod % 3600) / 60);
    match schedule {
        Schedule::Offset { secs } => {
            let (n, unit) = if *secs >= 3600 && secs % 3600 == 0 {
                (secs / 3600, "時間")
            } else if *secs >= 60 {
                (secs / 60, "分")
            } else {
                (*secs, "秒")
            };
            format!("{n}{unit}後に「{body}」を覚えておくね")
        }
        Schedule::AtTime { local } => {
            use chrono::{Datelike, Timelike};
            let day = (local.date() - now_local.date()).num_days();
            let day_label = match day {
                0 => "今日".to_string(),
                1 => "明日".to_string(),
                2 => "明後日".to_string(),
                _ => format!("{}月{}日", local.month(), local.day()),
            };
            format!(
                "{day_label}の{}:{:02}に「{body}」を覚えておくね",
                local.hour(),
                local.minute()
            )
        }
        Schedule::Daily { time_of_day } => {
            format!("毎日{}に「{body}」を覚えておくね", fmt_tod(*time_of_day))
        }
        Schedule::Weekly { weekday_mask, time_of_day } => {
            format!(
                "毎週{}曜の{}に「{body}」を覚えておくね",
                weekday_mask_names(*weekday_mask),
                fmt_tod(*time_of_day)
            )
        }
    }
}

/// 連続 API エラーがこの回数に達したら一時降格する。
const ERROR_STREAK_THRESHOLD: i64 = 3;
/// 一時降格の保持時間 (秒)。経過後に再度 advanced を試みる。
const DEGRADE_HOLD_SECS: i64 = 300;

pub async fn handle_user_message(
    app: AppHandle,
    state: &Arc<AppState>,
    user_text: &str,
) -> Result<DialogueResponse, String> {
    let trimmed = user_text.trim();

    // 同時実行を 1 件に絞る (busy ゲート)
    let permit = state
        .dialogue
        .busy
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| format!("busy semaphore: {e}"))?;
    state
        .dialogue
        .last_interaction
        .store(Utc::now().timestamp(), Ordering::SeqCst);
    // ユーザー操作で放置カウンタをリセット。
    crate::presence::idle::reset(state);

    // 降格期限が切れていれば復帰通知をまず出す。
    if recover_if_due(&state.dialogue) {
        notify::notify(&app, state, NoticeKind::ModeRecovered).await;
    }

    let result = run_dispatch(&app, state, trimmed).await;
    drop(permit);
    result
}

async fn run_dispatch(
    app: &AppHandle,
    state: &Arc<AppState>,
    user_text: &str,
) -> Result<DialogueResponse, String> {
    let settings = state.settings.lock().expect("settings poisoned").clone();

    // M7 (spec §4.6.1): daily_support_enabled なら予定表現をパースしてリマインダー登録を
    // 試みる。tools_enabled・advanced から独立した常時ローカル動作 (§4.2.1 不変条件)。
    // LLM は呼ばずに即時返事するので高速・低コスト。
    if settings.daily_support_enabled {
        let now_local = chrono::Local::now().naive_local();
        if let Some(parsed) = crate::tools::reminder::parse_reminder(user_text, now_local) {
            return handle_reminder_request(app, state, user_text, &parsed);
        }
    }

    // 上限超過は LLM を呼ぶ「前」に弾く (spec §4.2.7)。
    // 以前は try_advanced の成功後にしか判定しておらず、超過後も呼び続けていた。
    let gate = cost_gate(state, &settings);
    if gate == CostGate::Exceeded && !cost::notified_this_month(&state.db, cost::KEY_LIMIT_NOTIFIED)
    {
        // **この turn の返答そのものを告知にする。**
        // emit で別発話として流すと、直後に返る low 応答が同じ吹き出しへ描画され
        // (フロントの listen コールバックは並行する)、月 1 回しか出ない告知が
        // 視認前に消えうる。しかも告知済みフラグは立つので二度と出ない。
        // 返答として返せば必ず表示される。
        if let Some(resp) = system_message_reply(state, "cost_limit_exceeded") {
            cost::mark_notified_this_month(&state.db, cost::KEY_LIMIT_NOTIFIED);
            return Ok(resp);
        }
        // 辞書にキーが無いゴースト向けの保険 (既定辞書には v0.5 で追加済み)。
        announce_cost_limit_once(app, state, &settings).await;
    }
    // v0.5.3: 集計できずに止めた場合も、同じ理由（吹き出しの取り合い）で
    // この turn の返答として出す。こちらはプロセス内フラグで 1 回だけ。
    if gate == CostGate::Unknown
        && !state
            .dialogue
            .cost_unknown_notified
            .load(Ordering::SeqCst)
    {
        if let Some(resp) = system_message_reply(state, "cost_unknown") {
            state
                .dialogue
                .cost_unknown_notified
                .store(true, Ordering::SeqCst);
            return Ok(resp);
        }
        announce_cost_unknown_once(app, state, &settings).await;
    }
    let want_advanced = matches!(settings.mode, DialogueMode::Advanced)
        && !is_degraded(&state.dialogue)
        && !gate.blocks();

    if want_advanced {
        match try_advanced(state, user_text).await {
            Ok(resp) => {
                state.dialogue.error_streak.store(0, Ordering::SeqCst);
                // 成功直後にコスト判定 (api_usage が増えた直後)。
                evaluate_cost_status(app, state, &settings).await;
                return Ok(resp);
            }
            Err(err) => {
                let streak = state.dialogue.error_streak.fetch_add(1, Ordering::SeqCst) + 1;
                crate::ulog!("[advanced] error_streak={streak}: {err:#}");
                if streak >= ERROR_STREAK_THRESHOLD {
                    degrade(&state.dialogue);
                    notify::notify(
                        app,
                        state,
                        NoticeKind::ModeDegraded {
                            reason: DegradeReason::ApiError,
                        },
                    )
                    .await;
                }
            }
        }
    }
    // low へフォールバック
    fallback_low(state, user_text)
}

/// 当月コストを評価し、80% 警告 / 上限超過 (降格 + 告知) を月内一度きりで出す。
/// M14: advanced 独り言の補充 (`system::monologue`) も**同じ関数**を通す
/// (背景処理だけが上限を素通りする穴を作らない、foundation-design §3.5)。
/// 上限超過の告知を **その月に 1 回だけ** 出す。
///
/// 以前は非永続の `AtomicBool` を使っており、(1) 再起動で消える
/// (2) **月が替わっても戻らない**ため翌月の警告が二度と鳴らない、という 2 つの穴があった。
/// 当月タグを `app_settings` に保存して判定する（spec §4.2.7「次月リセットで復帰。」）。
///
/// **降格タイマーは張らない。** 超過の判定は `cost_exceeded` が毎回 DB を見て行うので、
/// タイマーで解除されると「5 分後に課金が再開する」という以前の穴に戻る。
///
/// **届いたときにだけ済みにする**（v0.5.6 項目 6）。以前は出す前に済みにしていたので、隠している間に
/// 上限に達するとその月は二度と告知しなかった（降格は画面に出る場所が無く、発話が唯一の伝達手段）。
pub(crate) async fn announce_cost_limit_once(
    app: &AppHandle,
    state: &Arc<AppState>,
    settings: &crate::state::Settings,
) {
    let told = notify::once_reached(
        cost::notified_this_month(&state.db, cost::KEY_LIMIT_NOTIFIED),
        || {
            notify::notify(
                app,
                state,
                NoticeKind::CostLimitExceeded {
                    provider: settings.llm_provider.clone(),
                },
            )
        },
        || cost::mark_notified_this_month(&state.db, cost::KEY_LIMIT_NOTIFIED),
    )
    .await;
    if told.is_some_and(notify::NoticeOutcome::reached) {
        notify::notify(
            app,
            state,
            NoticeKind::ModeDegraded {
                reason: DegradeReason::CostLimit,
            },
        )
        .await;
    }
}

/// 80% 到達の警告。呼び出し後のコスト記録を見て、その月に 1 回だけ出す。
///
/// 上限超過そのものの判定は `cost_exceeded` ゲートが LLM 呼び出しの前に行うので、
/// ここは 80% 警告と、超過に「今まさに乗った」場合の告知だけを担当する。
pub(crate) async fn evaluate_cost_status(
    app: &AppHandle,
    state: &Arc<AppState>,
    settings: &crate::state::Settings,
) {
    let status = match cost::check_status(&state.db, settings.monthly_limit_usd) {
        Ok(s) => s,
        Err(err) => {
            crate::ulog!("[cost] check_status failed: {err:#}");
            return;
        }
    };
    if status.unlimited {
        return;
    }
    if status.exceeded {
        announce_cost_limit_once(app, state, settings).await;
    } else if status.reached_80 {
        // 届いたときにだけ済みにする（v0.5.6 項目 6。以前は出す前に済みにしていた）。
        notify::once_reached(
            cost::notified_this_month(&state.db, cost::KEY_WARNED_80),
            || {
                notify::notify(
                    app,
                    state,
                    NoticeKind::CostWarning80 {
                        provider: settings.llm_provider.clone(),
                    },
                )
            },
            || cost::mark_notified_this_month(&state.db, cost::KEY_WARNED_80),
        )
        .await;
    }
}

/// LLM を呼ぶ前のコストゲートの判定結果 (spec §4.2.7)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CostGate {
    /// 呼んでよい（上限が無制限、または当月の使用が上限内）。
    Allow,
    /// 当月上限を超えている。
    Exceeded,
    /// **当月コストを集計できなかった。**
    Unknown,
}

impl CostGate {
    /// LLM 呼び出しを止めるか。
    pub(crate) fn blocks(self) -> bool {
        !matches!(self, CostGate::Allow)
    }
}

/// 月額上限のゲート。**LLM を呼ぶ前に必ず通す唯一のゲート** (spec §4.2.7)。
///
/// `cost::check_status` は毎回 `api_usage` の当月分を DB から集計するので、
/// プロセスを跨いでも月が替わっても正しい。以前は「超過 → 300 秒の一時降格」
/// だったため、実質「5 分の一時停止」でその月ずっと課金が続いていた。
/// 降格タイマー (`degraded_until`) は **API エラー由来専用**とし、コスト超過は
/// このゲートで毎回判定する。
///
/// **集計に失敗したら止める (fail-closed、v0.5.3)。** v0.5.2 まではここで
/// `false` を返して通していた（「集計できないときに課金を止めるのは過剰」）。
/// だが v0.5.2 で**破損 DB でも起動を続ける**ようにしたことで `sum_cost_since` の
/// 失敗が現実の経路になり、「上限を設定しているのに無制限に課金される」という
/// 組み合わせが生まれた。上限が有限＝ユーザーが「ここまで」と言っている以上、
/// 守れないなら使わない。止めたことは `CostGate::Unknown` として呼び出し側へ返し、
/// **黙って low に落ちたように見えないよう告知する**。
pub(crate) fn cost_gate(state: &Arc<AppState>, settings: &crate::state::Settings) -> CostGate {
    let limit = settings.monthly_limit_usd;
    decide_cost_gate(limit, || cost::check_status(&state.db, limit))
}

/// `cost_gate` の判定そのもの。`AppState` を組み立てずにテストできるよう分けてある。
fn decide_cost_gate(
    limit_usd: f64,
    check: impl FnOnce() -> anyhow::Result<cost::CostStatus>,
) -> CostGate {
    if limit_usd <= 0.0 {
        // 無制限は「守るべき上限が無い」ので、集計できるかどうかと無関係に通す。
        return CostGate::Allow;
    }
    match check() {
        Ok(st) if st.exceeded => CostGate::Exceeded,
        Ok(_) => CostGate::Allow,
        Err(err) => {
            crate::ulog!("[cost] check_status failed (上限を守れないので止めます): {err:#}");
            CostGate::Unknown
        }
    }
}

/// システムメッセージを「この turn の返答」として組み立てる。
///
/// 辞書 `system_messages.<key>` を引く。キーが無ければ None。
fn system_message_reply(state: &Arc<AppState>, key: &str) -> Option<DialogueResponse> {
    let guard = state.ghost.lock().expect("ghost poisoned");
    let bundle = guard.as_ref().ok()?;
    let ctx = crate::ghost::dict::WhenContext::now();
    let line = bundle
        .dictionary
        .pick_system_message(key, &ctx, bundle.sub_available())?;
    Some(banter::pattern_1("event", "low", line))
}

/// 「集計できないので止めている」告知を **このプロセスで 1 回だけ**出す。
///
/// 月次タグ (`cost::KEY_LIMIT_NOTIFIED` 等) を使わないのは意図的で、理由は
/// `DialogueState::cost_unknown_notified` のコメントに書いた（記録先の DB 自体が
/// 疑わしい状態なので、永続フラグに頼ると毎ターン告知しかねない）。
///
/// **届いたときにだけ済みにする**（v0.5.6 項目 6）。以前は出す前に済みにしていたので、隠している間に
/// 集計できなくなると、その起動のあいだ二度と告知しなかった。
pub(crate) async fn announce_cost_unknown_once(
    app: &AppHandle,
    state: &Arc<AppState>,
    settings: &crate::state::Settings,
) {
    notify::once_reached(
        state.dialogue.cost_unknown_notified.load(Ordering::SeqCst),
        || {
            notify::notify(
                app,
                state,
                NoticeKind::CostUnknown {
                    provider: settings.llm_provider.clone(),
                },
            )
        },
        || {
            state
                .dialogue
                .cost_unknown_notified
                .store(true, Ordering::SeqCst)
        },
    )
    .await;
}

/// 背景経路（独り言補充など）で、ゲートが止めた理由に応じた告知を出す。
/// チャット経路は「この turn の返答」として出すので、こちらは通らない。
pub(crate) async fn announce_cost_block(
    app: &AppHandle,
    state: &Arc<AppState>,
    settings: &crate::state::Settings,
    gate: CostGate,
) {
    match gate {
        CostGate::Allow => {}
        CostGate::Exceeded => evaluate_cost_status(app, state, settings).await,
        CostGate::Unknown => announce_cost_unknown_once(app, state, settings).await,
    }
}

fn degrade(d: &crate::state::DialogueState) {
    let until = Utc::now().timestamp() + DEGRADE_HOLD_SECS;
    d.degraded_until.store(until, Ordering::SeqCst);
}

fn recover_if_due(d: &crate::state::DialogueState) -> bool {
    let until = d.degraded_until.load(Ordering::SeqCst);
    if until == 0 {
        return false;
    }
    let now = Utc::now().timestamp();
    if now >= until {
        d.degraded_until.store(0, Ordering::SeqCst);
        d.error_streak.store(0, Ordering::SeqCst);
        true
    } else {
        false
    }
}

async fn try_advanced(
    state: &Arc<AppState>,
    user_text: &str,
) -> anyhow::Result<DialogueResponse> {
    let settings = {
        let s = state.settings.lock().expect("settings poisoned");
        s.clone()
    };
    let api_key = secrets::get_api_key_async(&settings.llm_provider).await?;
    // std::sync::MutexGuard を await を跨いで保持できないので、ブロックで握り→外す。
    let bundle = {
        let guard = state.ghost.lock().expect("ghost poisoned");
        match guard.as_ref() {
            Ok(b) => b.clone(),
            Err(s) => return Err(anyhow::anyhow!("{s}")),
        }
    };

    let reply = advanced::reply(&settings, &bundle, &state.db, api_key, user_text).await?;
    Ok(reply.response)
}

fn fallback_low(
    state: &Arc<AppState>,
    user_text: &str,
) -> Result<DialogueResponse, String> {
    let bundle_guard = state.ghost.lock().expect("ghost poisoned");
    let bundle = bundle_guard.as_ref().map_err(|s| s.clone())?;
    let sub_available = bundle.sub_available();
    // 記憶想起 (architecture §6.2) の材料。新しい順に見て最初に一致したものを使う。
    // 取得に失敗しても応答自体は続ける（記憶が無いのと同じ扱い）。
    let profile: Vec<(String, Option<String>)> = state
        .db
        .list_profile()
        .unwrap_or_default()
        .into_iter()
        .rev()
        .map(|e| (e.content, e.source_keywords))
        .collect();
    let resp = low::reply(&bundle.dictionary, user_text, &profile, sub_available);
    let now = Utc::now().timestamp();
    let _ = state.db.append_chat(now, "low", ChatRole::User, user_text, None);
    let _ = state.db.append_chat(
        now,
        "low",
        ChatRole::Main,
        &resp.main.text,
        resp.main.pose.as_deref(),
    );
    if let Some(sub) = &resp.sub {
        let _ = state.db.append_chat(
            now,
            "low",
            ChatRole::Sub,
            &sub.text,
            sub.pose.as_deref(),
        );
    }
    Ok(resp)
}

fn is_degraded(d: &crate::state::DialogueState) -> bool {
    let until = d.degraded_until.load(Ordering::SeqCst);
    if until == 0 {
        return false;
    }
    let now = Utc::now().timestamp();
    now < until
}


#[cfg(test)]
mod cost_gate_tests {
    use super::*;
    use crate::db::Db;
    use crate::system::cost;

    fn db_with_cost(dir: &std::path::Path, cost_usd: f64) -> Db {
        let db = Db::open(&dir.join("companion.db")).expect("open");
        db.migrate().expect("migrate");
        if cost_usd > 0.0 {
            db.append_api_usage(&crate::db::ApiUsageRow {
                provider: "openai".into(),
                model: "gpt-4o-mini".into(),
                prompt_tokens: 0,
                completion_tokens: 0,
                cost_usd,
                ts: cost::month_start_unix() + 1,
            })
            .expect("append");
        }
        db
    }

    #[test]
    fn within_limit_allows_and_over_limit_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_with_cost(dir.path(), 0.5);
        assert_eq!(
            decide_cost_gate(1.0, || cost::check_status(&db, 1.0)),
            CostGate::Allow
        );

        let dir2 = tempfile::tempdir().unwrap();
        let db2 = db_with_cost(dir2.path(), 2.0);
        assert_eq!(
            decide_cost_gate(1.0, || cost::check_status(&db2, 1.0)),
            CostGate::Exceeded
        );
    }

    /// **集計できないときは止める (fail-closed、v0.5.3)。**
    ///
    /// v0.5.2 まではここで通していた。v0.5.2 が「破損 DB でも起動を続ける」ように
    /// したことで `sum_cost_since` の失敗が現実の経路になり、**上限を設定して
    /// いるのに無制限に課金される**組み合わせが生まれた。
    #[test]
    fn unaggregatable_cost_blocks_when_a_limit_is_set() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_with_cost(dir.path(), 0.0);
        // 集計元を壊す。実機では破損 DB が同じ Err を出す。
        {
            let conn = rusqlite::Connection::open(dir.path().join("companion.db")).unwrap();
            conn.execute("DROP TABLE api_usage", []).unwrap();
        }
        assert!(cost::check_status(&db, 1.0).is_err(), "前提: 集計が失敗する");

        assert_eq!(
            decide_cost_gate(1.0, || cost::check_status(&db, 1.0)),
            CostGate::Unknown,
            "上限があるのに集計できないなら止める"
        );
        assert!(decide_cost_gate(1.0, || cost::check_status(&db, 1.0)).blocks());
    }

    /// 無制限 (上限 0) では集計できなくても止めない。
    /// **守るべき上限が無いのに機能を落とすのは、ただの機能低下**になるため。
    #[test]
    fn unaggregatable_cost_does_not_block_when_unlimited() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_with_cost(dir.path(), 0.0);
        {
            let conn = rusqlite::Connection::open(dir.path().join("companion.db")).unwrap();
            conn.execute("DROP TABLE api_usage", []).unwrap();
        }
        assert_eq!(
            decide_cost_gate(0.0, || cost::check_status(&db, 0.0)),
            CostGate::Allow
        );
    }
}

//! 通知配達サービス (M7、daily-support-design §3)。
//!
//! バック起点の自発発話の**単一経路**。ガバナンス判定 (`governance::can_deliver`) を
//! 呼ぶのはここだけで、check → 配達 → record を busy セマフォの permit 保持下で
//! 直列化する (複数 watcher の check-then-act 競合と二重発話の防止、§4.2)。
//!
//! 到達保証 (§3.1): gate 通過は「抑制されない」であって「届いた」ではない。
//! 実到達は `DeliveryOutcome` で返し、Notice の呼び出し側 (リマインダー watcher) は
//! `Deferred | Failed` を未達として active 維持・再試行につなぐ。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use chrono::Utc;
use tauri::{AppHandle, Emitter, Manager};

use crate::dialogue::{self, banter, DialogueResponse};
use crate::ghost::dict::{DialogueLine, SpeechTurn, WhenContext};
use crate::state::{AppState, DialogueMode};
use crate::system::governance::{self, Priority, SpeechCategory};

/// 配達の到達結果 (§3.1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// ゴースト発話で到達。
    Ghost,
    /// 発話不能でトーストにフォールバックして到達。
    Toast,
    /// ガバナンス抑制 or 発話中につき見送り (呼び出し側が再試行する)。
    Deferred,
    /// 配達手段が無かった (辞書未ヒットかつ fallback なし、emit 失敗等)。
    Failed,
}

impl DeliveryOutcome {
    /// reminder_log.delivery に入れる小文字表記。
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryOutcome::Ghost => "ghost",
            DeliveryOutcome::Toast => "toast",
            DeliveryOutcome::Deferred => "deferred",
            DeliveryOutcome::Failed => "failed",
        }
    }

    /// 到達した (ユーザーに見える形になった) か。
    pub fn reached(self) -> bool {
        matches!(self, DeliveryOutcome::Ghost | DeliveryOutcome::Toast)
    }
}

/// 通知配達の単一経路 (§3.1)。呼び出し側は gate を呼ばず category/priority を渡す
/// (二重ゲート禁止)。
///
/// 手順: busy try_acquire (直列化) → `can_deliver` 1 回 → 辞書解決 →
/// プレースホルダ置換 → `persist_and_speak` → 成功時のみ `record_delivered`。
/// 辞書未ヒット or 発話失敗時は `fallback` があれば `system-toast` へ。
pub async fn deliver_event(
    app: &AppHandle,
    state: &Arc<AppState>,
    category: SpeechCategory,
    priority: Priority,
    key: &str,
    placeholders: &[(&str, &str)],
    fallback: Option<String>,
) -> DeliveryOutcome {
    // 直列化: ユーザー応答・他の自発発話と同時に喋らない。取れなければ見送り
    // (リマインダー等の Notice は呼び出し側のポーリングで自然に再試行される)。
    let _permit = match state.dialogue.busy.try_acquire() {
        Ok(p) => p,
        Err(_) => return DeliveryOutcome::Deferred,
    };

    // ガバナンス判定はここで 1 回だけ (純粋判定・副作用なし)。
    if !governance::can_deliver(state, category, priority) {
        return DeliveryOutcome::Deferred;
    }

    // 可視性は **到達判定** であってガバナンスではないので、can_deliver には
    // 段を足さずここで見る (二重ゲート禁止の維持)。隠している間の発話は
    // 誰にも届かないため、喋ったことにしない。
    if !window_is_visible(app) {
        return DeliveryOutcome::Deferred;
    }

    // 辞書解決。Monologue のみ monologue セクション、他は events キー。
    let line = resolve_line(state, category, key);

    let outcome = match line {
        Some(mut line) => {
            apply_placeholders_to_line(&mut line, placeholders);
            let mut resp: DialogueResponse = banter::pattern_1("event", "low", line);
            // M9/M11: 🔕 フィードバック用メタ (§4.3)。バック起点発話にのみ付与する。
            // 🔕 の表示 (feedback_allowed) は feedback_target() — Situation* (段 4/5 と
            // バックオフの適用対象) に加え、M11 から Regular* 2 種 (カウントのみ、間隔
            // 延長は無し) も対象にする (regular-talk-design §4.2)。Notice や独り言に
            // 「頻度を下げる」レバーは無い。
            let seq = state.governance.speech_seq.fetch_add(1, Ordering::SeqCst) + 1;
            resp.speech_id = Some(seq.to_string());
            resp.category = Some(category.as_str());
            resp.priority = Some(priority.as_str());
            resp.feedback_allowed = Some(category.feedback_target());
            if dialogue::persist_and_speak(app, state, &resp) {
                // feedback_speech が「最新のタグ付き発話」とだけ照合できるよう記録
                // (permit 保持中 = 直列化下なので競合しない)
                *state.governance.last_speech.lock().expect("last_speech poisoned") =
                    Some((seq, category));
                DeliveryOutcome::Ghost
            } else {
                toast_fallback(app, fallback)
            }
        }
        None => toast_fallback(app, fallback),
    };

    // 到達 (Ghost|Toast) した時だけ最終発話時刻を更新する (§3.1)。
    if outcome.reached() {
        governance::record_delivered(state, category, Utc::now().timestamp());
    }
    outcome
}

/// ユーザー起点の確認発話 (スヌーズ確認・終了前確認等) を辞書 events キーから
/// **ゲートを通さず**発話する。設計 §4.2「ユーザー起点の応答はゲートしない」に対応する
/// 補助経路で、record_delivered も 🔕 メタの付与も行わない (自発発話の会計に混ぜない)。
/// 戻り値は発話した内容 (辞書未ヒット・発話失敗は None。呼び出し側は UI 表示で足りるため
/// 黙殺してよい。tray の終了前確認は hold 時間の計算に使う)。
pub fn speak_event_now(
    app: &AppHandle,
    state: &Arc<AppState>,
    key: &str,
    placeholders: &[(&str, &str)],
) -> Option<DialogueResponse> {
    let line = resolve_line(state, SpeechCategory::Reminder, key)?;
    let mut line = line;
    apply_placeholders_to_line(&mut line, placeholders);
    let resp: DialogueResponse = banter::pattern_1("event", "low", line);
    if dialogue::persist_and_speak(app, state, &resp) {
        Some(resp)
    } else {
        None
    }
}

fn resolve_line(
    state: &Arc<AppState>,
    category: SpeechCategory,
    key: &str,
) -> Option<DialogueLine> {
    // M14: 独り言だけは advanced のストック (LLM 生成 + 時事ネタ織り込み) を先に見る。
    // **ghost ロックの外**で DB を触るのが要点 (foundation-design §3.3): pop は書き込み
    // トランザクションなので、ghost ロックの内側でやると DB I/O の間ずっと保持してしまう。
    if matches!(category, SpeechCategory::Monologue) {
        if let Some(line) = pop_advanced_monologue(state) {
            return Some(line);
        }
    }
    let guard = state.ghost.lock().expect("ghost poisoned");
    let bundle = guard.as_ref().ok()?;
    let sub = bundle.sub_available();
    match category {
        SpeechCategory::Monologue => bundle.dictionary.pick_monologue(sub),
        _ => bundle.dictionary.pick_event(key, &WhenContext::now(), sub),
    }
}

/// advanced のストックから独り言を 1 件取り出す (M14、foundation-design §3.3)。
///
/// `None` を返す条件はすべて「辞書の独り言へ落ちる」に合流する (spec §4.2.1 AI 非依存):
/// low モード / **降格中** / ストックが空 / 全部失効 (ゴースト不一致・時事ネタ 7 日超・
/// 生成 30 日超) / ゴースト未読込 / DB エラー。
fn pop_advanced_monologue(state: &Arc<AppState>) -> Option<DialogueLine> {
    let mode = state.settings.lock().expect("settings poisoned").mode.clone();
    if !matches!(mode, DialogueMode::Advanced) {
        return None;
    }
    // spec §4.4.4:「降格中・LLM 障害時は low と同じ辞書経路にフォールバックする」。
    // ストックを喋るだけなら API は叩かないが、**降格中は advanced の顔を出さない**のが
    // 要件の書き方なのでそれに従う (ストックは消費されず、復帰後にそのまま使える)。
    let degraded_until = state.dialogue.degraded_until.load(Ordering::SeqCst);
    if degraded_until != 0 && Utc::now().timestamp() < degraded_until {
        return None;
    }
    // 鍵は**いま読み込まれている bundle の id**（`settings.ghost_id` はゴースト切替時に
    // 再起動より先に変わるため、実際に喋っている人格とズレる）。ロックは id の複製だけで
    // すぐ外し、DB I/O は ghost ロックの外で行う (foundation-design §3.3)。
    let ghost_id = {
        let guard = state.ghost.lock().expect("ghost poisoned");
        guard.as_ref().ok()?.ghost.id.clone()
    };
    let row = state
        .db
        .pop_monologue_cache(&ghost_id, Utc::now().timestamp())
        .unwrap_or_else(|err| {
            crate::ulog!("[deliver] 独り言ストックの取り出しに失敗、辞書へ: {err:#}");
            None
        })?;
    // pose はシェル依存で、生成時と今とでシェルが違いうる。存在しない pose を渡すと
    // 表情が変わらないだけでなく、フロントが未知 pose を掴むので None に落とす
    // (advanced 応答の validate_pose と同じ考え方)。
    let pose = row.pose.and_then(|p| {
        let guard = state.ghost.lock().expect("ghost poisoned");
        let bundle = guard.as_ref().ok()?;
        bundle.shell.characters.main.poses.contains_key(&p).then_some(p)
    });
    Some(DialogueLine {
        main: SpeechTurn {
            text: row.text,
            pose,
        },
        // 独り言は 1 キャラの発話 (ストックは sub を持たない、foundation-design §3.2)。
        sub: None,
    })
}

/// メインウインドウがユーザーに見えているか。
///
/// **なぜ必要か**: spec §4.6.1 は「発話不能時のフォールバックとして Windows トースト」
/// と書いているが、実装のトーストは OS 通知ではなく**アプリ内の 6 秒帯**
/// (`src/system/toast.ts` が `#system-toast` に出す) で、OS 通知プラグインは無い。
/// `app.emit` はウインドウが hide されていても `Ok` を返すため、トレイに隠している
/// 間に期限が来た単発リマインダーが **画面に何も出ないまま完了扱いで消えていた**。
///
/// 見えていないなら未達 (`Deferred`) とし、呼び出し側の再試行に委ねる。
fn window_is_visible(app: &AppHandle) -> bool {
    match app.get_webview_window("main") {
        // 取得できない/判定できないときは「見えている」に倒す。
        // ここで false に倒すと、判定不能なだけで通知が永久に滞留する。
        Some(w) => w.is_visible().unwrap_or(true),
        None => true,
    }
}

fn toast_fallback(app: &AppHandle, fallback: Option<String>) -> DeliveryOutcome {
    match fallback {
        Some(text) => {
            if app.emit("system-toast", text).is_ok() {
                DeliveryOutcome::Toast
            } else {
                DeliveryOutcome::Failed
            }
        }
        None => DeliveryOutcome::Failed,
    }
}

fn apply_placeholders_to_line(line: &mut DialogueLine, placeholders: &[(&str, &str)]) {
    line.main.text = apply_placeholders(&line.main.text, placeholders);
    if let Some(sub) = &mut line.sub {
        sub.text = apply_placeholders(&sub.text, placeholders);
    }
}

/// `{body}` `{count}` `{time}` `{summary}` 形式のプレースホルダを置換する (§3.3)。
/// 渡されなかった未知のプレースホルダは残さず空文字へ落とす
/// (辞書と実装のキー名ずれで `{xxx}` が生のままユーザーに見えるのを防ぐ)。
fn apply_placeholders(text: &str, placeholders: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        // `{` の直後から `}` までが ASCII の識別子ならプレースホルダとみなす
        match after.find('}') {
            Some(close)
                if close > 0
                    && after[..close]
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') =>
            {
                let name = &after[..close];
                if let Some((_, v)) = placeholders.iter().find(|(k, _)| *k == name) {
                    out.push_str(v);
                }
                // 未知プレースホルダは何も足さない (= 空へ)
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **未達は「届いた」に数えない。**
    ///
    /// `tasks.rs` は `outcome.reached()` が真のときだけ `advance_after_delivery` を
    /// 呼ぶ (= 単発リマインダーを完了扱いにする)。ここが崩れると、ウインドウを
    /// 隠している間に期限が来たリマインダーが画面に何も出ないまま消える。
    #[test]
    fn deferred_and_failed_are_not_reached() {
        assert!(!DeliveryOutcome::Deferred.reached(), "保留を到達扱いにしてはいけない");
        assert!(!DeliveryOutcome::Failed.reached());
        assert!(DeliveryOutcome::Ghost.reached());
        // Toast はアプリ内の帯なので、可視性ゲートを通った後だけ到達になる
        // (ゲートは deliver_event 側にあり、この列挙の意味は「表示までは行った」)。
        assert!(DeliveryOutcome::Toast.reached());
    }

    /// `reminder_log.delivery` に入る表記の固定。保留が他の値に化けると
    /// 「届かなかった」の追跡ができなくなる。
    #[test]
    fn outcome_strings_are_stable() {
        assert_eq!(DeliveryOutcome::Ghost.as_str(), "ghost");
        assert_eq!(DeliveryOutcome::Toast.as_str(), "toast");
        assert_eq!(DeliveryOutcome::Deferred.as_str(), "deferred");
        assert_eq!(DeliveryOutcome::Failed.as_str(), "failed");
    }

    #[test]
    fn placeholders_replace_known_keys() {
        let s = apply_placeholders("『{body}』の時間だよ、{time}!", &[("body", "お茶"), ("time", "9:00")]);
        assert_eq!(s, "『お茶』の時間だよ、9:00!");
    }

    #[test]
    fn unknown_placeholders_are_dropped() {
        let s = apply_placeholders("こんにちは{unknown}世界", &[]);
        assert_eq!(s, "こんにちは世界");
    }

    #[test]
    fn non_placeholder_braces_are_kept() {
        // 識別子でない・閉じられていない波括弧は素通し
        assert_eq!(apply_placeholders("顔文字 {・∀・} と {", &[]), "顔文字 {・∀・} と {");
        assert_eq!(apply_placeholders("{}", &[]), "{}");
    }

    #[test]
    fn same_key_replaces_all_occurrences() {
        let s = apply_placeholders("{body}、{body}", &[("body", "薬")]);
        assert_eq!(s, "薬、薬");
    }

    #[test]
    fn outcome_strings_match_db_contract() {
        assert_eq!(DeliveryOutcome::Ghost.as_str(), "ghost");
        assert_eq!(DeliveryOutcome::Toast.as_str(), "toast");
        assert_eq!(DeliveryOutcome::Deferred.as_str(), "deferred");
        assert_eq!(DeliveryOutcome::Failed.as_str(), "failed");
        assert!(DeliveryOutcome::Ghost.reached());
        assert!(DeliveryOutcome::Toast.reached());
        assert!(!DeliveryOutcome::Deferred.reached());
        assert!(!DeliveryOutcome::Failed.reached());
    }
}

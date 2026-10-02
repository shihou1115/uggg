//! ゴースト発話による横断通知 (spec §3.1 / architecture §11)。
//!
//! 辞書 `system_messages` にキーがあればそれを発話。無ければトーストフォールバックの
//! 代わりに `system-toast` イベントをフロントへ流す (M2 段階では console.error 代替)。
//!
//! **見えていない間は出さずに保留し、届いたかを返す**（v0.5.6 項目 6、§4.6.1 と同じ判定）。
//! 1 回だけ出す告知（コストの 80% 警告・上限到達・集計不能・アプリ更新）は `once_reached` で
//! **届いたときにだけ済みにする**（以前は届いたかを見ずに済みにし、隠している間に出ると二度と出なかった）。

use std::sync::Arc;

use tauri::{AppHandle, Emitter};

use crate::dialogue::{banter, DialogueResponse};
use crate::ghost::dict::WhenContext;
use crate::state::AppState;

#[derive(Debug, Clone)]
pub enum NoticeKind {
    CostWarning80 {
        provider: String,
    },
    CostLimitExceeded {
        provider: String,
    },
    /// **当月コストを集計できない** (v0.5.3、spec §4.2.7)。
    /// 上限が有限なら、集計不能の間は LLM を呼ばずに止める (fail-closed) ので、
    /// 黙って low に落ちたように見えないよう告知する。
    CostUnknown {
        provider: String,
    },
    ModeDegraded {
        reason: DegradeReason,
    },
    ModeRecovered,
    /// voicevox_core 資産 DL 完了。
    VoicevoxDlComplete,
    /// voicevox_core 資産 DL 失敗 (詳細は reason)。
    VoicevoxDlFailed {
        reason: String,
    },
    /// Irodori-TTS が利用できない (GPU 不可 / サイドカー起動失敗 / ヘルスチェック失敗 等)。
    /// 5 分に 1 回（間隔は発話の前に刻む）。次の失敗でまた出るので、届いたかは見ない（v0.5.6 項目 6 の対象外）。
    /// M4c Phase G の `tasks::spawn_irodori_health_watcher` から発火する。
    /// **理由の種類で言い分ける**（v0.5.7 項目 7、spec §6.0）。以前は理由によらず「GPU 環境が整っていない」と言っていた。
    IrodoriUnavailable {
        reason: String,
        kind: UnavailableKind,
    },
    /// **Irodori のランタイムに更新がある**（v0.5.7 項目 7）。以前は設定パネルを開いた人にしか見えず、
    /// 乗り換えが届いたことに気付けなかった。更新の対象の組ごとに 1 回、届いたときだけ済みにする（`once_reached`）。
    IrodoriUpdateAvailable {
        targets: Vec<String>,
    },
    /// Irodori Python ランタイム + 共通依存 DL が完了 (M4c Phase C 以降)。
    IrodoriDlComplete,
    /// Irodori 資産 DL が失敗 (Python embeddable / pip / torch / 依存のいずれかで失敗)。
    IrodoriDlFailed {
        reason: String,
    },
    /// M5-D: 新バージョン検出 (`update_feed_url` からの応答に基づく告知)。
    UpdateAvailable {
        version: String,
    },
    /// **カレンダーの取得に失敗し始めた**（v0.5.7 項目 9、spec §4.6.4）。ICS の URL が失効すると、開始前の通知が
    /// 静かに 1 件も来なくなっていた。取得元ごとに、失敗が続き始めたら 1 回（届いたときだけ済み）。
    /// `sources` は取得元の呼び名（`CalendarSource::label`。**URL は入れない**）。
    CalendarFetchFailed {
        sources: Vec<String>,
    },
    /// **参照音声が無い slot を VOICEVOX で話した**（v0.6.0 項目 3、spec §6.0）。高品質モードで、読み込み中の
    /// ゴーストにその slot の参照音声が無いとき。故障ではないので `IrodoriUnavailable` とは別（「使えない」と
    /// 言わず、5 分の待ちも共有しない）。ゴースト × slot ごとにプロセスで 1 回、届いたときだけ済み（`OnceGate`）。
    IrodoriVoiceRefMissing,
    /// **声を決めていないゴーストが VOICEVOX で初めて喋った**（v0.6.0 項目 2）。「声は設定の『音声』で変えられる」。
    /// ゴーストごとに 1 回、届いたときだけ済み（`app_settings` の `ghost_voice_hint_seen:<ghost_id>`）。
    GhostVoiceUnset,
    // M7: ReminderFired variant は削除した。リマインダー発火は
    // `system::deliver::deliver_event` + 辞書 events.reminder_fired 経路に一本化
    // (daily-support-design §3/§7.1)。
}

/// Irodori が使えない理由の種類（v0.5.7 項目 7）。辞書の行を言い分ける。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableKind {
    /// 導入・更新の最中（ユーザーが自分で始めた。終われば戻る）。
    Updating,
    /// モデルの重みやトークナイザが無い（更新が要る）。
    ModelMissing,
    /// GPU が見えない。
    NoGpu,
    /// GPU のメモリ（VRAM）が足りない。
    Vram,
    /// そのほか（サイドカーの起動に失敗した・応答しない 等）。
    Other,
}

#[derive(Debug, Clone)]
pub enum DegradeReason {
    ApiError,
    CostLimit,
}

impl NoticeKind {
    /// 辞書 `system_messages` のキー。
    ///
    /// **変種を増やすとこの match がコンパイルエラーになる**ので、キーの割り当て漏れは
    /// 起きない。一方「割り当てたキーが既定辞書に無い」は静かに起きる
    /// (`cost_warning_80` / `cost_limit_exceeded` が実際にそうだった) ため、
    /// 下の `dict_key_contract` テストが出荷辞書との突合を行う。
    fn dict_key(&self) -> &'static str {
        match self {
            NoticeKind::CostWarning80 { .. } => "cost_warning_80",
            NoticeKind::CostLimitExceeded { .. } => "cost_limit_exceeded",
            NoticeKind::CostUnknown { .. } => "cost_unknown",
            NoticeKind::ModeDegraded { .. } => "mode_degraded",
            NoticeKind::ModeRecovered => "mode_recovered",
            NoticeKind::VoicevoxDlComplete => "voicevox_dl_complete",
            NoticeKind::VoicevoxDlFailed { .. } => "voicevox_dl_failed",
            NoticeKind::IrodoriUnavailable { kind, .. } => match kind {
                UnavailableKind::Updating => "irodori_updating",
                UnavailableKind::ModelMissing => "irodori_model_missing",
                UnavailableKind::NoGpu => "irodori_no_gpu",
                UnavailableKind::Vram => "irodori_vram",
                UnavailableKind::Other => "irodori_unavailable",
            },
            NoticeKind::IrodoriUpdateAvailable { .. } => "irodori_update_available",
            NoticeKind::IrodoriDlComplete => "irodori_dl_complete",
            NoticeKind::IrodoriDlFailed { .. } => "irodori_dl_failed",
            NoticeKind::UpdateAvailable { .. } => "update_available",
            NoticeKind::CalendarFetchFailed { .. } => "calendar_fetch_failed",
            NoticeKind::IrodoriVoiceRefMissing => "irodori_voice_ref_missing",
            NoticeKind::GhostVoiceUnset => "ghost_voice_unset",
        }
    }

    fn fallback_text(&self) -> String {
        match self {
            NoticeKind::CostWarning80 { provider } => {
                format!("LLM 月次コストが上限の 80% に到達しました ({provider})")
            }
            NoticeKind::CostLimitExceeded { provider } => {
                format!("LLM 月次コストが上限を超過しました ({provider})。低負荷モードに降格します")
            }
            NoticeKind::CostUnknown { provider } => {
                format!("LLM 月次コストを集計できません ({provider})。上限を守れないため低負荷モードで動きます")
            }
            NoticeKind::ModeDegraded { reason } => match reason {
                DegradeReason::ApiError => "API エラーが続いたので一時的に低負荷モードへ切り替えました".to_string(),
                DegradeReason::CostLimit => "コスト上限超過により低負荷モードへ降格しました".to_string(),
            },
            NoticeKind::ModeRecovered => "通常モードに復帰しました".to_string(),
            NoticeKind::VoicevoxDlComplete => "VOICEVOX の音声資産ダウンロードが完了しました".to_string(),
            NoticeKind::VoicevoxDlFailed { reason } => {
                format!("VOICEVOX 音声資産のダウンロードに失敗しました: {reason}")
            }
            NoticeKind::IrodoriUnavailable { reason, .. } => {
                format!("Irodori-TTS が利用できません: {reason}。VOICEVOX 経路で発話します")
            }
            NoticeKind::IrodoriUpdateAvailable { targets } => format!(
                "Irodori-TTS のランタイムに更新があります（{}）。設定の「更新する」から入れられます",
                targets.join(" / ")
            ),
            NoticeKind::IrodoriDlComplete => {
                "Irodori-TTS の Python ランタイム導入が完了しました".to_string()
            }
            NoticeKind::IrodoriDlFailed { reason } => {
                format!("Irodori-TTS の導入に失敗しました: {reason}")
            }
            NoticeKind::UpdateAvailable { version } => {
                format!("ugg の新しいバージョン {version} が出ています")
            }
            NoticeKind::CalendarFetchFailed { sources } => format!(
                "カレンダーの予定を取れていません（{}）。設定の「いま取得」で確かめられます",
                sources.join(" / ")
            ),
            NoticeKind::IrodoriVoiceRefMissing => {
                "このゴーストの高品質モードの声（参照音声）がまだ無いので、VOICEVOX で話します。設定の「音声」で作れます"
                    .to_string()
            }
            NoticeKind::GhostVoiceUnset => {
                "声は設定の「音声」で変えられます（いまは初期の話者です）".to_string()
            }
        }
    }
}

/// 告知がどうなったか（v0.5.6 項目 6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeOutcome {
    /// 吹き出しかトーストで出した。
    Shown,
    /// ウインドウが見えていない（隠している・最小化している）ので出さずに保留した。
    Held,
    /// 出せなかった（イベントを送れない）。
    Failed,
}

impl NoticeOutcome {
    /// ユーザーに届いたか。**済みにしてよいのはこのときだけ。**
    pub fn reached(self) -> bool {
        self == NoticeOutcome::Shown
    }
}

/// 告知の出し方（純粋部分）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Hold,
    Speak,
    Toast,
}

/// 見えていなければ保留（§4.6.1 の配達と同じ「見えない間は届いていない」）。見えていれば辞書の行を
/// 喋り、辞書に無ければトーストへ落とす（§3.1 のフォールバック）。
fn route(visible: bool, has_line: bool) -> Route {
    match (visible, has_line) {
        (false, _) => Route::Hold,
        (true, true) => Route::Speak,
        (true, false) => Route::Toast,
    }
}

/// 1 回だけ出す告知を、**届いたときにだけ済みにする**（v0.5.6 項目 6、spec §6.0）。
///
/// `done` が真なら何もしない（`None`）。`show` の結果が届いた（`Shown`）ときだけ `mark` を呼ぶ。
/// 保留（見えていない）・失敗なら済みにしないので、次の機会（次のコスト判定・次の更新確認）にもう一度出る。
/// 以前は 4 か所の呼び出し元が届いたかを見ずに済みにしていた（うち 3 か所は発話より前に）ので、隠している
/// 間に出ると、コストの 2 件はその月、集計不能はその起動のあいだ、アプリ更新はその版では二度と出なかった。
pub(crate) async fn once_reached<Fut>(
    done: bool,
    show: impl FnOnce() -> Fut,
    mark: impl FnOnce(),
) -> Option<NoticeOutcome>
where
    Fut: std::future::Future<Output = NoticeOutcome>,
{
    if done {
        return None;
    }
    let outcome = show().await;
    if outcome.reached() {
        mark();
    }
    Some(outcome)
}

/// 鍵ごとに 1 回だけ出す告知の**原子的な取り合い**（v0.6.0 項目 2・3、spec §6.0）。
///
/// main と sub を並べて合成するので、「済みかを見てから出して、済みにする」では、見てから立てるまでの間に
/// もう 1 つが同じ鍵を見て 2 回出る。**見るのと取るのを 1 回の操作にする**（`try_begin`）。取ったあと
/// 届かなければ（保留・失敗）手放し（`release`）、次の機会にもう一度出る。届いたら済み（`mark_done`）。
/// 済みはプロセスの中だけ（ゴースト × slot の参照音声の告知。再起動したらまた 1 回出てよい）。
#[derive(Default)]
pub struct OnceGate {
    /// 鍵 → 済みか（false = 出している最中）。
    slots: std::sync::Mutex<std::collections::BTreeMap<String, bool>>,
}

impl OnceGate {
    /// 取れたら真。済み・出している最中なら偽（取れた 1 つだけが出す）。
    pub fn try_begin(&self, key: &str) -> bool {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if slots.contains_key(key) {
            return false;
        }
        slots.insert(key.to_string(), false);
        true
    }

    fn mark_done(&self, key: &str) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.insert(key.to_string(), true);
    }

    fn release(&self, key: &str) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.remove(key);
    }
}

/// `once_reached` に**取り合い**（`OnceGate`）を足したもの（v0.6.0 項目 2・3）。`done`（呼び出し元が持つ済みの
/// 記録。無ければ偽）が真なら取らずに何もしない。取れなければ `None`（ほかが出している・出した）。取れたら
/// `show` を実行し、**届いたときだけ** `mark` を呼んで済みにする。届かなければ手放す。
pub(crate) async fn once_reached_claimed<Fut>(
    gate: &OnceGate,
    key: &str,
    done: bool,
    show: impl FnOnce() -> Fut,
    mark: impl FnOnce(),
) -> Option<NoticeOutcome>
where
    Fut: std::future::Future<Output = NoticeOutcome>,
{
    if done || !gate.try_begin(key) {
        return None;
    }
    let outcome = once_reached(
        false,
        show,
        || {
            mark();
            gate.mark_done(key);
        },
    )
    .await;
    if !outcome.is_some_and(NoticeOutcome::reached) {
        gate.release(key);
    }
    outcome
}

/// 見えていない間に保留をログへ書いた告知（同じ告知の保留を毎回書かない）。見えている間に告知を出したら空にする。
static HELD_LOGGED: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// この告知の保留をまだ書いていなければ真（純粋部分）。
fn first_time_held(logged: &mut std::collections::BTreeSet<String>, text: &str) -> bool {
    logged.insert(text.to_string())
}

pub async fn notify(app: &AppHandle, state: &Arc<AppState>, kind: NoticeKind) -> NoticeOutcome {
    // **理由をログに残す** (v0.5.5 項目 1、spec §6.0)。
    // ユーザーに見せるのはキャラの台詞（辞書の行）でよいが、**辞書キーが存在すると
    // `fallback_text()` が使われず、`reason` がどこにも残らなかった**。
    // 「Irodori-TTS が利用できません」とだけ出て、原因は永久に分からない状態だった。
    // 載せる理由に会話の本文は入らない。合成失敗（`IrodoriUnavailable`）は生成元で伏字・
    // 切り詰め済み（`sanitize_sidecar_error`）。資産 DL の失敗（`VoicevoxDlFailed` /
    // `IrodoriDlFailed`）は通信・ファイル操作のエラーで、そもそも会話を含まない（切り詰めもしない）。
    let text = kind.fallback_text();
    // **見えていない間は出さない**（v0.5.6 項目 6）。以前は見えているかを見ずに出していたので、隠している
    // 間の告知は誰にも届かないまま（1 回だけの告知は）済みになった。
    if route(crate::system::deliver::window_is_visible(app), true) == Route::Hold {
        // **同じ告知の保留は 1 回だけ書く**（v0.5.6 リリース前監査）。1 回だけの告知は済みにしないので、
        // 呼び出し元が毎分試すと（上限で止まった独り言の補充）、隠している間じゅう毎分 2 行ずつ増え、
        // `ugg.log`（2MB・1 世代）の診断に要る古い行を押し出した。
        let mut logged = HELD_LOGGED.lock().unwrap_or_else(|e| e.into_inner());
        if first_time_held(&mut logged, &text) {
            crate::ulog!("[notify] {text}");
            crate::ulog!("[notify] ウインドウが見えていないので出さずに保留します（同じ告知の保留は、見えるまで書き直しません）");
        }
        return NoticeOutcome::Held;
    }
    HELD_LOGGED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    crate::ulog!("[notify] {text}");
    let key = kind.dict_key();
    let line = {
        let guard = state.ghost.lock().expect("ghost poisoned");
        match guard.as_ref() {
            Ok(b) => b
                .dictionary
                .pick_system_message(key, &WhenContext::now(), b.sub_available()),
            Err(_) => None,
        }
    };

    let sent = match (route(true, line.is_some()), line) {
        (Route::Speak, Some(line)) => {
            let resp: DialogueResponse = banter::pattern_1("system_message", "low", line);
            app.emit("dialogue", &resp)
                .map_err(|err| crate::ulog!("[notify] dialogue emit failed: {err}"))
        }
        _ => {
            // 辞書未定義 → トースト fallback。フロントが拾わなければ console.error 相当。
            app.emit("system-toast", kind.fallback_text())
                .map_err(|err| crate::ulog!("[notify] toast emit failed: {err}"))
        }
    };
    if sent.is_ok() {
        NoticeOutcome::Shown
    } else {
        NoticeOutcome::Failed
    }
}

#[cfg(test)]
mod delivery_tests {
    use super::*;
    use crate::db::Db;
    use crate::system::cost;

    /// 見えていなければ保留、見えていれば辞書の行を喋る（無ければトースト）。
    #[test]
    fn a_notice_is_held_while_the_window_is_not_visible() {
        assert_eq!(route(false, true), Route::Hold);
        assert_eq!(route(false, false), Route::Hold);
        assert_eq!(route(true, true), Route::Speak);
        assert_eq!(route(true, false), Route::Toast);
        assert!(NoticeOutcome::Shown.reached());
        assert!(!NoticeOutcome::Held.reached(), "保留は届いていない");
        assert!(!NoticeOutcome::Failed.reached());
    }

    /// **同じ告知の保留は 1 回だけ書く**（v0.5.6 リリース前監査）。上限で止まった独り言の補充は毎分試すので、
    /// 隠している間じゅう毎分 2 行ずつ `ugg.log` が増えていた。違う告知は書き、見えている間に出したら数え直す。
    #[test]
    fn a_held_notice_is_logged_once_until_the_window_is_shown() {
        let mut logged = std::collections::BTreeSet::new();
        assert!(first_time_held(&mut logged, "上限を超過しました"));
        assert!(!first_time_held(&mut logged, "上限を超過しました"), "同じ告知は書き直さない");
        assert!(first_time_held(&mut logged, "集計できません"), "違う告知は書く");
        logged.clear(); // 見えている間に出した
        assert!(first_time_held(&mut logged, "上限を超過しました"), "見えたあとの保留はまた書く");
    }

    /// **操作列: 隠している間に告知が来る → 見えるようになってまた来る → もう一度来る**（v0.5.6 項目 6、
    /// test-plan §3.2b）。月 1 回の 80% 警告を、実物の DB と当月タグで流す。隠している間は済みにせず、
    /// 見えたときに出て済みになり、そのあとは出さない。以前は 1 回目（隠している間）で済みになり、その月は
    /// 二度と出なかった。
    #[tokio::test]
    async fn a_notice_held_while_hidden_is_shown_later_and_only_once() {
        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        db.migrate().unwrap();
        let key = cost::KEY_WARNED_80;
        let shown = std::cell::Cell::new(0);
        let tell = |visible: bool| {
            let outcome = if visible {
                shown.set(shown.get() + 1);
                NoticeOutcome::Shown
            } else {
                NoticeOutcome::Held
            };
            async move { outcome }
        };

        let hidden = once_reached(
            cost::notified_this_month(&db, key),
            || tell(false),
            || cost::mark_notified_this_month(&db, key),
        )
        .await;
        assert_eq!(hidden, Some(NoticeOutcome::Held));
        assert!(!cost::notified_this_month(&db, key), "隠している間は済みにしない");

        let visible = once_reached(
            cost::notified_this_month(&db, key),
            || tell(true),
            || cost::mark_notified_this_month(&db, key),
        )
        .await;
        assert_eq!(visible, Some(NoticeOutcome::Shown));
        assert!(cost::notified_this_month(&db, key), "届いたら済みにする");

        let again = once_reached(
            cost::notified_this_month(&db, key),
            || tell(true),
            || cost::mark_notified_this_month(&db, key),
        )
        .await;
        assert_eq!(again, None, "済んだら出さない");
        assert_eq!(shown.get(), 1, "見えている間に出たのは 1 回だけ");
    }

    /// **取り合いは原子的**（v0.6.0 項目 2・3）。main と sub を並べて合成するので、見てから立てると 2 回出る。
    /// 同じ鍵を 16 本が同時に取りに行って、取れるのは 1 本だけ。別の鍵（別のゴースト・別の slot）は別々に取れる。
    #[test]
    fn only_one_caller_wins_the_same_key() {
        let gate = std::sync::Arc::new(OnceGate::default());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let (gate, barrier) = (gate.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    gate.try_begin("voice_ref_missing:g:main")
                })
            })
            .collect();
        let winners = handles.into_iter().map(|h| h.join().unwrap()).filter(|won| *won).count();
        assert_eq!(winners, 1, "同じ鍵を 2 つ以上が取れた");
        assert!(gate.try_begin("voice_ref_missing:g:sub"), "別の slot は別に取れる");
        assert!(gate.try_begin("voice_ref_missing:h:main"), "別のゴーストは別に取れる");
    }

    /// **届いたときだけ済み、届かなければ手放して次の機会にまた出る。出している最中の重なりは出さない**
    /// （v0.6.0 項目 3）。告知の台詞自体の合成が同じ経路に戻ってきても重ならない。
    #[tokio::test]
    async fn a_claimed_notice_is_done_only_when_reached_and_never_overlaps() {
        let gate = OnceGate::default();
        let shown = std::cell::Cell::new(0);
        let marked = std::cell::Cell::new(0);
        let (shown_ref, marked_ref) = (&shown, &marked);
        let run = |outcome: NoticeOutcome| {
            once_reached_claimed(
                &gate,
                "k",
                false,
                move || {
                    shown_ref.set(shown_ref.get() + 1);
                    async move { outcome }
                },
                move || marked_ref.set(marked_ref.get() + 1),
            )
        };

        assert_eq!(run(NoticeOutcome::Held).await, Some(NoticeOutcome::Held));
        assert_eq!(run(NoticeOutcome::Failed).await, Some(NoticeOutcome::Failed));
        assert_eq!(marked.get(), 0, "届いていないのに済みにした");
        assert_eq!(run(NoticeOutcome::Shown).await, Some(NoticeOutcome::Shown));
        assert_eq!(marked.get(), 1);
        assert_eq!(run(NoticeOutcome::Shown).await, None, "済みなのに出した");
        assert_eq!((shown.get(), marked.get()), (3, 1));

        // 済みの記録（呼び出し元が持つ）が真なら、取りもしない
        let g2 = OnceGate::default();
        let r = once_reached_claimed(&g2, "k", true, || async { NoticeOutcome::Shown }, || {}).await;
        assert_eq!(r, None);
        assert!(g2.try_begin("k"), "記録が済みでも取ってしまった");

        // 出している最中（show が待っている間）に同じ鍵が来たら、出さない（main と sub を並べて合成する形）
        let g3 = OnceGate::default();
        let count = std::cell::Cell::new(0);
        let slow = |_n: u8| {
            once_reached_claimed(
                &g3,
                "k",
                false,
                || {
                    count.set(count.get() + 1);
                    async {
                        tokio::task::yield_now().await;
                        NoticeOutcome::Shown
                    }
                },
                || {},
            )
        };
        let (a, b) = tokio::join!(slow(1), slow(2));
        assert_eq!(count.get(), 1, "並べて呼ぶと 2 回出る");
        assert_eq!([a, b].iter().filter(|o| o.is_some()).count(), 1);
    }

    /// **声の告知 2 つは `once_reached_claimed`（取り合い + `once_reached`）を通し、告知を出す**
    /// （v0.6.0 項目 2・3。下の一覧の 1 回だけの告知と同じ配線の契約を、取り合いのある 2 つへ広げる）。
    /// 済みの記録を取る側の中で、告知より前に書く形は許さない。
    #[test]
    fn the_voice_notices_go_through_the_claimed_once() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let src = std::fs::read_to_string(root.join("commands/tts.rs")).unwrap().replace("\r\n", "\n");
        let body_of = |name: &str| -> String {
            let at = src.find(name).unwrap_or_else(|| panic!("{name} が無い"));
            let rest = &src[at..];
            rest[..rest.find("\n}\n").unwrap()].to_string()
        };
        for (name, kind) in [
            ("pub(crate) async fn announce_voice_ref_missing_once(", "NoticeKind::IrodoriVoiceRefMissing"),
            ("pub(crate) async fn announce_ghost_voice_unset_once(", "NoticeKind::GhostVoiceUnset"),
        ] {
            let full = body_of(name);
            // 関数名の行は読まない（名前が取り合いの関数名を含むので、先頭に当たってしまう）
            let body = &full[full.find('\n').unwrap()..];
            // 取り合いは直接か、中身（`ghost_voice_unset_once`。中で `once_reached_claimed` を通す）を通る
            let once = body
                .find("once_reached_claimed(")
                .or_else(|| body.find("ghost_voice_unset_once("))
                .unwrap_or_else(|| panic!("{name}: 取り合いを通していない"));
            let notify_at = body.find("notify::notify(").unwrap_or_else(|| panic!("{name}: 告知を出していない"));
            assert!(body.contains(kind), "{name}: {kind} を出していない");
            assert!(once < notify_at, "{name}: 取る前に告知を出している");
            assert!(!body.contains("swap(true") && !body.contains(".store(true"), "{name}: 出す前に済みにする形");
        }
        // 取り合いの中身は `once_reached` を通す
        let helper = {
            let own = std::fs::read_to_string(root.join("system/notify.rs")).unwrap().replace("\r\n", "\n");
            let at = own.find("pub(crate) async fn once_reached_claimed<").unwrap();
            let rest = &own[at..];
            rest[..rest.find("\n}\n").unwrap()].to_string()
        };
        let claimed = helper.find("try_begin(").expect("取っていない");
        let once = helper.find("once_reached(").expect("once_reached を通していない");
        let released = helper.find("release(").expect("届かなければ手放していない");
        assert!(claimed < once && once < released);
    }

    /// **1 回だけ出す告知の呼び出し元は、すべて `once_reached` を通す**（v0.5.6 項目 6 の配線）。4 か所のうち
    /// 片方だけ直して隣を残す形を繰り返さないため、本文をテキストで見る（AppHandle が要るので単体では通せない）。
    #[test]
    fn every_one_time_notice_is_marked_only_when_reached() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap().replace("\r\n", "\n");
        let body_of = |src: &str, name: &str| -> String {
            let at = src.find(name).unwrap_or_else(|| panic!("{name} が無い"));
            let rest = &src[at..];
            rest[..rest.find("\n}\n").unwrap()].to_string()
        };
        let dialogue = read("dialogue/mod.rs");
        let update = read("system/update.rs");
        let tasks = read("tasks.rs");
        for (src, name, mark) in [
            (&dialogue, "pub(crate) async fn evaluate_cost_status", "mark_notified_this_month(&state.db, cost::KEY_WARNED_80)"),
            (&dialogue, "pub(crate) async fn announce_cost_limit_once", "mark_notified_this_month(&state.db, cost::KEY_LIMIT_NOTIFIED)"),
            (&dialogue, "pub(crate) async fn announce_cost_unknown_once", ".store(true"),
            (&update, "pub async fn check_update_once", "set_setting(&seen_key"),
            // v0.5.7 項目 7
            (&update, "pub async fn check_irodori_update_once", "set_setting(&seen_key"),
            // v0.5.7 項目 9
            (&tasks, "async fn announce_calendar_failures", "failures.mark_announced(&pending)"),
        ] {
            let body = body_of(src, name);
            let once = body
                .find("once_reached(")
                .unwrap_or_else(|| panic!("{name}: once_reached を通していない"));
            let marked = body.find(mark).unwrap_or_else(|| panic!("{name}: {mark} が無い"));
            assert!(once < marked, "{name}: 届いたかを見る前に済みにしている");
            // **済みにするのは告知を出す closure より後ろ**（v0.5.7 の変異テストで発覚）。`once_reached` の最初の
            // 引数（済んでいるかの判定）の中で先に済みにする形は、上の順序だけでは捕まらない。
            let shown = body[once..]
                .find("notify::notify(")
                .map(|at| once + at)
                .unwrap_or_else(|| panic!("{name}: once_reached の中で告知を出していない"));
            assert!(shown < marked, "{name}: 告知を出す前（済んでいるかの判定の中など）で済みにしている");
            assert_eq!(body.matches(mark).count(), 1, "{name}: 済みにする処理が 2 か所ある");
            assert!(!body.contains("swap(true"), "{name}: 出す前に済みにする形（swap）が残っている");
        }
        // notify 自身が見えているかを問うこと（呼び出し元が済みにする前提）
        let notify = body_of(&read("system/notify.rs"), "pub async fn notify(");
        let asks = notify
            .find("route(crate::system::deliver::window_is_visible(app)")
            .expect("notify が見えているかを問うていない");
        let emits = notify.find("app.emit(").unwrap();
        assert!(asks < emits, "出したあとで見えているかを問うている");
        let once = notify.find("first_time_held(").expect("保留のログを 1 回に絞っていない");
        let held = notify.find("return NoticeOutcome::Held").unwrap();
        assert!(asks < once && once < held, "保留の分岐で 1 回だけ書いていない");
    }
}

/// `NoticeKind` が引く辞書キーが、**出荷している既定辞書に実在する**ことの契約テスト。
///
/// `cost_warning_80` / `cost_limit_exceeded` は `dict_key()` が以前から引きに来て
/// いたのに `ghosts/default/dic/main.yaml` に定義が無く、月額上限の 80% 到達も
/// 超過もキャラクターが黙っていた。コンパイルもテストも緑のまま出荷されていた。
#[cfg(test)]
mod dict_key_contract {
    use super::*;
    use std::collections::BTreeSet;

    /// 全変種のサンプル。**変種を増やしたらここにも足すこと。**
    ///
    /// 足し忘れは `sample_covers_every_variant` が件数で捕まえる
    /// （変種を増やすと `dict_key` の match がコンパイルエラーになるので、
    /// そこを直した開発者は必ずテストを走らせることになる）。
    fn all_kinds() -> Vec<NoticeKind> {
        vec![
            NoticeKind::CostWarning80 { provider: "openai".into() },
            NoticeKind::CostLimitExceeded { provider: "openai".into() },
            NoticeKind::CostUnknown { provider: "openai".into() },
            NoticeKind::ModeDegraded { reason: DegradeReason::ApiError },
            NoticeKind::ModeRecovered,
            NoticeKind::VoicevoxDlComplete,
            NoticeKind::VoicevoxDlFailed { reason: "x".into() },
            NoticeKind::IrodoriUnavailable { reason: "x".into(), kind: UnavailableKind::Updating },
            NoticeKind::IrodoriUnavailable { reason: "x".into(), kind: UnavailableKind::ModelMissing },
            NoticeKind::IrodoriUnavailable { reason: "x".into(), kind: UnavailableKind::NoGpu },
            NoticeKind::IrodoriUnavailable { reason: "x".into(), kind: UnavailableKind::Vram },
            NoticeKind::IrodoriUnavailable { reason: "x".into(), kind: UnavailableKind::Other },
            NoticeKind::IrodoriUpdateAvailable { targets: vec!["transformers".into()] },
            NoticeKind::IrodoriDlComplete,
            NoticeKind::IrodoriDlFailed { reason: "x".into() },
            NoticeKind::UpdateAvailable { version: "1.0".into() },
            NoticeKind::CalendarFetchFailed { sources: vec!["calendar.example.com".into()] },
            NoticeKind::IrodoriVoiceRefMissing,
            NoticeKind::GhostVoiceUnset,
        ]
    }

    /// サンプルが全変種を覆っていること。**enum の定義と名前で突き合わせる**（v0.5.7 項目 9）。以前は
    /// サンプルの件数を 16 と書いて比べていたので、変種を足してもサンプルに足し忘れると鳴らなかった
    /// （`CalendarFetchFailed` を足したときに実際に鳴らなかった）。
    #[test]
    fn sample_covers_every_variant() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/system/notify.rs"),
        )
        .unwrap()
        .replace("\r\n", "\n");
        let body = &src[src.find("pub enum NoticeKind {").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let declared: BTreeSet<String> = body
            .lines()
            .filter_map(|l| l.strip_prefix("    "))
            .filter(|l| l.starts_with(|c: char| c.is_ascii_uppercase()))
            .map(|l| l.split(|c: char| !c.is_ascii_alphanumeric()).next().unwrap().to_string())
            .collect();
        let sampled: BTreeSet<String> = all_kinds()
            .iter()
            .map(|k| format!("{k:?}").split(|c: char| !c.is_ascii_alphanumeric()).next().unwrap().to_string())
            .collect();
        assert!(declared.len() >= 12, "enum の変種を読めていない: {declared:?}");
        assert_eq!(sampled, declared, "NoticeKind の変種を増やしたら all_kinds() にも足すこと");
    }

    /// **全変種の辞書キーが既定辞書の system_messages に存在すること。**
    #[test]
    fn every_dict_key_exists_in_default_dictionary() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri の親")
            .join("ghosts/default/dic/main.yaml");
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("既定辞書を読めない {}: {e}", path.display()));
        let doc: serde_yaml::Value =
            serde_yaml::from_str(&raw).expect("既定辞書が YAML として壊れている");
        let sysmsgs = doc
            .get("system_messages")
            .and_then(|v| v.as_mapping())
            .expect("既定辞書に system_messages が無い");

        let missing: Vec<&str> = all_kinds()
            .iter()
            .map(|k| k.dict_key())
            .filter(|key| !sysmsgs.contains_key(serde_yaml::Value::from(*key)))
            .collect();
        assert!(
            missing.is_empty(),
            "Rust が引くのに既定辞書に無い system_messages キー: {missing:?}\n\
             （引けないとゴーストが黙る。辞書に足すこと）"
        );
    }
}

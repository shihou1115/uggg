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
    IrodoriUnavailable {
        reason: String,
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
    // M7: ReminderFired variant は削除した。リマインダー発火は
    // `system::deliver::deliver_event` + 辞書 events.reminder_fired 経路に一本化
    // (daily-support-design §3/§7.1)。
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
            NoticeKind::IrodoriUnavailable { .. } => "irodori_unavailable",
            NoticeKind::IrodoriDlComplete => "irodori_dl_complete",
            NoticeKind::IrodoriDlFailed { .. } => "irodori_dl_failed",
            NoticeKind::UpdateAvailable { .. } => "update_available",
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
            NoticeKind::IrodoriUnavailable { reason } => {
                format!("Irodori-TTS が利用できません: {reason}。VOICEVOX 経路で発話します")
            }
            NoticeKind::IrodoriDlComplete => {
                "Irodori-TTS の Python ランタイム導入が完了しました".to_string()
            }
            NoticeKind::IrodoriDlFailed { reason } => {
                format!("Irodori-TTS の導入に失敗しました: {reason}")
            }
            NoticeKind::UpdateAvailable { version } => {
                format!("ugg の新しいバージョン {version} が出ています")
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
        for (src, name, mark) in [
            (&dialogue, "pub(crate) async fn evaluate_cost_status", "mark_notified_this_month(&state.db, cost::KEY_WARNED_80)"),
            (&dialogue, "pub(crate) async fn announce_cost_limit_once", "mark_notified_this_month(&state.db, cost::KEY_LIMIT_NOTIFIED)"),
            (&dialogue, "pub(crate) async fn announce_cost_unknown_once", ".store(true"),
            (&update, "pub async fn check_update_once", "set_setting(&seen_key"),
        ] {
            let body = body_of(src, name);
            let once = body
                .find("once_reached(")
                .unwrap_or_else(|| panic!("{name}: once_reached を通していない"));
            let marked = body.find(mark).unwrap_or_else(|| panic!("{name}: {mark} が無い"));
            assert!(once < marked, "{name}: 届いたかを見る前に済みにしている");
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
            NoticeKind::IrodoriUnavailable { reason: "x".into() },
            NoticeKind::IrodoriDlComplete,
            NoticeKind::IrodoriDlFailed { reason: "x".into() },
            NoticeKind::UpdateAvailable { version: "1.0".into() },
        ]
    }

    /// サンプルが全変種を覆っていること（件数での歯止め）。
    #[test]
    fn sample_covers_every_variant() {
        let keys: BTreeSet<_> = all_kinds().iter().map(|k| k.dict_key()).collect();
        assert_eq!(
            keys.len(),
            11,
            "NoticeKind の変種を増やしたら all_kinds() にも足すこと（現在のキー: {keys:?}）"
        );
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

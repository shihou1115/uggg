use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::dialogue::{self, low, DialogueResponse};
use crate::ghost::dict::WhenContext;
use crate::state::AppState;

const FIRST_BOOT_KEY: &str = "first_boot_done";

/// 右クリックメニューの「終了」。トレイの「終了」と同じ `quit_with_farewell` を通る（v0.5.6 項目 5）。
#[tauri::command]
pub async fn quit_app(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    quit_with_farewell(app, state.inner().clone());
    Ok(())
}

/// 「終了」がもう押されたか（待っている間の 2 回目を見分ける）。
static QUITTING: AtomicBool = AtomicBool::new(false);

/// 終了する（v0.5.6 項目 5、spec §4.4.2 / §4.3.5 / §4.6.2）。**トレイの「終了」と右クリックメニューの
/// 「終了」が同じここを通る。**
///
/// - **見えていれば**、今日の未完了 ToDo の確認（`events.todo_quit`）か終了のあいさつ（`events.quit`）をしてから終了する
/// - **隠している・最小化しているときは、すぐ終了する**（見えない間は届いていない扱い。§4.6.1 と揃える。
///   以前はトレイの経路が見えているかを見ず、何も表示されないまま最長 8.5 秒止まっていた）
/// - 待っている間に**もう一度「終了」を押すと、すぐ終了する**（待ち時間は文字数からの推定で、合成の
///   完了は待たない。Irodori では台詞が切れうる）
/// - ユーザーの操作への応答なので、発話ガバナンスのゲートは通さない（静音中も鳴る。§4.4.8）
///
/// 以前は右クリックメニューの「終了」が、あいさつも終了前の確認も無く即座に終わり（ウインドウの位置も
/// 保存していなかった）、経路によって挙動が違った。
pub(crate) fn quit_with_farewell(app: AppHandle, state: Arc<AppState>) {
    let pressed_again = QUITTING.swap(true, Ordering::SeqCst);
    let speak = says_farewell(
        crate::system::deliver::window_is_visible(&app),
        pressed_again,
    );
    tauri::async_runtime::spawn(async move {
        if speak {
            if let Some(resp) = say_farewell(&app, &state) {
                tokio::time::sleep(farewell_hold(&resp)).await;
            }
        }
        finish_quit(&app, &state).await;
    });
}

/// あいさつをしてから終わるか（純粋部分）。見えていて、1 回目の「終了」のときだけ。
fn says_farewell(visible: bool, pressed_again: bool) -> bool {
    visible && !pressed_again
}

/// 終了前に喋る辞書のキー（純粋部分）。日常支援が有効で今日の未完了 ToDo があれば確認（§4.6.2）、
/// 無ければあいさつ（§4.4.2）。
fn farewell_key(daily_support_enabled: bool, open_today: u64) -> &'static str {
    if daily_support_enabled && open_today > 0 {
        "todo_quit"
    } else {
        "quit"
    }
}

/// 喋ったあと終了まで待つ時間。フロントの吹き出しの表示時間（約 1.6 秒 + 1 文字 60ms）に余裕を足す。
fn farewell_hold(resp: &DialogueResponse) -> Duration {
    let chars = resp.main.text.chars().count()
        + resp.sub.as_ref().map_or(0, |s| s.text.chars().count());
    Duration::from_millis((1600 + chars as u64 * 60).min(8000) + 500)
}

/// 終了前の確認かあいさつを喋る。喋った内容を返す（辞書に無い・ゴーストを読めていないときは `None`）。
/// `todo_quit` が辞書に無ければ `quit` へ落ちる。
fn say_farewell(app: &AppHandle, state: &Arc<AppState>) -> Option<DialogueResponse> {
    let daily_on = state
        .settings
        .lock()
        .expect("settings poisoned")
        .daily_support_enabled;
    let open_today = if daily_on {
        state.db.count_open_todos(Some("today")).unwrap_or(0)
    } else {
        0
    };
    if farewell_key(daily_on, open_today) == "todo_quit" {
        let count = open_today.to_string();
        if let Some(resp) = crate::system::deliver::speak_event_now(
            app,
            state,
            "todo_quit",
            &[("count", count.as_str())],
        ) {
            return Some(resp);
        }
    }
    let line = {
        let guard = state.ghost.lock().expect("ghost poisoned");
        match guard.as_ref() {
            Ok(b) => low::event(&b.dictionary, "quit", &WhenContext::now(), b.sub_available()),
            Err(_) => None,
        }
    }?;
    dialogue::persist_and_speak(app, state, &line).then_some(line)
}

/// 後片付けをして終了する。
async fn finish_quit(app: &AppHandle, state: &Arc<AppState>) {
    // ウインドウ位置を即時保存してから終了
    crate::presence::window_pos::persist_now(app, state);
    // greeted は先に下ろしておく (短期間で再起動した場合の重複挨拶を防ぐ ―
    // ただし frontend_ready が greeted を見るのは初回のみなので影響軽微)
    state.dialogue.greeted.store(false, Ordering::SeqCst);
    // Irodori サイドカーを best-effort で止める (未起動なら即 return)。止めきれなくても、
    // ugg の寿命に結びつけてあるので一緒に終わる (v0.5.6 項目 4)
    let _ = state.tts.irodori.shutdown().await;
    app.exit(0);
}

#[tauri::command]
pub fn hide_window(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

/// M5-H: 自動起動の有効/無効切替 (`tauri-plugin-autostart` 経由)。
#[tauri::command]
pub fn set_autostart(enabled: bool, app: AppHandle) -> Result<(), String> {
    let manager = app.autolaunch();
    if enabled {
        manager
            .enable()
            .map_err(|e| format!("自動起動の有効化に失敗: {e}"))?;
    } else {
        manager
            .disable()
            .map_err(|e| format!("自動起動の無効化に失敗: {e}"))?;
    }
    Ok(())
}

/// フロントの初期化完了通知。
/// 初回起動なら events.first_boot、それ以外は events.boot を時間帯別に発火させる。
/// 二重呼び出しは greeted ガードで no-op。
#[tauri::command]
pub fn frontend_ready(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    if state.dialogue.greeted.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    let first_boot = match state.db.get_setting(FIRST_BOOT_KEY) {
        Ok(v) => v.is_none(),
        Err(err) => return Err(format!("{err:#}")),
    };

    let bundle_guard = state.ghost.lock().expect("ghost poisoned");
    let bundle = bundle_guard.as_ref().map_err(|s| s.clone())?;
    let ctx = WhenContext::now();
    let response = low::boot_greeting(
        &bundle.dictionary,
        &ctx,
        first_boot,
        bundle.sub_available(),
    );
    drop(bundle_guard);

    if let Some(resp) = response {
        app.emit("dialogue", &resp)
            .map_err(|e| format!("dialogue イベント送信に失敗しました: {e}"))?;
    }

    if first_boot {
        state
            .db
            .set_setting(FIRST_BOOT_KEY, "1")
            .map_err(|e| format!("{e:#}"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **見えていて 1 回目のときだけ、あいさつをしてから終わる**（v0.5.6 項目 5）。隠している・最小化している
    /// ときはすぐ終わる（以前はトレイの経路が見えているかを見ず、何も表示されないまま最長 8.5 秒止まった）。
    /// 待っている間の 2 回目もすぐ終わる。
    #[test]
    fn a_farewell_is_said_only_when_visible_and_pressed_once() {
        assert!(says_farewell(true, false));
        assert!(!says_farewell(false, false), "隠していればすぐ終了");
        assert!(!says_farewell(true, true), "もう一度押したらすぐ終了");
        assert!(!says_farewell(false, true));
    }

    /// 今日の未完了 ToDo があれば終了前の確認、無ければあいさつ（§4.6.2 / §4.4.2）。日常支援が無効なら数えない。
    #[test]
    fn open_todos_today_turn_the_farewell_into_a_check() {
        assert_eq!(farewell_key(true, 2), "todo_quit");
        assert_eq!(farewell_key(true, 0), "quit");
        assert_eq!(farewell_key(false, 2), "quit", "日常支援が無効なら確認しない");
    }

    /// 待ち時間は吹き出しの表示時間に合わせ、長い台詞でも 8.5 秒で打ち切る。
    #[test]
    fn the_wait_follows_the_line_length_up_to_a_cap() {
        let line = |text: &str| -> DialogueResponse {
            crate::dialogue::banter::pattern_1(
                "event",
                "low",
                crate::ghost::dict::DialogueLine {
                    main: crate::ghost::dict::SpeechTurn { text: text.into(), pose: None },
                    sub: None,
                },
            )
        };
        assert_eq!(farewell_hold(&line("またね")), Duration::from_millis(1600 + 3 * 60 + 500));
        assert_eq!(farewell_hold(&line(&"あ".repeat(500))), Duration::from_millis(8500));
    }

    /// **トレイの「終了」と右クリックメニューの「終了」が同じ経路を通る**（v0.5.6 項目 5 の配線）。
    /// 片方だけ直して隣を残す形を繰り返さないため、本文をテキストで見る（AppHandle が要るので単体では通せない）。
    #[test]
    fn both_quit_entries_go_through_the_farewell() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap().replace("\r\n", "\n");
        let body_of = |src: &str, name: &str| -> String {
            let at = src.find(name).unwrap_or_else(|| panic!("{name} が無い"));
            let rest = &src[at..];
            rest[..rest.find("\n}\n").unwrap()].to_string()
        };

        let lifecycle = read("commands/lifecycle.rs");
        let quit_app = body_of(&lifecycle, "pub async fn quit_app");
        assert!(quit_app.contains("quit_with_farewell("), "メニューの終了が共通の経路を通っていない");
        assert!(!quit_app.contains("app.exit("), "メニューの終了がすぐ終わる形に戻っている");

        let tray = read("window/tray.rs");
        assert!(
            tray.contains("lifecycle::quit_with_farewell("),
            "トレイの終了が共通の経路を通っていない"
        );
        assert!(!tray.contains("fn quit_with_farewell"), "トレイに別の終了の経路が残っている");

        let quit = body_of(&lifecycle, "pub(crate) fn quit_with_farewell");
        assert!(quit.contains("window_is_visible("), "見えているかを見ていない");
    }
}

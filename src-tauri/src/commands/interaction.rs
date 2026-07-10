//! つつき (poke) / 撫で (nade) / 入力促し (input_prompt) コマンド
//! (spec §4.3.1 / §4.3.2 / §4.3.3)。
//!
//! 縦のみ部位判定: head / chest / body。横は廃止。
//! 探索順: `<event>_<target>_<region>` → `<event>_<target>`。
//! poke の rapid=true (4 回以上連打) は `poke_rapid` を先に探す。

use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::db::ChatRole;
use crate::dialogue::{self, banter, DialogueResponse};
use crate::ghost::dict::{SpeechTurn, WhenContext};
use crate::state::AppState;

#[tauri::command]
pub fn poke(
    app: AppHandle,
    target: String,
    region: String,
    rapid: bool,
    state: State<'_, Arc<AppState>>,
) -> Option<DialogueResponse> {
    let s = state.inner().clone();
    let resp = lookup_interaction(&s, "poke", &target, &region, rapid);
    if let Some(r) = &resp {
        dialogue::persist_and_speak(&app, &s, r);
        crate::presence::idle::reset(&s);
    }
    resp
}

#[tauri::command]
pub fn nade(
    app: AppHandle,
    target: String,
    region: String,
    state: State<'_, Arc<AppState>>,
) -> Option<DialogueResponse> {
    let s = state.inner().clone();
    let resp = lookup_interaction(&s, "nade", &target, &region, false);
    if let Some(r) = &resp {
        dialogue::persist_and_speak(&app, &s, r);
        crate::presence::idle::reset(&s);
    }
    resp
}

/// キャラクリック時の入力促し (spec §4.3.1)。クリックされた側だけの単発ターンを返す。
/// 発話レンダリングはフロント (renderPrompt) が行うため dialogue イベントは emit しない。
#[tauri::command]
pub fn input_prompt(target: String, state: State<'_, Arc<AppState>>) -> Option<SpeechTurn> {
    prompt_from_dictionary(state.inner(), &target, |dict, t| dict.pick_input_prompt(t))
}

/// キャラ右クリック時のメニュー前口上 (main) / メインへの誘導 (sub)。spec §4.3.5。
/// メニュー本体の描画はフロント (バルーン内メニュー) が行う。
#[tauri::command]
pub fn menu_prompt(target: String, state: State<'_, Arc<AppState>>) -> Option<SpeechTurn> {
    prompt_from_dictionary(state.inner(), &target, |dict, t| dict.pick_menu_prompt(t))
}

/// 促し系コマンドの共通処理: 辞書抽選 → chat_log へ 1 行記録 (emit はしない)。
/// 辞書にセクションが無い・sub 無しゴーストの sub は None。
fn prompt_from_dictionary(
    state: &Arc<AppState>,
    target: &str,
    pick: impl Fn(&crate::ghost::dict::Dictionary, &str) -> Option<SpeechTurn>,
) -> Option<SpeechTurn> {
    let turn = {
        let guard = state.ghost.lock().expect("ghost poisoned");
        let bundle = guard.as_ref().ok()?;
        if target == "sub" && !bundle.sub_available() {
            return None;
        }
        pick(&bundle.dictionary, target)
    }?;
    let role = if target == "sub" {
        ChatRole::Sub
    } else {
        ChatRole::Main
    };
    let now = chrono::Utc::now().timestamp();
    let _ = state
        .db
        .append_chat(now, "low", role, &turn.text, turn.pose.as_deref());
    crate::presence::idle::reset(state);
    Some(turn)
}

/// 辞書探索 → DialogueResponse 組立。
/// `kind` は "poke" / "nade"。rapid=true で poke のみ poke_rapid 先行。
fn lookup_interaction(
    state: &Arc<AppState>,
    kind: &str,
    target: &str,
    region: &str,
    rapid: bool,
) -> Option<DialogueResponse> {
    let guard = state.ghost.lock().expect("ghost poisoned");
    let bundle = guard.as_ref().ok()?;
    let sub_available = bundle.sub_available();
    let ctx = WhenContext::now();
    let dict = &bundle.dictionary;

    let mut keys: Vec<String> = Vec::new();
    if rapid && kind == "poke" {
        keys.push("poke_rapid".to_string());
    }
    if !region.is_empty() {
        keys.push(format!("{kind}_{target}_{region}"));
    }
    keys.push(format!("{kind}_{target}"));

    for key in keys {
        if let Some(line) = dict.pick_event(&key, &ctx, sub_available) {
            return Some(banter::pattern_1("event", "low", line));
        }
    }
    None
}

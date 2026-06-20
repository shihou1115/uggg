//! つつき (poke) / 撫で (nade) コマンド (spec §4.3.2 / §4.3.3)。
//!
//! 縦のみ部位判定: head / chest / body。横は廃止。
//! 探索順: `<event>_<target>_<region>` → `<event>_<target>`。
//! poke の rapid=true (4 回以上連打) は `poke_rapid` を先に探す。

use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::dialogue::{self, banter, DialogueResponse};
use crate::ghost::dict::WhenContext;
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

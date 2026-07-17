use crate::dialogue::{banter, DialogueResponse};
use crate::ghost::dict::{DialogueLine, Dictionary, SpeechTurn, WhenContext};

/// ユーザー入力に対する辞書ベース応答を組み立てる。
/// マッチも fallback も無ければ汎用文「…」を返してフロントに「沈黙」を見せない。
pub fn reply(dict: &Dictionary, text: &str, sub_available: bool) -> DialogueResponse {
    let line = dict
        .pick_reply(text, sub_available)
        .unwrap_or_else(|| default_silence_line());
    banter::pattern_1("reply", "low", line)
}

/// 起動挨拶。`first` が true なら first_boot を、無ければ boot を選択。
/// どちらも候補が無ければ None。
pub fn boot_greeting(
    dict: &Dictionary,
    ctx: &WhenContext,
    first: bool,
    sub_available: bool,
) -> Option<DialogueResponse> {
    let key = if first { "first_boot" } else { "boot" };
    let line = dict.pick_event(key, ctx, sub_available)?;
    Some(banter::pattern_1("event", "low", line))
}

/// 任意イベントキー (idle / quit / focus_start 等) を low 発話として組み立てる。
pub fn event(
    dict: &Dictionary,
    key: &str,
    ctx: &WhenContext,
    sub_available: bool,
) -> Option<DialogueResponse> {
    let line = dict.pick_event(key, ctx, sub_available)?;
    Some(banter::pattern_1("event", "low", line))
}

// M7: monologue ヘルパは削除した。独り言は system::deliver::deliver_event が
// dict.pick_monologue を直接使う (ガバナンスゲート下、daily-support-design §4.2)。

fn default_silence_line() -> DialogueLine {
    DialogueLine {
        main: SpeechTurn {
            text: "……".to_string(),
            pose: None,
        },
        sub: None,
    }
}

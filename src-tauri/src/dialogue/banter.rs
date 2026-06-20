use rand::Rng;

use crate::dialogue::DialogueResponse;
use crate::ghost::dict::DialogueLine;

/// 掛け合いパターン 1 (main → sub) 固定の発話を組み立てる。
/// low モード / event 系 / system_message は常にこれ。
pub fn pattern_1(kind: &'static str, mode: &'static str, line: DialogueLine) -> DialogueResponse {
    DialogueResponse {
        kind,
        mode,
        pattern: 1,
        main: line.main,
        sub: line.sub,
    }
}

/// advanced 用パターン抽選。
/// 重み付け (architecture §4.2.4 「番号が小さいほど高確率」):
///   1: 50%, 2: 25%, 3: 15%, 4: 10%
/// サブ無しゴーストは常に 1。
pub fn pick_advanced_pattern(sub_available: bool) -> u8 {
    if !sub_available {
        return 1;
    }
    let r: f64 = rand::thread_rng().gen_range(0.0..1.0);
    if r < 0.50 {
        1
    } else if r < 0.75 {
        2
    } else if r < 0.90 {
        3
    } else {
        4
    }
}

/// advanced モードの応答にパターン番号を付ける。
/// `extra` 相当の 3 ターン目は M3 以降の課題のため、3/4 は 1/2 にフォールバックする。
pub fn assemble_advanced(line: DialogueLine, sub_available: bool) -> DialogueResponse {
    let pattern = pick_advanced_pattern(sub_available);
    let effective = match pattern {
        3 => 1,
        4 => 2,
        n => n,
    };
    DialogueResponse {
        kind: "reply",
        mode: "advanced",
        pattern: effective,
        main: line.main,
        sub: line.sub,
    }
}

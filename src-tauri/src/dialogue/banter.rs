use rand::Rng;

use crate::dialogue::DialogueResponse;
use crate::ghost::dict::{DialogueLine, SpeechTurn};

/// 掛け合いパターン 1 (main → sub) 固定の発話を組み立てる。
/// low モード / event 系 / system_message は常にこれ。
pub fn pattern_1(kind: &'static str, mode: &'static str, line: DialogueLine) -> DialogueResponse {
    DialogueResponse {
        kind,
        mode,
        pattern: 1,
        main: line.main,
        sub: line.sub,
        extra: None,
        speech_id: None,
        category: None,
        priority: None,
        feedback_allowed: None,
    }
}

/// advanced 用パターン抽選。LLM 呼び出し前に `advanced::reply` から呼ばれ、
/// 結果を system prompt に反映する (パターン3/4 は3ターン目の構成を LLM に指示するため)。
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

/// advanced モードの応答を組み立てる。`pattern` は LLM 呼び出し前に
/// `pick_advanced_pattern` で決定済みのもの。パターン3/4 は3ターン目 (`extra`) を
/// LLM に要求しているが、返さなかった / 空文字だった場合はここで 3→1・4→2 に
/// 縮退する (安全縮退、spec §4.2.4)。
///
/// 縮退条件に `sub` の有無も含める: パターン3/4 は定義上 main と sub の 3 ターン構成
/// (main→sub→main / sub→main→sub) なので、LLM が `sub` を落とした応答で 3/4 を維持すると
/// 「main が 2 つの吹き出しで連続発話」「sub が 1 ターン目を飛ばして extra 枠だけで喋る」
/// という spec §4.2.4 に反する表示になる (リリース前レビュー指摘)。
pub fn assemble_advanced(
    pattern: u8,
    line: DialogueLine,
    extra: Option<SpeechTurn>,
) -> DialogueResponse {
    let (effective_pattern, effective_extra) = match pattern {
        3 | 4 if extra.is_some() && line.sub.is_some() => (pattern, extra),
        3 => (1, None),
        4 => (2, None),
        n => (n, None),
    };
    DialogueResponse {
        kind: "reply",
        mode: "advanced",
        pattern: effective_pattern,
        main: line.main,
        sub: line.sub,
        extra: effective_extra,
        speech_id: None,
        category: None,
        priority: None,
        feedback_allowed: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line() -> DialogueLine {
        DialogueLine {
            main: SpeechTurn {
                text: "main".into(),
                pose: None,
            },
            sub: Some(SpeechTurn {
                text: "sub".into(),
                pose: None,
            }),
        }
    }

    #[test]
    fn pick_advanced_pattern_without_sub_is_always_1() {
        for _ in 0..50 {
            assert_eq!(pick_advanced_pattern(false), 1);
        }
    }

    #[test]
    fn pick_advanced_pattern_stays_within_1_to_4() {
        for _ in 0..500 {
            let p = pick_advanced_pattern(true);
            assert!((1..=4).contains(&p), "pattern {p} out of range");
        }
    }

    #[test]
    fn pattern_1_has_no_extra() {
        let resp = pattern_1("reply", "low", line());
        assert_eq!(resp.pattern, 1);
        assert!(resp.extra.is_none());
    }

    #[test]
    fn assemble_advanced_pattern_1_and_2_never_carry_extra() {
        let extra = Some(SpeechTurn {
            text: "余計".into(),
            pose: None,
        });
        let resp = assemble_advanced(1, line(), extra.clone());
        assert_eq!(resp.pattern, 1);
        assert!(resp.extra.is_none());
        let resp = assemble_advanced(2, line(), extra);
        assert_eq!(resp.pattern, 2);
        assert!(resp.extra.is_none());
    }

    #[test]
    fn assemble_advanced_pattern_3_with_extra_is_kept() {
        let extra = Some(SpeechTurn {
            text: "main2".into(),
            pose: None,
        });
        let resp = assemble_advanced(3, line(), extra);
        assert_eq!(resp.pattern, 3);
        assert_eq!(resp.extra.unwrap().text, "main2");
    }

    #[test]
    fn assemble_advanced_pattern_4_with_extra_is_kept() {
        let extra = Some(SpeechTurn {
            text: "sub2".into(),
            pose: None,
        });
        let resp = assemble_advanced(4, line(), extra);
        assert_eq!(resp.pattern, 4);
        assert_eq!(resp.extra.unwrap().text, "sub2");
    }

    #[test]
    fn assemble_advanced_pattern_3_without_extra_degrades_to_1() {
        let resp = assemble_advanced(3, line(), None);
        assert_eq!(resp.pattern, 1);
        assert!(resp.extra.is_none());
    }

    #[test]
    fn assemble_advanced_pattern_4_without_extra_degrades_to_2() {
        let resp = assemble_advanced(4, line(), None);
        assert_eq!(resp.pattern, 2);
        assert!(resp.extra.is_none());
    }
}

//! advanced モードの応答生成。
//! 1) システムプロンプト + 履歴 + ユーザー入力 を LLM に投げる
//! 2) LLM から JSON 応答を取り出して DialogueLine に変換
//! 3) コスト記録、chat_log への保存

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::Deserialize;

use crate::db::{ApiUsageRow, ChatRole, Db, ProfileOrigin};
use crate::dialogue::llm::{
    estimate_cost_usd, extract_json_blob, ChatMessage, ChatResponse, LlmClient,
};
use crate::dialogue::{banter, DialogueResponse};
use crate::ghost::dict::{DialogueLine, SpeechTurn};
use crate::ghost::GhostBundle;
use crate::state::Settings;

/// 1 ターン分のユーザー入力 → DialogueResponse。
/// `usage` は記録目的で AdvancedReply に同梱するが、現状は cost.rs 側で `api_usage` テーブルへ
/// 直接書き込む経路があり、構造体としては未使用。デバッグ計装・将来の UI 計測用に残す。
pub struct AdvancedReply {
    pub response: DialogueResponse,
    #[allow(dead_code)]
    pub usage: ReplyUsage,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct ReplyUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_usd: f64,
}

pub async fn reply(
    settings: &Settings,
    bundle: &GhostBundle,
    db: &Db,
    api_key: Option<String>,
    user_text: &str,
) -> Result<AdvancedReply> {
    let client = LlmClient::new(settings.llm_base_url.clone(), api_key);
    // パターンは LLM 呼び出し前に決めて system prompt に反映する (3/4 は3ターン構成を指示する)。
    let pattern = banter::pick_advanced_pattern(bundle.sub_available());
    let messages = build_messages(bundle, db, user_text, settings.tools_enabled, pattern)?;
    let response = client.chat(&settings.llm_model, messages).await?;
    parse_and_record(response, bundle, db, settings, user_text, pattern).await
}

async fn parse_and_record(
    response: ChatResponse,
    bundle: &GhostBundle,
    db: &Db,
    settings: &Settings,
    user_text: &str,
    pattern: u8,
) -> Result<AdvancedReply> {
    let raw = response
        .choices
        .first()
        .ok_or_else(|| anyhow!("LLM 応答に choices が含まれていません"))?
        .message
        .content
        .clone();

    // JSON で返ればそれを使う。小さいモデルは指示に従えずプレーンテキストを返すことが
    // あるので、その場合は生テキストを main の発話として扱う (low へ落とさず LLM 応答を活かす)。
    let parsed = match parse_dialogue_json(&raw, bundle, pattern) {
        Ok(p) => p,
        Err(err) => {
            let fallback = plaintext_fallback(&raw)
                .ok_or_else(|| anyhow!("LLM 応答が JSON でもテキストでもありません: {err:#}"))?;
            eprintln!("[advanced] JSON パース失敗、プレーンテキストとして表示: {err:#}");
            fallback
        }
    };
    let line = parsed.line;
    let extra = parsed.extra;

    let prompt_tokens = response.usage.map(|u| u.prompt_tokens).unwrap_or(0);
    let completion_tokens = response.usage.map(|u| u.completion_tokens).unwrap_or(0);
    let cost = estimate_cost_usd(&settings.llm_model, prompt_tokens, completion_tokens);

    let now = Utc::now().timestamp();
    // chat_log: user → main → sub の順で記録。掛け合いパターン3/4 では 3ターン目
    // (extra) を話者と同じロール (パターン3=Main / パターン4=Sub) で追記するため最大 4 行。
    db.append_chat(now, "advanced", ChatRole::User, user_text, None)?;
    db.append_chat(
        now,
        "advanced",
        ChatRole::Main,
        &line.main.text,
        line.main.pose.as_deref(),
    )?;
    if let Some(sub) = &line.sub {
        db.append_chat(
            now,
            "advanced",
            ChatRole::Sub,
            &sub.text,
            sub.pose.as_deref(),
        )?;
    }
    // 3ターン目 (パターン3=main の再発話 / パターン4=sub の再発話) も同じ話者役として記録する。
    if let Some(extra) = &extra {
        let role = if pattern == 3 { ChatRole::Main } else { ChatRole::Sub };
        db.append_chat(now, "advanced", role, &extra.text, extra.pose.as_deref())?;
    }
    // api_usage: 0 トークンでも 1 行残しておく (回数監視)
    db.append_api_usage(&ApiUsageRow {
        provider: settings.llm_provider.clone(),
        model: settings.llm_model.clone(),
        prompt_tokens: prompt_tokens as i64,
        completion_tokens: completion_tokens as i64,
        cost_usd: cost,
        ts: now,
    })?;

    // 自動抽出: LLM が memory を返したら user_profile に origin=auto で保存。
    if let Some(memory) = parsed.memory {
        let memory = memory.trim();
        if !memory.is_empty() {
            db.insert_profile(memory, ProfileOrigin::Auto, None, now)?;
            // 容量管理 (low モードと同じ件数上限ベースで簡易実装)。
            // 要約サイクル (advanced 用) は将来課題。
            enforce_profile_capacity(db, settings.profile_max_count)?;
        }
    }

    let response = banter::assemble_advanced(pattern, line, extra);
    Ok(AdvancedReply {
        response,
        usage: ReplyUsage {
            prompt_tokens,
            completion_tokens,
            cost_usd: cost,
        },
    })
}

fn enforce_profile_capacity(db: &Db, max_count: u32) -> Result<()> {
    let auto_count = db.count_profile_origin(ProfileOrigin::Auto)?;
    if auto_count > max_count as u64 {
        let to_drop = auto_count - max_count as u64;
        db.prune_oldest_auto(to_drop)?;
    }
    Ok(())
}

fn build_messages(
    bundle: &GhostBundle,
    db: &Db,
    user_text: &str,
    tools_enabled: bool,
    pattern: u8,
) -> Result<Vec<ChatMessage>> {
    let mut out = Vec::new();
    let profile_block = render_profile_block(db)?;
    let tools_block = if tools_enabled {
        render_tools_block(db)
    } else {
        String::new()
    };
    out.push(ChatMessage::system(system_prompt(
        bundle,
        pattern,
        &profile_block,
        &tools_block,
    )));

    // M2 初期: 履歴注入は最小限。中長期記憶は user_profile (system prompt) でカバー。
    for hist in load_recent_history(db, 8)? {
        out.push(hist);
    }

    out.push(ChatMessage::user(user_text.to_string()));
    Ok(out)
}

fn render_profile_block(db: &Db) -> Result<String> {
    let entries = db.list_profile().unwrap_or_default();
    if entries.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::from("\n知っているユーザー情報 (この情報を活かして自然に話す):\n");
    for e in entries {
        // 100 文字を超える要素は念のため切り詰め
        let content = if e.content.chars().count() > 200 {
            e.content.chars().take(200).collect::<String>() + "…"
        } else {
            e.content.clone()
        };
        out.push_str(&format!("- {content}\n"));
    }
    Ok(out)
}

/// M5-B: tools_enabled のときに system prompt に注入する補助情報。
fn render_tools_block(db: &Db) -> String {
    let now_label = crate::tools::clock::now_jp_label();
    let now_ts = chrono::Utc::now().timestamp();
    let mut out = format!("\n[現在] {now_label}\n");
    let pending = db
        .list_reminders(crate::db::ReminderFilter::Active)
        .unwrap_or_default();
    let upcoming: Vec<_> = pending
        .into_iter()
        .filter(|r| r.due_ts > now_ts && r.due_ts - now_ts < 24 * 3600)
        .collect();
    if !upcoming.is_empty() {
        out.push_str("[保留中のリマインダー (24 時間以内)]\n");
        for r in upcoming {
            let mins = (r.due_ts - now_ts).max(0) / 60;
            out.push_str(&format!("- 約 {mins} 分後: {}\n", r.text));
        }
    }
    out
}

fn system_prompt(bundle: &GhostBundle, pattern: u8, profile_block: &str, tools_block: &str) -> String {
    let main_name = bundle.ghost.characters.main.name.as_str();
    let sub_name = bundle
        .ghost
        .characters
        .sub
        .as_ref()
        .map(|s| s.name.as_str())
        .unwrap_or("sub");
    // ghost.json の persona を載せる (spec §4.2)。無ければ従来の役割だけの既定文。
    // persona 不在だと LLM は 2 人を区別できず、掛け合い (§4.2.4) が
    // 「同調役が 2 人いる」状態に潰れる (2026-09-02 の実測で確認)。
    let main_desc = bundle.ghost.characters.main.describe("メインキャラ。");
    let sub_block = match &bundle.ghost.characters.sub {
        Some(sub) => format!(
            "- 「{}」(sub): {}",
            sub.name,
            sub.describe("デスクトップに住む相方キャラ。")
        ),
        None => String::new(),
    };
    // prompt.max_chars_per_line / style_notes も同様に作者指定を優先する。
    let gp = bundle.ghost.prompt.as_ref();
    let main_len_line = match gp.and_then(|p| p.max_chars_per_line) {
        Some(n) => format!("main は {n} 文字以内、sub は 1 行程度。"),
        None => "main は 1-2 行、sub は 1 行程度。".to_string(),
    };
    let style_line = gp
        .and_then(|p| p.style_notes.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("2 人で掛け合うように自然な会話にする。説教くさい長文は禁止。")
        .to_string();
    let available_poses = available_pose_names(bundle);
    let sub_required_line = if bundle.sub_available() {
        format!(
            "- sub: \"{sub_name}\" の台詞 (短く 1 行)、pose は {available_poses}"
        )
    } else {
        "- sub: null (サブキャラ無しゴーストのため必ず null)".to_string()
    };
    // 掛け合いパターン (spec §4.2.4)。パターン3/4 のみ3ターン目 (extra) を要求する。
    let turn_structure_line = if bundle.sub_available() {
        match pattern {
            2 => format!(
                "- 会話の流れ: \"{sub_name}\" が話しかけ、\"{main_name}\" が短く応じる (sub → main の順)。"
            ),
            3 => format!(
                "- 会話の流れ: \"{main_name}\" が話しかけ、\"{sub_name}\" が応じ、最後にもう一度 \"{main_name}\" が続きを話す (main → sub → main の3ターン構成)。"
            ),
            4 => format!(
                "- 会話の流れ: \"{sub_name}\" が話しかけ、\"{main_name}\" が応じ、最後にもう一度 \"{sub_name}\" が続きを話す (sub → main → sub の3ターン構成)。"
            ),
            _ => format!(
                "- 会話の流れ: \"{main_name}\" が話しかけ、\"{sub_name}\" が短く応じる (main → sub の順)。"
            ),
        }
    } else {
        String::new()
    };
    let sub_poses = sub_pose_names(bundle);
    let (extra_field, extra_required_line) = match pattern {
        3 => (
            format!("\n  \"extra\":  {{ \"text\": \"...\", \"pose\": \"<pose>\" }},"),
            format!(
                "- extra: \"{main_name}\" の2回目の発言 (1-2 行、3ターン目の締め)、pose は {available_poses}\n"
            ),
        ),
        4 => (
            format!("\n  \"extra\":  {{ \"text\": \"...\", \"pose\": \"<pose>\" }},"),
            // パターン4 の3ターン目は sub の発言なので **sub の pose 集合**を提示する。
            // 検証側 (`validate_pose(.., extra_is_main=false)`) が sub の集合で照合するため、
            // main の語彙を出すと main/sub で pose 名が違うシェルでは必ず drop される
            // (リリース前レビュー指摘)。
            format!(
                "- extra: \"{sub_name}\" の2回目の発言 (1 行程度、3ターン目の締め)、pose は {sub_poses}\n"
            ),
        ),
        _ => (String::new(), String::new()),
    };
    format!(
        r#"あなたはデスクトップマスコットアプリ「{ghost}」のキャラクターです。
登場人物:
- 「{main}」(main): {main_desc}
{sub_block}
{profile_block}{tools_block}
応答ルール:
- 1 ターンは短く: {main_len_line}
- {style_line}
- 上の人物設定に従って話す。設定文そのものを台詞で説明しない。
- 既存ユーザー情報を尊重し、それを基に親密に話す。
- 新しく覚えるべきユーザー情報があれば memory に 1 文だけ書く。無ければ memory は空文字。
{turn_structure_line}

出力形式: 必ず次の JSON のみを返す。前置き / 後置き / マークダウン禁止。
{{
  "main":   {{ "text": "...", "pose": "<pose>" }},
  "sub":    {{ "text": "...", "pose": "<pose>" }},{extra_field}
  "memory": ""
}}
{sub_required_line}
{extra_required_line}- pose に使えるのは次のいずれか: {available_poses}
"#,
        ghost = bundle.ghost.name,
        main = main_name,
        main_desc = main_desc,
        sub_block = sub_block,
        main_len_line = main_len_line,
        style_line = style_line,
        profile_block = profile_block,
        tools_block = tools_block,
        turn_structure_line = turn_structure_line,
        sub_required_line = sub_required_line,
        extra_field = extra_field,
        extra_required_line = extra_required_line,
        available_poses = available_poses,
    )
}

/// main キャラの pose 名一覧 (プロンプトで LLM に提示する語彙)。
/// M14 の独り言生成 (`system::monologue`) も同じ語彙を使う。
pub(crate) fn available_pose_names(bundle: &GhostBundle) -> String {
    let mut names: Vec<&str> = bundle
        .shell
        .characters
        .main
        .poses
        .keys()
        .map(|s| s.as_str())
        .collect();
    names.sort();
    names.dedup();
    names.join(" / ")
}

/// sub キャラの pose 名一覧 (パターン4 の3ターン目のプロンプト提示用)。
/// sub 未定義のシェルでは main の集合にフォールバックする (パターン4 自体が
/// `pick_advanced_pattern` で選ばれないため実際には到達しない)。
fn sub_pose_names(bundle: &GhostBundle) -> String {
    let Some(sub) = bundle.shell.characters.sub.as_ref() else {
        return available_pose_names(bundle);
    };
    let mut names: Vec<&str> = sub.poses.keys().map(|s| s.as_str()).collect();
    names.sort();
    names.dedup();
    names.join(" / ")
}

fn load_recent_history(_db: &Db, _max: usize) -> Result<Vec<ChatMessage>> {
    // M2 初期: 履歴注入を簡略化し、毎ターンこの場の入力だけで応答させる。
    // 中長期記憶は user_profile (system prompt に注入予定) でカバーする方針。
    // 履歴の本格注入は M2-I 完了後に検討。
    Ok(Vec::new())
}

// ===== JSON パース =====

#[derive(Debug, Deserialize)]
struct ParsedResponse {
    main: ParsedTurn,
    #[serde(default)]
    sub: Option<ParsedTurn>,
    /// パターン3/4 の3ターン目。他パターンでは無視する。
    #[serde(default)]
    extra: Option<ParsedTurn>,
    /// memory: 自動抽出された記憶。空文字 / 省略時は保存しない。
    #[serde(default)]
    memory: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ParsedTurn {
    text: String,
    #[serde(default)]
    pose: Option<String>,
}

pub struct ParsedAdvanced {
    pub line: DialogueLine,
    /// パターン3/4 の3ターン目。それ以外のパターン、または LLM が返さなかった場合は None。
    pub extra: Option<SpeechTurn>,
    pub memory: Option<String>,
}

/// LLM が JSON を返さなかったときのフォールバック。
/// 生テキストを main 単独の発話にする。空なら None。
/// ```json フェンスや前後の空白は剥がしておく。
fn plaintext_fallback(raw: &str) -> Option<ParsedAdvanced> {
    let text = extract_json_blob(raw).trim().to_string();
    if text.is_empty() {
        return None;
    }
    Some(ParsedAdvanced {
        line: DialogueLine {
            main: SpeechTurn { text, pose: None },
            sub: None,
        },
        extra: None,
        memory: None,
    })
}

/// `pattern` はパターン3/4 のときだけ `extra` を取り出す (pose 検証はその話者キャラの
/// pose 集合で行う: パターン3=main、パターン4=sub)。それ以外のパターンでは extra を無視する
/// (LLM が誤って含めても捨てる)。
fn parse_dialogue_json(raw: &str, bundle: &GhostBundle, pattern: u8) -> Result<ParsedAdvanced> {
    let json = extract_json_blob(raw);
    let parsed: ParsedResponse = serde_json::from_str(json)
        .with_context(|| format!("JSON 構造が想定と違います: {json}"))?;

    let main = SpeechTurn {
        text: parsed.main.text.trim().to_string(),
        pose: validate_pose(parsed.main.pose, bundle, true),
    };
    if main.text.is_empty() {
        return Err(anyhow!("main.text が空でした"));
    }

    let sub = if bundle.sub_available() {
        parsed.sub.and_then(|s| {
            let text = s.text.trim().to_string();
            if text.is_empty() {
                None
            } else {
                Some(SpeechTurn {
                    text,
                    pose: validate_pose(s.pose, bundle, false),
                })
            }
        })
    } else {
        None
    };

    let extra = if matches!(pattern, 3 | 4) {
        let extra_is_main = pattern == 3;
        parsed.extra.and_then(|e| {
            let text = e.text.trim().to_string();
            if text.is_empty() {
                None
            } else {
                Some(SpeechTurn {
                    text,
                    pose: validate_pose(e.pose, bundle, extra_is_main),
                })
            }
        })
    } else {
        None
    };

    Ok(ParsedAdvanced {
        line: DialogueLine { main, sub },
        extra,
        memory: parsed.memory,
    })
}

fn validate_pose(pose: Option<String>, bundle: &GhostBundle, is_main: bool) -> Option<String> {
    let poses = if is_main {
        &bundle.shell.characters.main.poses
    } else {
        match &bundle.shell.characters.sub {
            Some(sub) => &sub.poses,
            None => return None,
        }
    };
    pose.filter(|p| poses.contains_key(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use crate::ghost::manifest::{BaseSize, GhostCharacter, GhostCharacters, GhostManifest, GhostPrompt, ShellCharacterDef, ShellCharacters, ShellManifest};

    fn make_bundle(with_sub: bool) -> GhostBundle {
        let mut poses = BTreeMap::new();
        poses.insert("normal".into(), "main/normal.png".into());
        poses.insert("happy".into(), "main/happy.png".into());
        poses.insert("troubled".into(), "main/troubled.png".into());
        poses.insert("surprised".into(), "main/surprised.png".into());
        let main_def = ShellCharacterDef {
            base_size: BaseSize { width: 280, height: 420 },
            default_pose: "normal".into(),
            poses: poses.clone(),
            poke_regions: Default::default(),
        };
        let sub_def = if with_sub {
            Some(ShellCharacterDef {
                base_size: BaseSize { width: 240, height: 360 },
                default_pose: "normal".into(),
                poses: poses.clone(),
                poke_regions: Default::default(),
            })
        } else {
            None
        };
        let ghost = GhostManifest {
            schema_version: 1,
            id: "default".into(),
            name: "ミミとクロ".into(),
            characters: GhostCharacters {
                main: GhostCharacter {
                    name: "ミミ".into(),
                    persona: Some("元気で好奇心旺盛な女の子。一人称は「あたし」。".into()),
                },
                sub: if with_sub {
                    Some(GhostCharacter {
                        name: "クロ".into(),
                        persona: Some("冷静でちょっと毒舌な黒猫。一人称は「ボク」。".into()),
                    })
                } else {
                    None
                },
            },
            dictionaries: vec!["dic/main.yaml".into()],
            prompt: Some(GhostPrompt {
                max_chars_per_line: Some(60),
                style_notes: Some("二人の掛け合いとして自然に。".into()),
            }),
        };
        let shell = ShellManifest {
            schema_version: 1,
            id: "default".into(),
            name: "デフォルト".into(),
            characters: ShellCharacters {
                main: main_def,
                sub: sub_def,
            },
        };
        GhostBundle {
            ghost,
            shell,
            shell_dir: PathBuf::from(""),
            dictionary: empty_dict(),
        }
    }

    fn empty_dict() -> crate::ghost::dict::Dictionary {
        crate::ghost::dict::Dictionary {
            schema_version: 3,
            input_match: vec![],
            fallback: vec![],
            recall: vec![],
            monologue: vec![],
            events: Default::default(),
            system_messages: Default::default(),
            input_prompt_main: vec![],
            input_prompt_sub: vec![],
            menu_prompt_main: vec![],
            menu_prompt_sub: vec![],
        }
    }

    #[test]
    fn parse_with_sub() {
        let raw = r#"{"main":{"text":"こんにちは","pose":"happy"},"sub":{"text":"どうも","pose":"normal"}}"#;
        let parsed = parse_dialogue_json(raw, &make_bundle(true), 1).unwrap();
        assert_eq!(parsed.line.main.text, "こんにちは");
        assert_eq!(parsed.line.main.pose.as_deref(), Some("happy"));
        assert!(parsed.line.sub.is_some());
        assert_eq!(parsed.line.sub.unwrap().text, "どうも");
    }

    #[test]
    fn parse_without_sub_when_no_sub_in_shell() {
        let raw = r#"{"main":{"text":"こん","pose":"normal"},"sub":{"text":"無視","pose":"normal"}}"#;
        let parsed = parse_dialogue_json(raw, &make_bundle(false), 1).unwrap();
        assert!(parsed.line.sub.is_none());
    }

    #[test]
    fn invalid_pose_dropped() {
        let raw = r#"{"main":{"text":"hi","pose":"wink"}}"#;
        let parsed = parse_dialogue_json(raw, &make_bundle(true), 1).unwrap();
        assert!(parsed.line.main.pose.is_none(), "未知 pose は drop されるべき");
    }

    #[test]
    fn fenced_json_supported() {
        let raw = "```json\n{\"main\":{\"text\":\"ok\"}}\n```";
        let parsed = parse_dialogue_json(raw, &make_bundle(true), 1).unwrap();
        assert_eq!(parsed.line.main.text, "ok");
    }

    #[test]
    fn empty_main_text_is_error() {
        let raw = r#"{"main":{"text":""}}"#;
        assert!(parse_dialogue_json(raw, &make_bundle(true), 1).is_err());
    }

    #[test]
    fn memory_captured_when_present() {
        let raw = r#"{"main":{"text":"hi"},"memory":"ユーザーは犬好き"}"#;
        let parsed = parse_dialogue_json(raw, &make_bundle(true), 1).unwrap();
        assert_eq!(parsed.memory.as_deref(), Some("ユーザーは犬好き"));
    }

    #[test]
    fn memory_absent_when_missing() {
        let raw = r#"{"main":{"text":"hi"}}"#;
        let parsed = parse_dialogue_json(raw, &make_bundle(true), 1).unwrap();
        assert!(parsed.memory.is_none());
    }

    #[test]
    fn plaintext_fallback_uses_raw_as_main() {
        let raw = "こんにちは！私はミミです。";
        let parsed = plaintext_fallback(raw).unwrap();
        assert_eq!(parsed.line.main.text, "こんにちは！私はミミです。");
        assert!(parsed.line.sub.is_none());
        assert!(parsed.line.main.pose.is_none());
    }

    #[test]
    fn plaintext_fallback_empty_is_none() {
        assert!(plaintext_fallback("   ").is_none());
    }

    // ===== パターン3/4 の3ターン目 (extra) =====

    #[test]
    fn pattern3_extracts_extra() {
        let raw = r#"{"main":{"text":"main1"},"sub":{"text":"sub1"},"extra":{"text":"main2","pose":"happy"}}"#;
        let parsed = parse_dialogue_json(raw, &make_bundle(true), 3).unwrap();
        let extra = parsed.extra.expect("pattern3 は extra を持つはず");
        assert_eq!(extra.text, "main2");
        assert_eq!(extra.pose.as_deref(), Some("happy"));
    }

    #[test]
    fn pattern4_extracts_extra() {
        let raw = r#"{"main":{"text":"main1"},"sub":{"text":"sub1"},"extra":{"text":"sub2","pose":"troubled"}}"#;
        let parsed = parse_dialogue_json(raw, &make_bundle(true), 4).unwrap();
        let extra = parsed.extra.expect("pattern4 は extra を持つはず");
        assert_eq!(extra.text, "sub2");
        assert_eq!(extra.pose.as_deref(), Some("troubled"));
    }

    #[test]
    fn pattern1_ignores_extra_even_if_present() {
        // LLM が誤って extra を含めても、パターン1/2 では無視する。
        let raw = r#"{"main":{"text":"main1"},"extra":{"text":"余計な3ターン目"}}"#;
        let parsed = parse_dialogue_json(raw, &make_bundle(true), 1).unwrap();
        assert!(parsed.extra.is_none());
    }

    #[test]
    fn pattern3_missing_extra_field_is_none() {
        // LLM が extra を返さなかった場合 (安全縮退の入力側)。
        let raw = r#"{"main":{"text":"main1"},"sub":{"text":"sub1"}}"#;
        let parsed = parse_dialogue_json(raw, &make_bundle(true), 3).unwrap();
        assert!(parsed.extra.is_none());
    }

    #[test]
    fn pattern3_extra_invalid_pose_dropped_using_main_pose_set() {
        // pose 検証は話者キャラ (パターン3=main) の pose 集合で行う。
        let raw = r#"{"main":{"text":"main1"},"extra":{"text":"main2","pose":"wink"}}"#;
        let parsed = parse_dialogue_json(raw, &make_bundle(true), 3).unwrap();
        let extra = parsed.extra.unwrap();
        assert!(extra.pose.is_none(), "未知 pose は drop されるべき");
    }

    #[test]
    fn assemble_advanced_keeps_pattern3_when_extra_present() {
        let line = DialogueLine {
            main: SpeechTurn { text: "main1".into(), pose: None },
            sub: Some(SpeechTurn { text: "sub1".into(), pose: None }),
        };
        let extra = Some(SpeechTurn { text: "main2".into(), pose: None });
        let resp = banter::assemble_advanced(3, line, extra);
        assert_eq!(resp.pattern, 3);
        assert_eq!(resp.extra.unwrap().text, "main2");
    }

    #[test]
    fn assemble_advanced_degrades_pattern3_to_1_when_extra_missing() {
        let line = DialogueLine {
            main: SpeechTurn { text: "main1".into(), pose: None },
            sub: Some(SpeechTurn { text: "sub1".into(), pose: None }),
        };
        let resp = banter::assemble_advanced(3, line, None);
        assert_eq!(resp.pattern, 1);
        assert!(resp.extra.is_none());
    }

    #[test]
    fn assemble_advanced_degrades_pattern4_to_2_when_extra_missing() {
        let line = DialogueLine {
            main: SpeechTurn { text: "main1".into(), pose: None },
            sub: Some(SpeechTurn { text: "sub1".into(), pose: None }),
        };
        let resp = banter::assemble_advanced(4, line, None);
        assert_eq!(resp.pattern, 2);
        assert!(resp.extra.is_none());
    }

    // ===== system_prompt のパターン別出し分け =====
    // 3ターン目が実際に出るかは「LLM に extra を要求できているか」の 1 点に依存する。
    // ここが壊れても縮退が効いて 264 テストは緑のまま静かに旧挙動へ戻るため、
    // プロンプト文字列を直接検証して分岐落ち・format! の取り違えを検知する。

    fn prompt_for(pattern: u8) -> String {
        system_prompt(&make_bundle(true), pattern, "", "")
    }

    /// プロンプト中の「- extra:」の行 (3ターン目の指示) を取り出す。
    fn extra_line(prompt: &str) -> String {
        prompt
            .lines()
            .find(|l| l.starts_with("- extra:"))
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn system_prompt_requests_extra_only_for_pattern3_and_4() {
        // 1/2 で extra を要求しないのは、実際に使うのが 25% だけでトークンと
        // 応答時間を無駄にしないための設計判断 (architecture §10.4)。
        for p in [1u8, 2] {
            assert!(
                !prompt_for(p).contains("\"extra\""),
                "pattern {p} は3ターン目を要求しないはず"
            );
        }
        for p in [3u8, 4] {
            assert!(
                prompt_for(p).contains("\"extra\""),
                "pattern {p} は3ターン目を要求するはず"
            );
        }
    }

    #[test]
    fn system_prompt_states_turn_structure_per_pattern() {
        assert!(prompt_for(1).contains("main → sub の順"));
        assert!(prompt_for(2).contains("sub → main の順"));
        assert!(prompt_for(3).contains("main → sub → main の3ターン構成"));
        assert!(prompt_for(4).contains("sub → main → sub の3ターン構成"));
    }

    // ===== 3ターン目の pose 語彙 (話者キャラの集合であること) =====
    // 既定 fixture は sub の poses が main の clone なので、語彙の取り違えを検知できない。
    // main/sub で異なる pose を持つ bundle を別に用意する。

    fn make_bundle_distinct_poses() -> GhostBundle {
        let mut bundle = make_bundle(true);
        let mut main_poses = BTreeMap::new();
        main_poses.insert("normal".into(), "main/normal.png".into());
        main_poses.insert("main_only".into(), "main/main_only.png".into());
        let mut sub_poses = BTreeMap::new();
        sub_poses.insert("normal".into(), "sub/normal.png".into());
        sub_poses.insert("sub_only".into(), "sub/sub_only.png".into());
        bundle.shell.characters.main.poses = main_poses;
        if let Some(sub) = bundle.shell.characters.sub.as_mut() {
            sub.poses = sub_poses;
        }
        bundle
    }

    #[test]
    fn system_prompt_extra_line_uses_speaker_pose_vocabulary() {
        let bundle = make_bundle_distinct_poses();
        // パターン3 の3ターン目は main の発言 → main の pose 集合
        let p3 = extra_line(&system_prompt(&bundle, 3, "", ""));
        assert!(p3.contains("main_only"), "p3 の extra 行: {p3}");
        assert!(!p3.contains("sub_only"), "p3 の extra 行: {p3}");
        // パターン4 の3ターン目は sub の発言 → sub の pose 集合
        // (検証側 validate_pose も sub の集合で照合するため、ここが main だと必ず drop される)
        let p4 = extra_line(&system_prompt(&bundle, 4, "", ""));
        assert!(p4.contains("sub_only"), "p4 の extra 行: {p4}");
        assert!(!p4.contains("main_only"), "p4 の extra 行: {p4}");
    }

    #[test]
    fn extra_pose_validated_against_speaker_pose_set() {
        let bundle = make_bundle_distinct_poses();
        let with_pose = |pose: &str| {
            format!(
                r#"{{"main":{{"text":"m"}},"sub":{{"text":"s"}},"extra":{{"text":"e","pose":"{pose}"}}}}"#
            )
        };
        // パターン3 (話者=main): main 固有 pose は保持、sub 固有 pose は drop
        let kept = parse_dialogue_json(&with_pose("main_only"), &bundle, 3).unwrap();
        assert_eq!(kept.extra.unwrap().pose.as_deref(), Some("main_only"));
        let dropped = parse_dialogue_json(&with_pose("sub_only"), &bundle, 3).unwrap();
        assert!(dropped.extra.unwrap().pose.is_none());
        // パターン4 (話者=sub): 対称
        let kept4 = parse_dialogue_json(&with_pose("sub_only"), &bundle, 4).unwrap();
        assert_eq!(kept4.extra.unwrap().pose.as_deref(), Some("sub_only"));
        let dropped4 = parse_dialogue_json(&with_pose("main_only"), &bundle, 4).unwrap();
        assert!(dropped4.extra.unwrap().pose.is_none());
    }

    // ===== 安全縮退のトリガ (spec §4.2.4 の3ターン構成が成立しない入力) =====

    #[test]
    fn extra_with_blank_text_is_treated_as_missing() {
        // JSON 文字列としての表記。"\\n" は JSON のエスケープ (パース後は改行 1 文字)。
        for blank in ["", "   ", "\\n", "\\t "] {
            let raw = format!(
                r#"{{"main":{{"text":"m"}},"sub":{{"text":"s"}},"extra":{{"text":"{blank}"}}}}"#
            );
            let parsed = parse_dialogue_json(&raw, &make_bundle(true), 3).unwrap();
            assert!(parsed.extra.is_none(), "空白のみの extra は None のはず: {blank:?}");
        }
    }

    #[test]
    fn assemble_advanced_degrades_when_sub_missing_even_with_extra() {
        // パターン3/4 は定義上 main と sub の3ターン構成 (main→sub→main / sub→main→sub)。
        // LLM が sub を落とした応答で 3/4 を維持すると「main が2つの吹き出しで連続発話」
        // 「sub が1ターン目を飛ばして extra 枠だけで喋る」という spec §4.2.4 違反の表示になる。
        let line_without_sub = || DialogueLine {
            main: SpeechTurn { text: "main1".into(), pose: None },
            sub: None,
        };
        let extra = || Some(SpeechTurn { text: "extra1".into(), pose: None });

        let r3 = banter::assemble_advanced(3, line_without_sub(), extra());
        assert_eq!(r3.pattern, 1, "sub 欠落時のパターン3 は 1 へ縮退する");
        assert!(r3.extra.is_none(), "縮退時は3ターン目も落とす");

        let r4 = banter::assemble_advanced(4, line_without_sub(), extra());
        assert_eq!(r4.pattern, 2, "sub 欠落時のパターン4 は 2 へ縮退する");
        assert!(r4.extra.is_none());
    }

    /// persona / style_notes / max_chars_per_line が system prompt に載ること。
    ///
    /// これが無いと LLM は 2 人を名前でしか区別できず、掛け合い (spec §4.2.4) が
    /// 「同調役が 2 人いる」状態に潰れる。2026-09-02 の実測 (ローカル LLM・8 往復 x2)
    /// では、persona 不在だと一人称の一致が main/sub とも 0/8 だった。
    #[test]
    fn system_prompt_carries_ghost_persona_and_prompt_hints() {
        let bundle = make_bundle(true);
        let p = system_prompt(&bundle, 1, "", "");
        assert!(p.contains("元気で好奇心旺盛な女の子"), "main の persona が無い: {p}");
        assert!(p.contains("冷静でちょっと毒舌な黒猫"), "sub の persona が無い: {p}");
        assert!(p.contains("60 文字以内"), "max_chars_per_line が効いていない: {p}");
        assert!(p.contains("二人の掛け合いとして自然に"), "style_notes が無い: {p}");
        // 人格を台詞で復唱させない歯止め。
        assert!(p.contains("設定文そのものを台詞で説明しない"), "自己申告の抑止が無い: {p}");
        // 旧ハードコード文が残っていないこと。
        assert!(!p.contains("(main): メインキャラ。"), "ハードコードが残っている: {p}");
        assert!(!p.contains("デスクトップに住む相方キャラ。"), "ハードコードが残っている: {p}");
    }

    /// persona / prompt を書いていないゴーストでも既定文で成立すること。
    #[test]
    fn system_prompt_falls_back_when_ghost_omits_persona() {
        let mut bundle = make_bundle(true);
        bundle.ghost.characters.main.persona = None;
        bundle.ghost.characters.sub.as_mut().unwrap().persona = None;
        bundle.ghost.prompt = None;
        let p = system_prompt(&bundle, 1, "", "");
        assert!(p.contains("(main): メインキャラ。"), "既定文が出ない: {p}");
        assert!(p.contains("デスクトップに住む相方キャラ。"), "既定文が出ない: {p}");
        assert!(p.contains("main は 1-2 行"), "既定の長さ指示が出ない: {p}");
    }

    #[test]
    fn assemble_advanced_keeps_pattern4_when_sub_and_extra_present() {
        let line = DialogueLine {
            main: SpeechTurn { text: "main1".into(), pose: None },
            sub: Some(SpeechTurn { text: "sub1".into(), pose: None }),
        };
        let resp = banter::assemble_advanced(4, line, Some(SpeechTurn { text: "sub2".into(), pose: None }));
        assert_eq!(resp.pattern, 4);
        assert_eq!(resp.extra.unwrap().text, "sub2");
    }
}


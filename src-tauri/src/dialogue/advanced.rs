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
    let messages = build_messages(bundle, db, user_text, settings.tools_enabled)?;
    let response = client.chat(&settings.llm_model, messages).await?;
    parse_and_record(response, bundle, db, settings, user_text).await
}

async fn parse_and_record(
    response: ChatResponse,
    bundle: &GhostBundle,
    db: &Db,
    settings: &Settings,
    user_text: &str,
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
    let parsed = match parse_dialogue_json(&raw, bundle) {
        Ok(p) => p,
        Err(err) => {
            let fallback = plaintext_fallback(&raw)
                .ok_or_else(|| anyhow!("LLM 応答が JSON でもテキストでもありません: {err:#}"))?;
            eprintln!("[advanced] JSON パース失敗、プレーンテキストとして表示: {err:#}");
            fallback
        }
    };
    let line = parsed.line;

    let prompt_tokens = response.usage.map(|u| u.prompt_tokens).unwrap_or(0);
    let completion_tokens = response.usage.map(|u| u.completion_tokens).unwrap_or(0);
    let cost = estimate_cost_usd(&settings.llm_model, prompt_tokens, completion_tokens);

    let now = Utc::now().timestamp();
    // chat_log: user → main → sub の順で 3 行記録
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

    let response = banter::assemble_advanced(line, bundle.sub_available());
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
    let pending = db.list_reminders().unwrap_or_default();
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

fn system_prompt(bundle: &GhostBundle, profile_block: &str, tools_block: &str) -> String {
    let main_name = bundle.ghost.characters.main.name.as_str();
    let sub_block = match &bundle.ghost.characters.sub {
        Some(sub) => format!(
            "- 「{}」(sub): デスクトップに住む相方キャラ。",
            sub.name
        ),
        None => String::new(),
    };
    let available_poses = available_pose_names(bundle);
    let sub_required_line = if bundle.sub_available() {
        format!(
            "- sub: \"{}\" の台詞 (短く 1 行)、pose は {}",
            bundle
                .ghost
                .characters
                .sub
                .as_ref()
                .map(|s| s.name.as_str())
                .unwrap_or("sub"),
            available_poses
        )
    } else {
        "- sub: null (サブキャラ無しゴーストのため必ず null)".to_string()
    };
    format!(
        r#"あなたはデスクトップマスコットアプリ「{ghost}」のキャラクターです。
登場人物:
- 「{main}」(main): メインキャラ。
{sub_block}
{profile_block}{tools_block}
応答ルール:
- 1 ターンは短く: main は 1-2 行、sub は 1 行程度。
- 2 人で掛け合うように自然な会話にする。説教くさい長文は禁止。
- 既存ユーザー情報を尊重し、それを基に親密に話す。
- 新しく覚えるべきユーザー情報があれば memory に 1 文だけ書く。無ければ memory は空文字。

出力形式: 必ず次の JSON のみを返す。前置き / 後置き / マークダウン禁止。
{{
  "main":   {{ "text": "...", "pose": "<pose>" }},
  "sub":    {{ "text": "...", "pose": "<pose>" }},
  "memory": ""
}}
{sub_required_line}
- pose に使えるのは次のいずれか: {available_poses}
"#,
        ghost = bundle.ghost.name,
        main = main_name,
        sub_block = sub_block,
        profile_block = profile_block,
        tools_block = tools_block,
        sub_required_line = sub_required_line,
        available_poses = available_poses,
    )
}

fn available_pose_names(bundle: &GhostBundle) -> String {
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
        memory: None,
    })
}

fn parse_dialogue_json(raw: &str, bundle: &GhostBundle) -> Result<ParsedAdvanced> {
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

    Ok(ParsedAdvanced {
        line: DialogueLine { main, sub },
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

    use crate::ghost::manifest::{
        BaseSize, GhostCharacter, GhostCharacters, GhostManifest, ShellCharacterDef,
        ShellCharacters, ShellManifest,
    };

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
                main: GhostCharacter { name: "ミミ".into() },
                sub: if with_sub {
                    Some(GhostCharacter { name: "クロ".into() })
                } else {
                    None
                },
            },
            dictionaries: vec!["dic/main.yaml".into()],
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
        let parsed = parse_dialogue_json(raw, &make_bundle(true)).unwrap();
        assert_eq!(parsed.line.main.text, "こんにちは");
        assert_eq!(parsed.line.main.pose.as_deref(), Some("happy"));
        assert!(parsed.line.sub.is_some());
        assert_eq!(parsed.line.sub.unwrap().text, "どうも");
    }

    #[test]
    fn parse_without_sub_when_no_sub_in_shell() {
        let raw = r#"{"main":{"text":"こん","pose":"normal"},"sub":{"text":"無視","pose":"normal"}}"#;
        let parsed = parse_dialogue_json(raw, &make_bundle(false)).unwrap();
        assert!(parsed.line.sub.is_none());
    }

    #[test]
    fn invalid_pose_dropped() {
        let raw = r#"{"main":{"text":"hi","pose":"wink"}}"#;
        let parsed = parse_dialogue_json(raw, &make_bundle(true)).unwrap();
        assert!(parsed.line.main.pose.is_none(), "未知 pose は drop されるべき");
    }

    #[test]
    fn fenced_json_supported() {
        let raw = "```json\n{\"main\":{\"text\":\"ok\"}}\n```";
        let parsed = parse_dialogue_json(raw, &make_bundle(true)).unwrap();
        assert_eq!(parsed.line.main.text, "ok");
    }

    #[test]
    fn empty_main_text_is_error() {
        let raw = r#"{"main":{"text":""}}"#;
        assert!(parse_dialogue_json(raw, &make_bundle(true)).is_err());
    }

    #[test]
    fn memory_captured_when_present() {
        let raw = r#"{"main":{"text":"hi"},"memory":"ユーザーは犬好き"}"#;
        let parsed = parse_dialogue_json(raw, &make_bundle(true)).unwrap();
        assert_eq!(parsed.memory.as_deref(), Some("ユーザーは犬好き"));
    }

    #[test]
    fn memory_absent_when_missing() {
        let raw = r#"{"main":{"text":"hi"}}"#;
        let parsed = parse_dialogue_json(raw, &make_bundle(true)).unwrap();
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
}

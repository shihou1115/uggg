//! advanced 独り言の LLM 生成・キャッシュ補充・時事ネタ織り込み
//! (M14、spec §4.4.4 / §4.4.6、foundation-design §3.3〜§3.5)。
//!
//! **本モジュールは「配達する文の出どころ」を増やすだけ**で、発話可否には一切関与しない
//! (spec §4.6.3 原則)。発火間隔・静音・busy 直列化は従来どおり `tasks::spawn_random_talk`
//! と `system::deliver` の単一ゲートが持つ。
//!
//! AI 非依存 (spec §4.2.1): 独り言の基盤は辞書のまま。mode != advanced・降格中・
//! API 障害・上限超過・キャッシュ空のいずれでも `deliver::resolve_line` が辞書の
//! `pick_monologue` に落ちるので、この経路が丸ごと死んでも独り言は止まらない。
//!
//! 補充は**発話とは独立**に走る (発話の直前に LLM を待たない)。1 回の呼び出しで
//! 複数件まとめて生成し、DB (`monologue_cache`) に積む。ストックは DB なので
//! 再起動をまたいで保持される (spec §4.4.4 の要求)。

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::Deserialize;
use tauri::AppHandle;

use crate::db::{ApiUsageRow, TopicCacheRow, TOPIC_MAX_AGE_SECS};
use crate::dialogue::advanced::available_pose_names;
use crate::dialogue::llm::{estimate_cost_usd, extract_json_blob, ChatMessage, LlmClient};
use crate::ghost::GhostBundle;
use crate::state::{AppState, DialogueMode, Settings};
use crate::system::{cost, secrets};

/// この件数を下回ったら補充する (foundation-design §3.3 の初期値 3)。
const REFILL_THRESHOLD: u64 = 3;
/// 1 回の LLM 呼び出しで作らせる件数 (同 5)。発話頻度より呼び出し頻度を低く保つための束。
const REFILL_BATCH: usize = 5;
/// ストック上限 (foundation-design §7-6)。閾値 3 + バッチ 5 なら定常は 8 件前後で、
/// 20 は「積み過ぎない」ための安全網 (プロンプト変更や再試行で膨らんだときに効く)。
const STOCK_MAX: u64 = 20;
/// 補充の最短間隔 (秒、同 30 分)。失敗が続いても呼び出しが詰まらないようにする。
const REFILL_MIN_INTERVAL_SECS: i64 = 30 * 60;
/// 補充の LLM 呼び出しタイムアウト (秒)。
///
/// 当初は `regular_talk::polish_script` と同じ 20 秒にしていたが、**実機で短すぎた**
/// (2026-08-17)。polish_script は「一文の言い換え」で出力が数十トークンなのに対し、
/// こちらは**独り言を 5 件まとめて生成**するので出力量が桁違いになる。
/// LM Studio の実測ではウォーム状態でも 22〜44 秒かかり (推論するモデルは更に伸びる)、
/// **ローカル LLM では補充が絶対に成功しない**状態だった。`LlmClient` 自身が
/// 「ローカル LLM は初回ロード + 推論で 60 秒を超えることがある」として全体 180 秒を
/// 取っているのと整合させる (spec §4.2.2 は LMStudio/Ollama を正式サポートしている)。
///
/// 待つのは tick の**発話判定より後**なので、伸びても遅れるのは次 tick の判定だけ
/// (最短間隔 30 分に 1 回・最大 2 分)。既定 10 分間隔の独り言には実質影響しない。
const REFILL_LLM_TIMEOUT_SECS: u64 = 120;
/// プロンプトに渡す見出しの最大件数。多すぎるとトークンを食い、独り言が要約くさくなる。
const TOPIC_MATERIAL_MAX: usize = 5;
/// 鮮度フィルタ前に DB から読む見出しの件数 (新しい順)。
const TOPIC_SCAN_LIMIT: u32 = 20;
/// 織り込みに使う見出しに要求する**最短の残り寿命**。
///
/// 賞味期限ぎりぎり (取得から 6 日 23 時間) の見出しを織り込むと、積んだ文が
/// **発話時失効 (§3.4) でほぼ即座に捨てられ**、在庫が 0 のまま 30 分ごとに補充を
/// 繰り返す「積んでは捨てる」有料ループになる。材料側にこの余裕を要求して断ち切る。
const TOPIC_MIN_REMAINING_SECS: i64 = 24 * 60 * 60;

/// プロンプトに渡す時事ネタ 1 件 (見出しと取得時刻だけ。link は独り言に使わない)。
#[derive(Debug, Clone, PartialEq, Eq)]
struct TopicMaterial {
    headline: String,
    fetched_ts: i64,
}

/// ランダムトーク tick から毎回呼ばれる補充トリガ (foundation-design §3.3)。
///
/// 前段のゲートを 1 つでも外したら**何もせずに返る** — 辞書経路が生きているので実害は無い。
/// ゲート: advanced か / 降格中でないか / 最短間隔を空けたか / ストックが閾値を下回るか /
/// 月額上限を超えていないか。
pub async fn maybe_refill(app: &AppHandle, state: &Arc<AppState>) {
    let settings = state.settings.lock().expect("settings poisoned").clone();
    if !matches!(settings.mode, DialogueMode::Advanced) {
        return;
    }
    // 独り言そのものが無効なら補充もしない (spec §4.4.4「0 設定で無効化可能」)。
    // これが無いと、独り言を切っているユーザーの裏で**絶対に喋られない文のために**
    // 30 分ごとに課金 API が呼ばれ続ける。
    if settings.monologue_interval_min == 0 {
        return;
    }
    // 対話チャットと同じ一時降格状態を尊重する (dialogue::mod の degrade と同じフィールド)。
    let degraded_until = state.dialogue.degraded_until.load(Ordering::SeqCst);
    let now = Utc::now().timestamp();
    if degraded_until != 0 && now < degraded_until {
        return;
    }
    let last_refill = state.dialogue.monologue_refill_ts.load(Ordering::SeqCst);
    if last_refill != 0 && now - last_refill < REFILL_MIN_INTERVAL_SECS {
        return;
    }
    // 在庫の鍵は **`settings.ghost_id` ではなく、いま読み込まれている bundle の id**。
    // ゴースト切替はホットリロードされず再起動が要る (spec §4.5.2) ため、切替直後〜再起動前は
    // `settings.ghost_id` が新ゴースト・実際に喋っている人格が旧ゴースト、という食い違いが起きる。
    // `settings` 側を鍵にすると「旧ゴーストの人格で生成した文を新ゴーストの行として積む」ことになり、
    // ghost_id を持たせた意味 (§3.2) が消える。
    let ghost_id = {
        let guard = state.ghost.lock().expect("ghost poisoned");
        match guard.as_ref() {
            Ok(b) => b.ghost.id.clone(),
            Err(_) => return,
        }
    };
    let stock = match state.db.count_monologue_cache(&ghost_id, now) {
        Ok(n) => n,
        Err(err) => {
            eprintln!("[monologue] ストック件数の取得に失敗、補充を見送り: {err:#}");
            return;
        }
    };
    if stock >= REFILL_THRESHOLD {
        return;
    }

    // **月額上限を補充の前に必ず見る** (foundation-design §3.5)。
    // これが無いと、チャットを使わず常駐しているだけのユーザーで上限が素通りする
    // (補充は無人で 30 分ごとに走り、降格は 5 分で自動復帰するので歯止めにならない)。
    match cost::check_status(&state.db, settings.monthly_limit_usd) {
        Ok(status) if status.exceeded => {
            // 超過はチャット経路と同じ扱い (降格・告知) に合流させる。
            // 背景処理だけが上限を素通りする穴を作らない。
            crate::dialogue::evaluate_cost_status(app, state, &settings).await;
            return;
        }
        Ok(_) => {}
        Err(err) => {
            eprintln!("[monologue] コスト確認に失敗、補充を見送り: {err:#}");
            return;
        }
    }

    // ここから先は LLM を叩く。成否によらず最短間隔を消費させる (失敗の連打を防ぐ)。
    state
        .dialogue
        .monologue_refill_ts
        .store(now, Ordering::SeqCst);

    if refill_once(state, &settings, stock, now).await {
        // 呼び出し後も advanced 会話と同じ上限評価を通す (§3.5)。
        crate::dialogue::evaluate_cost_status(app, state, &settings).await;
    }
}

/// 実際に 1 回 LLM を呼んでストックを積む。戻り値は「API を実際に叩いたか」
/// (呼んだときだけ呼び出し側が上限評価を回す)。
async fn refill_once(
    state: &Arc<AppState>,
    settings: &Settings,
    stock: u64,
    now: i64,
) -> bool {
    // API キーは None も許容する: base_url を差し替えたローカル LLM (LMStudio/Ollama) は
    // キー無しで動き、対話チャット (`try_advanced`) も同じく None を通している。
    // ここだけ None を拒むと、advanced チャットが動く環境で独り言だけ補充されない。
    let api_key = match secrets::get_api_key_async(&settings.llm_provider).await {
        Ok(k) => k,
        Err(err) => {
            eprintln!("[monologue] API キー取得に失敗、補充を見送り: {err:#}");
            return false;
        }
    };
    // std::sync::MutexGuard を await を跨いで保持できないので、ブロックで握り→外す。
    let bundle = {
        let guard = state.ghost.lock().expect("ghost poisoned");
        match guard.as_ref() {
            Ok(b) => b.clone(),
            Err(err) => {
                eprintln!("[monologue] ゴースト未読込のため補充を見送り: {err}");
                return false;
            }
        }
    };

    let materials = topic_materials(state, settings, now);
    // 複数の見出しを織り込んだ文は**最も古いネタに引きずられる**ので、
    // 記録するのは渡した見出しのうち最古の取得時刻 (安全側、foundation-design §3.4)。
    let topic_fetched_ts = materials.iter().map(|m| m.fetched_ts).min();

    let client = LlmClient::new(settings.llm_base_url.clone(), api_key);
    let messages = vec![
        ChatMessage::system(build_system_prompt(&bundle, &materials, REFILL_BATCH)),
        ChatMessage::user(build_user_prompt(REFILL_BATCH)),
    ];
    let result = tokio::time::timeout(
        Duration::from_secs(REFILL_LLM_TIMEOUT_SECS),
        client.chat(&settings.llm_model, messages),
    )
    .await;
    let response = match result {
        Ok(Ok(resp)) => resp,
        Ok(Err(err)) => {
            eprintln!("[monologue] 補充の API エラー、次の周期で再試行: {err:#}");
            // 呼びには行ったので上限評価は回す (課金が発生している可能性がある)。
            return true;
        }
        Err(_) => {
            eprintln!("[monologue] 補充がタイムアウト、次の周期で再試行");
            return true;
        }
    };

    // 会計記録は advanced 会話と同じテーブル・同じ関数
    // (= spec §4.4.4「コスト管理は advanced 会話と同じ扱い」)。
    let prompt_tokens = response.usage.map(|u| u.prompt_tokens).unwrap_or(0);
    let completion_tokens = response.usage.map(|u| u.completion_tokens).unwrap_or(0);
    let cost_usd = estimate_cost_usd(&settings.llm_model, prompt_tokens, completion_tokens);
    let _ = state.db.append_api_usage(&ApiUsageRow {
        provider: settings.llm_provider.clone(),
        model: settings.llm_model.clone(),
        prompt_tokens: prompt_tokens as i64,
        completion_tokens: completion_tokens as i64,
        cost_usd,
        ts: now,
    });

    let raw = response
        .choices
        .first()
        .map(|c| c.message.content.as_str())
        .unwrap_or_default();
    let generated = match parse_monologue_batch(raw) {
        Ok(lines) => lines,
        Err(err) => {
            eprintln!("[monologue] 応答が壊れているため 1 件も積まない: {err:#}");
            return true;
        }
    };

    let room = STOCK_MAX.saturating_sub(stock) as usize;
    let mut pushed = 0usize;
    for line in generated.into_iter().take(room) {
        // 鍵は**プロンプトに使った bundle 自身の id**。人格と鍵が構造的に一致する
        // (`settings.ghost_id` は切替直後に先行して変わるので使わない)。
        if let Err(err) = state.db.push_monologue_cache(
            &bundle.ghost.id,
            &line.text,
            line.pose.as_deref(),
            topic_fetched_ts,
            now,
        ) {
            eprintln!("[monologue] ストックの保存に失敗: {err:#}");
            break;
        }
        pushed += 1;
    }
    eprintln!(
        "[monologue] 補充: {pushed} 件 (在庫 {stock} → {}、時事ネタ材料 {} 件)",
        stock + pushed as u64,
        materials.len()
    );
    true
}

/// 時事ネタの材料を集める (**織り込み時の失効**、foundation-design §3.4)。
/// `topics_enabled` が false なら空 (低モードと同じく時事ネタ無し、spec §4.4.6)。
fn topic_materials(state: &Arc<AppState>, settings: &Settings, now: i64) -> Vec<TopicMaterial> {
    if !settings.topics_enabled {
        return Vec::new();
    }
    // `list_recent_topics` は fetched_ts 降順なので、鮮度フィルタは呼び出し側で掛ければ足りる
    // (除外される行は残る行より必ず古い。foundation-design §7-2 の決着)。
    // 既存の 7 日 prune は topics_enabled が ON の周期でしか走らないので、それには依存しない。
    let rows = match state.db.list_recent_topics(TOPIC_SCAN_LIMIT) {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("[monologue] 時事ネタの読み出しに失敗、独り言のみで生成: {err:#}");
            return Vec::new();
        }
    };
    // 興味分野から外したキーワードの見出しは `topics_cache` に最大 7 日残るので、
    // **いま有効なトピックの見出しだけ**に絞る (外した興味の話題を喋り続けない)。
    let enabled = state.db.list_enabled_topics().unwrap_or_else(|err| {
        eprintln!("[monologue] 有効トピックの読み出しに失敗、時事ネタ無しで生成: {err:#}");
        Vec::new()
    });
    select_topic_materials(settings.topics_enabled, &enabled, rows, now)
}

/// 材料の選別 (純関数)。除外条件は 3 つ:
///
/// 1. **同意していない** (`topics_enabled == false`) → 1 件も渡さない (spec §4.4.6 既定オフ)
/// 2. **興味分野から外された** トピックの見出し
/// 3. **鮮度が足りない** — 取得から 7 日超はもちろん、残り寿命が
///    `TOPIC_MIN_REMAINING_SECS` を切るものも使わない (積んだ直後に失効する有料ループ回避)
fn select_topic_materials(
    topics_enabled: bool,
    enabled_topics: &[String],
    rows: Vec<TopicCacheRow>,
    now: i64,
) -> Vec<TopicMaterial> {
    if !topics_enabled {
        return Vec::new();
    }
    let usable_age = TOPIC_MAX_AGE_SECS - TOPIC_MIN_REMAINING_SECS;
    rows.into_iter()
        .filter(|r| enabled_topics.iter().any(|t| t == &r.topic))
        .filter(|r| now - r.fetched_ts <= usable_age)
        .take(TOPIC_MATERIAL_MAX)
        .map(|r| TopicMaterial {
            headline: r.headline,
            fetched_ts: r.fetched_ts,
        })
        .collect()
}

// ===== プロンプト (foundation-design §7-4 の決着) =====

/// 独り言生成のシステムプロンプト。
///
/// 会話履歴・長期記憶 (`user_profile`) は**渡さない** — 独り言は「ひとりごと」であって
/// 応答ではないため (foundation-design §1.1)。材料はゴースト設定と時事ネタだけ。
fn build_system_prompt(bundle: &GhostBundle, materials: &[TopicMaterial], count: usize) -> String {
    let main_name = bundle.ghost.characters.main.name.as_str();
    let poses = available_pose_names(bundle);
    // ghost.json の persona を載せる (spec §4.2)。独り言も同じ人格で書かせる。
    let persona_line = match bundle
        .ghost
        .characters
        .main
        .persona
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(t) => format!("
{main_name} の人物像: {t}
上の設定に従って書き、設定文そのものを独り言で説明しない。
"),
        None => String::new(),
    };
    let topics_block = if materials.is_empty() {
        String::new()
    } else {
        let mut block = String::from(
            "\n最近の話題 (雑談の種。全部に触れる必要はなく、使わない件があってよい):\n",
        );
        for m in materials {
            block.push_str(&format!("- {}\n", m.headline));
        }
        block.push_str(
            "話題に触れるときは、ニュースの要約や事実の断定はせず、\
             見聞きしたことへの感想や独り言としてひとこと漏らす程度にする。\n",
        );
        block
    };
    format!(
        r#"あなたはデスクトップマスコットアプリ「{ghost}」のメインキャラ「{main}」です。
ユーザーの画面の片隅にいて、誰に向けるでもなく**ふと漏らす独り言**を書いてください。
{persona_line}{topics_block}
独り言のルール:
- 1 件につき 1〜2 行の短い独り言。キャラクターの口調を保つ。
- 相手に質問しない・返事を促さない・呼びかけない (独り言なので会話にしない)。
- 説教くさい長文、箇条書き、記号や絵文字の羅列は禁止。
- {count} 件はそれぞれ別の話題にし、同じ言い回しを繰り返さない。

出力形式: 必ず次の JSON 配列のみを返す。前置き / 後置き / マークダウン禁止。
[
  {{ "text": "...", "pose": "<pose>" }}
]
- pose に使えるのは次のいずれか: {poses}
"#,
        ghost = bundle.ghost.name,
        persona_line = persona_line,
        main = main_name,
        topics_block = topics_block,
        count = count,
        poses = poses,
    )
}

fn build_user_prompt(count: usize) -> String {
    format!("独り言を {count} 件、JSON 配列で返してください。")
}

// ===== 応答パース =====

/// LLM が返した独り言 1 件。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct GeneratedMonologue {
    text: String,
    #[serde(default)]
    pose: Option<String>,
}

/// 応答 (JSON 配列) を独り言の候補列にする。
///
/// **壊れていたら 1 件も積まない** (foundation-design §3.5) — 半端に積むと、
/// 崩れた文がストックに残って何度も喋られる。
/// `pose` の妥当性はここでは見ない: シェルは push と pop の間に変わりうるので、
/// 検証は消費時 (`deliver::resolve_line`) の 1 箇所に寄せる (§3.3)。
fn parse_monologue_batch(raw: &str) -> Result<Vec<GeneratedMonologue>> {
    let json = extract_json_blob(raw);
    let parsed: Vec<GeneratedMonologue> = serde_json::from_str(json)
        .with_context(|| format!("JSON 配列として読めません: {json}"))?;
    let cleaned: Vec<GeneratedMonologue> = parsed
        .into_iter()
        .filter_map(|m| {
            let text = m.text.trim().to_string();
            if text.is_empty() {
                return None;
            }
            let pose = m
                .pose
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty());
            Some(GeneratedMonologue { text, pose })
        })
        .collect();
    if cleaned.is_empty() {
        bail!("有効な独り言が 1 件も含まれていません");
    }
    Ok(cleaned)
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

    const DAY: i64 = 24 * 60 * 60;

    fn topic_row(headline: &str, fetched_ts: i64) -> TopicCacheRow {
        TopicCacheRow {
            id: 0,
            topic: "テスト".into(),
            headline: headline.into(),
            link: "https://example.com/".into(),
            fetched_ts,
        }
    }

    fn make_bundle() -> GhostBundle {
        let mut poses = BTreeMap::new();
        poses.insert("normal".to_string(), "main/normal.png".to_string());
        poses.insert("happy".to_string(), "main/happy.png".to_string());
        let main_def = ShellCharacterDef {
            base_size: BaseSize {
                width: 280,
                height: 420,
            },
            default_pose: "normal".into(),
            poses,
            poke_regions: Default::default(),
        };
        GhostBundle {
            ghost: GhostManifest {
                schema_version: 1,
                id: "default".into(),
                name: "ミミとクロ".into(),
                characters: GhostCharacters {
                    main: GhostCharacter {
                        name: "ミミ".into(),
                        persona: Some("元気で好奇心旺盛な女の子。一人称は「あたし」。".into()),
                    },
                    sub: None,
                },
                dictionaries: vec!["dic/main.yaml".into()],
                prompt: None,
            },
            shell: ShellManifest {
                schema_version: 1,
                id: "default".into(),
                name: "デフォルト".into(),
                characters: ShellCharacters {
                    main: main_def,
                    sub: None,
                },
            },
            shell_dir: PathBuf::from(""),
            dictionary: crate::ghost::dict::Dictionary {
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
            },
        }
    }

    // ===== 材料の選別 (同意ゲート + 興味 + 織り込み時の失効) =====

    /// 既定の有効トピック集合（`topic_row` が付ける "テスト" を許可する）。
    fn enabled() -> Vec<String> {
        vec!["テスト".to_string()]
    }

    fn select(rows: Vec<TopicCacheRow>, now: i64) -> Vec<TopicMaterial> {
        select_topic_materials(true, &enabled(), rows, now)
    }

    #[test]
    fn no_materials_without_consent() {
        // spec §4.4.6「既定オフ・オンボーディングで明示同意必須」。同意が無ければ 1 件も渡さない。
        let now = 100 * DAY;
        let rows = vec![topic_row("今日のニュース", now)];
        assert!(
            select_topic_materials(false, &enabled(), rows, now).is_empty(),
            "topics_enabled=false では時事ネタを一切渡さない"
        );
    }

    #[test]
    fn materials_exclude_disabled_interest_topics() {
        // 興味分野から外したキーワードの見出しは topics_cache に最大 7 日残るので、
        // 「いま有効なトピック」で絞らないと外した話題を喋り続ける。
        let now = 100 * DAY;
        let mut foreign = topic_row("外した興味の見出し", now);
        foreign.topic = "やめた分野".into();
        let rows = vec![foreign, topic_row("有効な見出し", now)];
        let out = select(rows, now);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].headline, "有効な見出し");
    }

    #[test]
    fn materials_drop_headlines_older_than_a_week() {
        let now = 100 * DAY;
        let rows = vec![
            topic_row("今日のニュース", now),
            topic_row("5 日前のニュース", now - 5 * DAY),
            topic_row("8 日前のニュース", now - 8 * DAY),
        ];
        let out = select(rows, now);
        assert_eq!(out.len(), 2, "7 日を超えた見出しは材料に使わない");
        assert!(out.iter().all(|m| !m.headline.contains("8 日前")));
    }

    #[test]
    fn materials_require_a_minimum_remaining_life() {
        // 賞味期限ぎりぎりの見出しを織り込むと、積んだ文が発話時失効で即捨てられ、
        // 在庫 0 のまま 30 分ごとに補充を繰り返す有料ループになる。
        let now = 100 * DAY;
        let almost_stale = now - (TOPIC_MAX_AGE_SECS - 60); // 残り 60 秒
        assert!(
            select(vec![topic_row("ほぼ期限切れ", almost_stale)], now).is_empty(),
            "残り寿命が足りない見出しは材料にしない"
        );
        // 余裕がちょうど閾値ぶんあれば使う（境界）。
        let boundary = now - (TOPIC_MAX_AGE_SECS - TOPIC_MIN_REMAINING_SECS);
        assert_eq!(select(vec![topic_row("境界", boundary)], now).len(), 1);
    }

    #[test]
    fn materials_are_capped() {
        let now = 100 * DAY;
        let rows: Vec<_> = (0..TOPIC_MATERIAL_MAX + 5)
            .map(|i| topic_row(&format!("見出し{i}"), now))
            .collect();
        assert_eq!(select(rows, now).len(), TOPIC_MATERIAL_MAX);
    }

    #[test]
    fn oldest_material_ts_is_recorded_for_the_batch() {
        // 複数の見出しを渡した文は最も古いネタに引きずられるので、記録も最古に合わせる。
        let now = 100 * DAY;
        let materials = select(
            vec![
                topic_row("新しい", now),
                topic_row("古い", now - 5 * DAY),
                topic_row("中くらい", now - 2 * DAY),
            ],
            now,
        );
        assert_eq!(
            materials.iter().map(|m| m.fetched_ts).min(),
            Some(now - 5 * DAY)
        );
    }

    // ===== プロンプト =====

    /// 名前と pose 語彙しか検査しておらず、**persona が丸ごと欠けていても通っていた**
    /// テストの是正 (2026-09-02)。テスト名が主張する内容を実際に検証する。
    #[test]
    fn system_prompt_includes_persona_and_poses() {
        let p = build_system_prompt(&make_bundle(), &[], REFILL_BATCH);
        assert!(p.contains("ミミとクロ"), "{p}");
        assert!(p.contains("ミミ"), "{p}");
        assert!(p.contains("happy") && p.contains("normal"), "pose 語彙: {p}");
        // 本題: ghost.json の persona がプロンプトに載ること。
        assert!(
            p.contains("元気で好奇心旺盛な女の子"),
            "persona 本文がプロンプトに無い: {p}"
        );
        // persona 文を台詞で復唱させない歯止めも載せる。
        assert!(p.contains("設定文そのものを"), "自己申告の抑止が無い: {p}");
    }

    /// persona 未指定のゴースト (第三者製で書いていない場合) でも壊れないこと。
    #[test]
    fn system_prompt_omits_persona_block_when_absent() {
        let mut bundle = make_bundle();
        bundle.ghost.characters.main.persona = None;
        let p = build_system_prompt(&bundle, &[], REFILL_BATCH);
        assert!(!p.contains("人物像"), "persona 不在なのにブロックが出た: {p}");
        assert!(p.contains("ミミ"), "名前は残る: {p}");
    }

    #[test]
    fn system_prompt_omits_topics_block_when_no_materials() {
        let p = build_system_prompt(&make_bundle(), &[], REFILL_BATCH);
        assert!(!p.contains("最近の話題"), "材料が無ければ話題ブロックを出さない: {p}");
    }

    #[test]
    fn system_prompt_lists_topic_headlines_when_present() {
        let materials = vec![
            TopicMaterial {
                headline: "新作アニメ公開".into(),
                fetched_ts: 1,
            },
            TopicMaterial {
                headline: "Rust 1.85 がリリース".into(),
                fetched_ts: 2,
            },
        ];
        let p = build_system_prompt(&make_bundle(), &materials, REFILL_BATCH);
        assert!(p.contains("最近の話題"), "{p}");
        assert!(p.contains("新作アニメ公開"), "{p}");
        assert!(p.contains("Rust 1.85 がリリース"), "{p}");
    }

    #[test]
    fn system_prompt_forbids_addressing_the_user() {
        // 「独り言であって応答ではない」は本機能の肝なので、指示の欠落を検知する。
        let p = build_system_prompt(&make_bundle(), &[], REFILL_BATCH);
        assert!(p.contains("独り言"), "{p}");
        assert!(p.contains("質問しない"), "{p}");
    }

    // ===== 応答パース =====

    #[test]
    fn parse_batch_reads_json_array() {
        let raw = r#"[{"text":"のどかだなあ","pose":"happy"},{"text":"お茶でも飲もうかな"}]"#;
        let out = parse_monologue_batch(raw).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].text, "のどかだなあ");
        assert_eq!(out[0].pose.as_deref(), Some("happy"));
        assert!(out[1].pose.is_none());
    }

    #[test]
    fn parse_batch_handles_fenced_block() {
        let raw = "```json\n[{\"text\":\"ふう\"}]\n```";
        let out = parse_monologue_batch(raw).unwrap();
        assert_eq!(out[0].text, "ふう");
    }

    #[test]
    fn parse_batch_drops_blank_entries() {
        let raw = r#"[{"text":"  "},{"text":"生きてる"},{"text":""}]"#;
        let out = parse_monologue_batch(raw).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "生きてる");
    }

    #[test]
    fn parse_batch_blank_pose_becomes_none() {
        let raw = r#"[{"text":"ねむい","pose":"   "}]"#;
        let out = parse_monologue_batch(raw).unwrap();
        assert!(out[0].pose.is_none());
    }

    #[test]
    fn parse_batch_rejects_broken_json() {
        // 壊れた応答からは 1 件も積まない (半端な文がストックに残らないこと)。
        assert!(parse_monologue_batch("これはただの文章です").is_err());
        assert!(parse_monologue_batch(r#"{"text":"配列ではない"}"#).is_err());
        assert!(parse_monologue_batch("[{\"text\":\"閉じてない\"}").is_err());
    }

    #[test]
    fn parse_batch_rejects_all_blank_array() {
        assert!(parse_monologue_batch(r#"[{"text":""},{"text":"  "}]"#).is_err());
        assert!(parse_monologue_batch("[]").is_err());
    }

    // maybe_refill / refill_once は AppState (DB・keyring・network) を要求するため
    // 単体テストの対象にしない (このコードベースに AppState を組む前例が無い。
    // regular_talk::polish_script と同じ扱い)。ゲートと二段失効のうち DB で表現される
    // 側は db.rs の monologue_cache テストが、純粋な判定は上記が受け持つ。
}

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::{Datelike, NaiveDateTime, Timelike};
use rand::Rng;
use serde::{Deserialize, Serialize};

// ===== 公開型: 辞書 v3 =====
//
// schema_version は 3 固定。
// architecture.md §6 と完全対応する最小実装。
// "将来のために" のフィールドは入れない。M1 のスコープは:
//   - input_match による低負荷モード会話
//   - events.first_boot / events.boot (時間帯別 when)
// それ以外のセクション (recall / monologue / events の他キー / system_messages)
// は構文的に受理して保持するだけ。実発火は M2+ で行う。

#[derive(Debug, Clone)]
pub struct Dictionary {
    pub schema_version: u32,
    pub input_match: Vec<InputMatchRule>,
    pub fallback: Vec<Line>,
    pub recall: Vec<Line>,
    pub monologue: Vec<Line>,
    pub events: HashMap<String, Vec<EventLine>>,
    pub system_messages: HashMap<String, Vec<EventLine>>,
}

#[derive(Debug, Clone)]
pub struct InputMatchRule {
    pub id: String,
    pub keywords: Vec<String>,
    pub priority: i32,
    pub responses: Vec<Line>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Line {
    pub main: SpeechTurn,
    pub sub: Option<SpeechTurn>,
}

#[derive(Debug, Clone)]
pub struct EventLine {
    pub when: Option<WhenExpr>,
    pub line: Line,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeechTurn {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pose: Option<String>,
}

// ===== 公開型: 評価結果 =====

/// 1ターンの発話結果（フロントへの emit/log 用）。
/// banter::apply_pattern が Line + Pattern1 を組み立てる。
#[derive(Debug, Clone, Serialize)]
pub struct DialogueLine {
    pub main: SpeechTurn,
    /// shell に sub があり、かつ辞書 sub も指定されたときだけ Some。
    pub sub: Option<SpeechTurn>,
}

impl From<Line> for DialogueLine {
    fn from(value: Line) -> Self {
        DialogueLine {
            main: value.main,
            sub: value.sub,
        }
    }
}

// ===== 内部 YAML 型 =====
//
// YAML をいったん「素朴な構造」に落としてから、上記の公開型に詰め替える。
// 公開型を直接 deserialize にすると when の代数的判定がしにくくなるため
// 二段階にしている。

#[derive(Debug, Deserialize)]
struct DictionaryYaml {
    schema_version: u32,
    #[serde(default)]
    input_match: Vec<InputMatchYaml>,
    #[serde(default)]
    fallback: Vec<LineYaml>,
    #[serde(default)]
    recall: Vec<LineYaml>,
    #[serde(default)]
    monologue: Vec<LineYaml>,
    #[serde(default)]
    events: HashMap<String, Vec<EventLineYaml>>,
    #[serde(default)]
    system_messages: HashMap<String, Vec<EventLineYaml>>,
}

#[derive(Debug, Deserialize)]
struct InputMatchYaml {
    id: String,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default = "default_priority")]
    priority: i32,
    #[serde(default)]
    responses: Vec<LineYaml>,
}

fn default_priority() -> i32 {
    1
}

#[derive(Debug, Deserialize)]
struct LineYaml {
    main: SpeechTurnYaml,
    #[serde(default)]
    sub: Option<SpeechTurnYaml>,
}

#[derive(Debug, Deserialize)]
struct EventLineYaml {
    #[serde(default)]
    when: Option<WhenYaml>,
    main: SpeechTurnYaml,
    #[serde(default)]
    sub: Option<SpeechTurnYaml>,
}

#[derive(Debug, Deserialize)]
struct SpeechTurnYaml {
    text: String,
    #[serde(default)]
    pose: Option<String>,
}

// ===== when 条件 =====

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct WhenYaml {
    #[serde(default)]
    hour_from: Option<u32>,
    #[serde(default)]
    hour_to: Option<u32>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    all_of: Option<Vec<WhenYaml>>,
    #[serde(default)]
    any_of: Option<Vec<WhenYaml>>,
    #[serde(default)]
    not: Option<Box<WhenYaml>>,
    #[serde(default)]
    not_in_recent: Option<NotInRecentYaml>,
    #[serde(default)]
    probability: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct NotInRecentYaml {
    key: String,
    count: u32,
}

#[derive(Debug, Clone)]
pub enum WhenExpr {
    Hour { from: u32, to: u32 },
    Date { month: u32, day: u32 },
    AllOf(Vec<WhenExpr>),
    AnyOf(Vec<WhenExpr>),
    Not(Box<WhenExpr>),
    NotInRecent { key: String, count: u32 },
    Probability(f64),
}

impl WhenExpr {
    fn from_yaml(yaml: WhenYaml) -> Result<Self> {
        // 同一ノードで複数フィールドが指定されたら all_of で結合する。
        // architecture.md §6.3 の ① 単純条件は (hour_from + hour_to) が共起するので
        // それを一つの Hour に束ねたうえで、他のキーが付いていれば AllOf にまとめる。
        let mut leaves: Vec<WhenExpr> = Vec::new();

        if yaml.hour_from.is_some() || yaml.hour_to.is_some() {
            let from = yaml
                .hour_from
                .ok_or_else(|| anyhow!("when.hour_to を指定する場合は hour_from も必須です"))?;
            let to = yaml
                .hour_to
                .ok_or_else(|| anyhow!("when.hour_from を指定する場合は hour_to も必須です"))?;
            // hour_to は 24 (=翌日 0 時を表すエンドポイント) まで許す。
            // hour_from は 0..=23、hour_to は 0..=24。
            if from > 23 || to > 24 {
                return Err(anyhow!(
                    "when.hour_from は 0〜23、hour_to は 0〜24 の範囲で指定してください (from={from}, to={to})"
                ));
            }
            leaves.push(WhenExpr::Hour { from, to });
        }

        if let Some(date) = yaml.date {
            let (m, d) = parse_md(&date)
                .with_context(|| format!("when.date の形式が不正です: '{date}'"))?;
            leaves.push(WhenExpr::Date { month: m, day: d });
        }

        if let Some(all_of) = yaml.all_of {
            let mut children = Vec::new();
            for child in all_of {
                children.push(WhenExpr::from_yaml(child)?);
            }
            leaves.push(WhenExpr::AllOf(children));
        }

        if let Some(any_of) = yaml.any_of {
            let mut children = Vec::new();
            for child in any_of {
                children.push(WhenExpr::from_yaml(child)?);
            }
            leaves.push(WhenExpr::AnyOf(children));
        }

        if let Some(not) = yaml.not {
            leaves.push(WhenExpr::Not(Box::new(WhenExpr::from_yaml(*not)?)));
        }

        if let Some(nir) = yaml.not_in_recent {
            if nir.count == 0 {
                return Err(anyhow!("when.not_in_recent.count は 1 以上にしてください"));
            }
            leaves.push(WhenExpr::NotInRecent {
                key: nir.key,
                count: nir.count,
            });
        }

        if let Some(p) = yaml.probability {
            if !(0.0..=1.0).contains(&p) {
                return Err(anyhow!(
                    "when.probability は 0.0〜1.0 の範囲で指定してください (p={p})"
                ));
            }
            leaves.push(WhenExpr::Probability(p));
        }

        match leaves.len() {
            0 => Err(anyhow!("when に評価可能な条件が一つも含まれていません")),
            1 => Ok(leaves.into_iter().next().unwrap()),
            _ => Ok(WhenExpr::AllOf(leaves)),
        }
    }

    /// 真偽 + 「特異度」を返す。
    /// architecture.md §6.3 末尾: 特異度の高い候補から抽選するため、
    /// 単純条件は 1、AllOf は子の和、AnyOf はマッチした子の最大値、Not は内側の特異度、
    /// NotInRecent / Probability は条件成立時 1。
    pub fn evaluate(&self, ctx: &WhenContext) -> WhenResult {
        match self {
            WhenExpr::Hour { from, to } => {
                let h = ctx.hour;
                let ok = if from <= to {
                    *from <= h && h < *to
                } else {
                    // 跨ぎ (例: 22-5 → 22,23,0,1,2,3,4)
                    h >= *from || h < *to
                };
                WhenResult::leaf(ok)
            }
            WhenExpr::Date { month, day } => {
                let ok = *month == ctx.month && *day == ctx.day;
                WhenResult::leaf(ok)
            }
            WhenExpr::AllOf(children) => {
                let mut total = 0u32;
                for c in children {
                    let r = c.evaluate(ctx);
                    if !r.matched {
                        return WhenResult {
                            matched: false,
                            specificity: 0,
                        };
                    }
                    total = total.saturating_add(r.specificity);
                }
                WhenResult {
                    matched: true,
                    specificity: total.max(1),
                }
            }
            WhenExpr::AnyOf(children) => {
                let mut best = 0u32;
                let mut matched = false;
                for c in children {
                    let r = c.evaluate(ctx);
                    if r.matched {
                        matched = true;
                        best = best.max(r.specificity);
                    }
                }
                WhenResult {
                    matched,
                    specificity: best,
                }
            }
            WhenExpr::Not(inner) => {
                let r = inner.evaluate(ctx);
                WhenResult {
                    matched: !r.matched,
                    specificity: r.specificity.max(1),
                }
            }
            WhenExpr::NotInRecent { key, count } => {
                let recent = ctx.recent_keys.iter().rev().take(*count as usize);
                let ok = recent.into_iter().all(|k| k != key);
                WhenResult::leaf(ok)
            }
            WhenExpr::Probability(p) => {
                let ok = ctx.rng_sample < *p;
                WhenResult::leaf(ok)
            }
        }
    }
}

fn parse_md(text: &str) -> Result<(u32, u32)> {
    let parts: Vec<&str> = text.split('-').collect();
    if parts.len() != 2 {
        return Err(anyhow!("'MM-DD' 形式で指定してください"));
    }
    let month: u32 = parts[0]
        .parse()
        .map_err(|_| anyhow!("月が数値ではありません"))?;
    let day: u32 = parts[1]
        .parse()
        .map_err(|_| anyhow!("日が数値ではありません"))?;
    if !(1..=12).contains(&month) {
        return Err(anyhow!("月は 1〜12 の範囲で指定してください"));
    }
    if !(1..=31).contains(&day) {
        return Err(anyhow!("日は 1〜31 の範囲で指定してください"));
    }
    Ok((month, day))
}

#[derive(Debug, Clone)]
pub struct WhenContext {
    pub hour: u32,
    pub month: u32,
    pub day: u32,
    /// 直近に発火したイベントキー (新しい順とは限らない: 評価側は末尾を最新として扱う)。
    pub recent_keys: Vec<String>,
    /// probability 用にあらかじめサンプルした乱数 [0.0, 1.0)。
    /// 候補ごとに 1 サンプルするのが正解だが、boot 時のような少数候補ならまとめて 1 つで実用上問題ない。
    /// 必要になったら候補ごとに作り直す。
    pub rng_sample: f64,
}

impl WhenContext {
    pub fn now() -> Self {
        let now: NaiveDateTime = chrono::Local::now().naive_local();
        let rng_sample = rand::thread_rng().gen_range(0.0..1.0_f64);
        Self {
            hour: now.hour(),
            month: now.month(),
            day: now.day(),
            recent_keys: Vec::new(),
            rng_sample,
        }
    }

    #[cfg(test)]
    fn fixed(hour: u32, month: u32, day: u32) -> Self {
        Self {
            hour,
            month,
            day,
            recent_keys: Vec::new(),
            rng_sample: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WhenResult {
    pub matched: bool,
    pub specificity: u32,
}

impl WhenResult {
    fn leaf(ok: bool) -> Self {
        Self {
            matched: ok,
            specificity: if ok { 1 } else { 0 },
        }
    }
}

// ===== 候補抽選 =====

impl Dictionary {
    /// 入力テキストにマッチする input_match を 1 行抽選。
    /// マッチ無しなら fallback から 1 行抽選。
    /// fallback も空ならパニックではなく `None`。
    pub fn pick_reply(&self, user_text: &str, sub_available: bool) -> Option<DialogueLine> {
        if let Some(line) = self.pick_input_match(user_text, sub_available) {
            return Some(line);
        }
        self.pick_fallback(sub_available)
    }

    pub fn pick_input_match(
        &self,
        user_text: &str,
        sub_available: bool,
    ) -> Option<DialogueLine> {
        let mut best_priority: Option<i32> = None;
        let mut hits: Vec<&InputMatchRule> = Vec::new();
        for rule in &self.input_match {
            if rule.responses.is_empty() {
                continue;
            }
            let matched = rule
                .keywords
                .iter()
                .any(|kw| !kw.is_empty() && user_text.contains(kw));
            if !matched {
                continue;
            }
            match best_priority {
                Some(p) if rule.priority < p => continue,
                Some(p) if rule.priority > p => {
                    hits.clear();
                    best_priority = Some(rule.priority);
                }
                None => best_priority = Some(rule.priority),
                _ => {}
            }
            hits.push(rule);
        }

        let rule = pick_random(&hits)?;
        pick_random(&rule.responses).map(|line| project_line(line, sub_available))
    }

    pub fn pick_fallback(&self, sub_available: bool) -> Option<DialogueLine> {
        pick_random(&self.fallback).map(|line| project_line(line, sub_available))
    }

    /// ランダムトーク (monologue) から 1 件抽選。
    pub fn pick_monologue(&self, sub_available: bool) -> Option<DialogueLine> {
        pick_random(&self.monologue).map(|line| project_line(line, sub_available))
    }

    /// 指定イベントキーの候補から when 条件で抽選。
    /// 候補は: when 付き && matched で specificity 最大の集合 → 抽選 (1)。
    ///        無ければ when 無しの候補から抽選 (2)。
    pub fn pick_event(
        &self,
        event_key: &str,
        ctx: &WhenContext,
        sub_available: bool,
    ) -> Option<DialogueLine> {
        let candidates = self.events.get(event_key)?;
        pick_event_candidate(candidates, ctx, sub_available)
    }

    pub fn pick_system_message(
        &self,
        key: &str,
        ctx: &WhenContext,
        sub_available: bool,
    ) -> Option<DialogueLine> {
        let candidates = self.system_messages.get(key)?;
        pick_event_candidate(candidates, ctx, sub_available)
    }
}

fn pick_event_candidate(
    candidates: &[EventLine],
    ctx: &WhenContext,
    sub_available: bool,
) -> Option<DialogueLine> {
    let mut best_specificity: u32 = 0;
    let mut conditional: Vec<&EventLine> = Vec::new();
    let mut unconditional: Vec<&EventLine> = Vec::new();

    for cand in candidates {
        match &cand.when {
            None => unconditional.push(cand),
            Some(expr) => {
                let r = expr.evaluate(ctx);
                if !r.matched {
                    continue;
                }
                if r.specificity > best_specificity {
                    best_specificity = r.specificity;
                    conditional.clear();
                }
                if r.specificity == best_specificity {
                    conditional.push(cand);
                }
            }
        }
    }

    let chosen = if !conditional.is_empty() {
        pick_random(&conditional)
    } else {
        pick_random(&unconditional)
    }?;

    Some(project_line(&chosen.line, sub_available))
}

fn project_line(line: &Line, sub_available: bool) -> DialogueLine {
    DialogueLine {
        main: line.main.clone(),
        sub: if sub_available { line.sub.clone() } else { None },
    }
}

fn pick_random<T>(items: &[T]) -> Option<&T> {
    if items.is_empty() {
        return None;
    }
    let idx = rand::thread_rng().gen_range(0..items.len());
    items.get(idx)
}

// ===== パーサ =====

pub fn load_dictionary(path: &Path) -> Result<Dictionary> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("辞書ファイルを開けませんでした: {}", path.display()))?;
    let yaml: DictionaryYaml = serde_yaml::from_str(&raw)
        .with_context(|| format!("辞書 YAML の構文エラーです: {}", path.display()))?;
    if yaml.schema_version != 3 {
        return Err(anyhow!(
            "辞書 schema_version が未対応です（期待: 3, 検出: {}）: {}",
            yaml.schema_version,
            path.display()
        ));
    }

    let dict = Dictionary {
        schema_version: yaml.schema_version,
        input_match: convert_input_match(yaml.input_match)?,
        fallback: convert_lines(yaml.fallback)?,
        recall: convert_lines(yaml.recall)?,
        monologue: convert_lines(yaml.monologue)?,
        events: convert_event_map(yaml.events)?,
        system_messages: convert_event_map(yaml.system_messages)?,
    };

    validate_dictionary(&dict, path)?;
    Ok(dict)
}

fn convert_input_match(items: Vec<InputMatchYaml>) -> Result<Vec<InputMatchRule>> {
    let mut out = Vec::with_capacity(items.len());
    for y in items {
        out.push(InputMatchRule {
            id: y.id,
            keywords: y.keywords,
            priority: y.priority,
            responses: convert_lines(y.responses)?,
        });
    }
    Ok(out)
}

fn convert_lines(items: Vec<LineYaml>) -> Result<Vec<Line>> {
    let mut out = Vec::with_capacity(items.len());
    for y in items {
        out.push(Line {
            main: SpeechTurn {
                text: y.main.text,
                pose: y.main.pose,
            },
            sub: y.sub.map(|s| SpeechTurn {
                text: s.text,
                pose: s.pose,
            }),
        });
    }
    Ok(out)
}

fn convert_event_map(
    items: HashMap<String, Vec<EventLineYaml>>,
) -> Result<HashMap<String, Vec<EventLine>>> {
    let mut out = HashMap::with_capacity(items.len());
    for (key, list) in items {
        let mut converted = Vec::with_capacity(list.len());
        for y in list {
            let when = match y.when {
                Some(w) => Some(WhenExpr::from_yaml(w).with_context(|| {
                    format!("events.{key} の when 解析に失敗しました")
                })?),
                None => None,
            };
            converted.push(EventLine {
                when,
                line: Line {
                    main: SpeechTurn {
                        text: y.main.text,
                        pose: y.main.pose,
                    },
                    sub: y.sub.map(|s| SpeechTurn {
                        text: s.text,
                        pose: s.pose,
                    }),
                },
            });
        }
        out.insert(key, converted);
    }
    Ok(out)
}

fn validate_dictionary(dict: &Dictionary, path: &Path) -> Result<()> {
    // main.text が空 → 規約として禁止 (architecture §6.4)。
    let mut checks = Vec::new();
    for rule in &dict.input_match {
        for (i, line) in rule.responses.iter().enumerate() {
            if line.main.text.trim().is_empty() {
                checks.push(format!(
                    "input_match[id={}].responses[{i}].main.text が空です",
                    rule.id
                ));
            }
        }
    }
    for (i, line) in dict.fallback.iter().enumerate() {
        if line.main.text.trim().is_empty() {
            checks.push(format!("fallback[{i}].main.text が空です"));
        }
    }
    for (key, list) in &dict.events {
        for (i, ev) in list.iter().enumerate() {
            if ev.line.main.text.trim().is_empty() {
                checks.push(format!("events.{key}[{i}].main.text が空です"));
            }
        }
    }
    if !checks.is_empty() {
        return Err(anyhow!(
            "辞書バリデーションエラー: {}\n対象: {}",
            checks.join("; "),
            path.display()
        ));
    }
    Ok(())
}

// ===== テスト =====

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(hour: u32, month: u32, day: u32) -> WhenContext {
        WhenContext::fixed(hour, month, day)
    }

    fn yaml_to_when(yaml: &str) -> WhenExpr {
        let y: WhenYaml = serde_yaml::from_str(yaml).expect("yaml parse");
        WhenExpr::from_yaml(y).expect("when from_yaml")
    }

    #[test]
    fn when_hour_simple() {
        let w = yaml_to_when("{hour_from: 5, hour_to: 11}");
        assert!(w.evaluate(&ctx(5, 6, 20)).matched);
        assert!(w.evaluate(&ctx(10, 6, 20)).matched);
        assert!(!w.evaluate(&ctx(11, 6, 20)).matched); // 半開
        assert!(!w.evaluate(&ctx(4, 6, 20)).matched);
    }

    #[test]
    fn when_hour_wrap_midnight() {
        let w = yaml_to_when("{hour_from: 22, hour_to: 5}");
        assert!(w.evaluate(&ctx(22, 6, 20)).matched);
        assert!(w.evaluate(&ctx(0, 6, 20)).matched);
        assert!(w.evaluate(&ctx(4, 6, 20)).matched);
        assert!(!w.evaluate(&ctx(5, 6, 20)).matched);
        assert!(!w.evaluate(&ctx(12, 6, 20)).matched);
    }

    #[test]
    fn when_date() {
        let w = yaml_to_when(r#"{date: "12-24"}"#);
        assert!(w.evaluate(&ctx(10, 12, 24)).matched);
        assert!(!w.evaluate(&ctx(10, 12, 25)).matched);
    }

    #[test]
    fn when_all_of() {
        let w = yaml_to_when(
            r#"
            all_of:
              - { hour_from: 22, hour_to: 24 }
              - { date: "12-24" }
            "#,
        );
        assert!(w.evaluate(&ctx(22, 12, 24)).matched);
        assert!(!w.evaluate(&ctx(22, 12, 25)).matched);
        assert!(!w.evaluate(&ctx(20, 12, 24)).matched);
        // 特異度: hour=1 + date=1 = 2
        assert_eq!(w.evaluate(&ctx(22, 12, 24)).specificity, 2);
    }

    #[test]
    fn when_any_of() {
        let w = yaml_to_when(
            r#"
            any_of:
              - { date: "12-24" }
              - { date: "12-25" }
            "#,
        );
        assert!(w.evaluate(&ctx(10, 12, 24)).matched);
        assert!(w.evaluate(&ctx(10, 12, 25)).matched);
        assert!(!w.evaluate(&ctx(10, 12, 26)).matched);
    }

    #[test]
    fn when_not() {
        let w = yaml_to_when(r#"{ not: { hour_from: 0, hour_to: 5 } }"#);
        assert!(!w.evaluate(&ctx(0, 6, 20)).matched);
        assert!(w.evaluate(&ctx(12, 6, 20)).matched);
    }

    #[test]
    fn when_not_in_recent() {
        let w = yaml_to_when(r#"{ not_in_recent: { key: "boot_evening", count: 3 } }"#);
        let mut c = ctx(10, 6, 20);
        c.recent_keys = vec!["x".into(), "y".into(), "z".into()];
        assert!(w.evaluate(&c).matched);
        c.recent_keys = vec!["x".into(), "boot_evening".into(), "z".into()];
        assert!(!w.evaluate(&c).matched);
        // count=3 の枠外なら可
        c.recent_keys = vec![
            "boot_evening".into(),
            "a".into(),
            "b".into(),
            "c".into(),
        ];
        assert!(w.evaluate(&c).matched);
    }

    #[test]
    fn when_probability_deterministic() {
        let w = yaml_to_when("{probability: 0.5}");
        let mut c = ctx(0, 1, 1);
        c.rng_sample = 0.49;
        assert!(w.evaluate(&c).matched);
        c.rng_sample = 0.51;
        assert!(!w.evaluate(&c).matched);
    }

    #[test]
    fn input_match_priority() {
        let yaml = r#"
schema_version: 3
input_match:
  - id: lower
    keywords: ["こんにちは"]
    priority: 1
    responses:
      - main: { text: "low" }
  - id: higher
    keywords: ["こんにちは"]
    priority: 9
    responses:
      - main: { text: "high" }
fallback: []
"#;
        let path = std::env::temp_dir().join("ugg_test_priority.yaml");
        std::fs::write(&path, yaml).unwrap();
        let dict = load_dictionary(&path).unwrap();
        let line = dict.pick_input_match("こんにちは、世界", false).unwrap();
        assert_eq!(line.main.text, "high");
    }

    #[test]
    fn sub_dropped_when_unavailable() {
        let yaml = r#"
schema_version: 3
input_match:
  - id: greet
    keywords: ["hi"]
    priority: 1
    responses:
      - main: { text: "main" }
        sub: { text: "sub" }
fallback: []
"#;
        let path = std::env::temp_dir().join("ugg_test_sub.yaml");
        std::fs::write(&path, yaml).unwrap();
        let dict = load_dictionary(&path).unwrap();
        let with_sub = dict.pick_reply("hi", true).unwrap();
        assert!(with_sub.sub.is_some());
        let no_sub = dict.pick_reply("hi", false).unwrap();
        assert!(no_sub.sub.is_none());
    }

    #[test]
    fn boot_event_hour_specific_wins_over_unconditional() {
        let yaml = r#"
schema_version: 3
input_match: []
fallback: []
events:
  boot:
    - main: { text: "default" }
    - when: { hour_from: 5, hour_to: 11 }
      main: { text: "morning" }
"#;
        let path = std::env::temp_dir().join("ugg_test_boot.yaml");
        std::fs::write(&path, yaml).unwrap();
        let dict = load_dictionary(&path).unwrap();
        let line = dict.pick_event("boot", &ctx(8, 6, 20), false).unwrap();
        assert_eq!(line.main.text, "morning");
        // 時間外なら無条件にフォールバック
        let line = dict.pick_event("boot", &ctx(3, 6, 20), false).unwrap();
        assert_eq!(line.main.text, "default");
    }

    #[test]
    fn unsupported_schema_version_is_rejected() {
        let yaml = "schema_version: 2\ninput_match: []\nfallback: []\n";
        let path = std::env::temp_dir().join("ugg_test_v2.yaml");
        std::fs::write(&path, yaml).unwrap();
        let err = load_dictionary(&path).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("schema_version"), "{msg}");
        assert!(msg.contains("3"), "{msg}");
    }

    #[test]
    fn empty_main_text_rejected() {
        let yaml = r#"
schema_version: 3
input_match:
  - id: bad
    keywords: ["a"]
    priority: 1
    responses:
      - main: { text: "" }
fallback: []
"#;
        let path = std::env::temp_dir().join("ugg_test_empty_main.yaml");
        std::fs::write(&path, yaml).unwrap();
        let err = load_dictionary(&path).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("main.text が空"), "{msg}");
    }

    #[test]
    fn parse_md_validates() {
        assert!(parse_md("13-01").is_err());
        assert!(parse_md("01-32").is_err());
        assert!(parse_md("12-31").is_ok());
    }
}

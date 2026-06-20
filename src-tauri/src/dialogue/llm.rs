//! OpenAI 互換 chat completions クライアント (spec §4.2.2)。
//!
//! プロバイダ抽象は持たない: 公式 OpenAI / Grok / LMStudio / Ollama 等は
//! base_url とモデル名の違いで吸収できるため。

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub message: ChatMessage,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
}

impl LlmClient {
    pub fn new(base_url: Option<String>, api_key: Option<String>) -> Self {
        let base_url = base_url
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        // ローカル LLM (LMStudio/Ollama 等) は大きめモデルだと初回ロード + 推論で
        // 60 秒を超えることがある。全体タイムアウトは余裕をもって 180 秒。
        // 接続自体は localhost なら即時なので connect は 15 秒で十分。
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .connect_timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client build failed");
        Self {
            http,
            base_url,
            api_key,
        }
    }

    pub async fn chat(&self, model: &str, messages: Vec<ChatMessage>) -> Result<ChatResponse> {
        let url = format!(
            "{}/chat/completions",
            self.base_url.trim_end_matches('/')
        );
        let mut req = self.http.post(&url);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let body = ChatRequest {
            model: model.to_string(),
            messages,
            temperature: 0.7,
        };
        let resp = req
            .json(&body)
            .send()
            .await
            .with_context(|| format!("LLM へ接続できませんでした: {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("LLM API エラー: status={status} body={text}"));
        }
        let parsed = resp
            .json::<ChatResponse>()
            .await
            .context("LLM 応答 JSON のパースに失敗")?;
        Ok(parsed)
    }
}

// ===== コスト推定 =====
//
// シンプルなテーブル: input/output の per-1M-token 単価 (USD)。
// 未掲載モデルや local LLM は 0.0 (記録のみ)。
// プロバイダ抽象を持たないため、provider 名は無視してモデル名で判定する。

pub fn estimate_cost_usd(model: &str, prompt_tokens: u64, completion_tokens: u64) -> f64 {
    let (input_per_m, output_per_m) = pricing_for(model);
    (prompt_tokens as f64 / 1_000_000.0) * input_per_m
        + (completion_tokens as f64 / 1_000_000.0) * output_per_m
}

fn pricing_for(model: &str) -> (f64, f64) {
    // 2026 初頭の公式価格を概算で反映。誤差は ±20% を想定し UI でも実費ではなく目安と注釈する。
    let m = model.to_ascii_lowercase();
    if m.starts_with("gpt-4o-mini") {
        (0.15, 0.60)
    } else if m.starts_with("gpt-4o") {
        (2.50, 10.00)
    } else if m.starts_with("gpt-4.1-mini") {
        (0.40, 1.60)
    } else if m.starts_with("gpt-4.1") {
        (2.00, 8.00)
    } else if m.starts_with("o3-mini") || m.starts_with("o4-mini") {
        (1.10, 4.40)
    } else {
        // 未掲載 (ローカル LLM 含む) は 0
        (0.0, 0.0)
    }
}

// ===== 応答パース =====
//
// LLM には JSON でこちらの形式に従って返してもらう。
// マークダウンの ```json ... ``` で囲まれているケースに耐性を持たせる。

pub fn extract_json_blob(raw: &str) -> &str {
    let trimmed = raw.trim();
    // ```json ... ``` を剥がす
    if let Some(rest) = trimmed.strip_prefix("```") {
        let after_lang = rest
            .split_once('\n')
            .map(|(_, body)| body)
            .unwrap_or(rest);
        if let Some(end) = after_lang.rfind("```") {
            return after_lang[..end].trim();
        }
        return after_lang.trim();
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_handles_fenced_block() {
        let raw = "```json\n{\"main\":{\"text\":\"hi\"}}\n```";
        assert_eq!(extract_json_blob(raw), "{\"main\":{\"text\":\"hi\"}}");
    }

    #[test]
    fn extract_json_passthrough_for_bare_json() {
        let raw = "{\"main\":{\"text\":\"hi\"}}";
        assert_eq!(extract_json_blob(raw), raw);
    }

    #[test]
    fn pricing_known_model() {
        let cost = estimate_cost_usd("gpt-4o-mini", 1_000_000, 1_000_000);
        // 0.15 + 0.60 = 0.75
        assert!((cost - 0.75).abs() < 1e-9);
    }

    #[test]
    fn pricing_unknown_model_returns_zero() {
        let cost = estimate_cost_usd("local-llama", 9999, 9999);
        assert_eq!(cost, 0.0);
    }
}

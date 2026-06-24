//! M5-C: 時事ネタ RSS。
//!
//! - 取得源: Google News の日本語 RSS (`https://news.google.com/rss/search?q=<query>&hl=ja&gl=JP&ceid=JP:ja`)
//! - パース: `quick-xml` で `<item><title>…</title><link>…</link></item>` を抽出
//! - 暗い見出しフィルタ: spec §4.5 の方針に沿って、訃報・事件・災害・戦争系のキーワードを含む見出しは除外
//!   (静寂モード相当のフィルタ。網羅性は求めず、明らかに重い話題だけ排除する)
//! - 結果は `topics_cache(topic, headline, link, fetched_ts)` に INSERT OR IGNORE で蓄積
//!
//! advanced 独り言経路 (M3 spawn_random_talk) は `topics_enabled` が true のとき本モジュールの
//! `pick_random_recent` を見て確率的にネタを差し込む (実装は M5-C のスコープ外、Phase 統合は M5 重量グループ完了後に再検討)。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use quick_xml::events::Event;
use quick_xml::reader::Reader;

use crate::state::AppState;

/// 暗い見出しのキーワード (架空語含む、含めば除外)。
/// メンテはここを更新するだけ。多言語化は将来課題。
const DARK_KEYWORDS: &[&str] = &[
    "死亡", "殺害", "遺体", "自殺", "心中", "墜落", "炎上", "焼死", "暴行", "刺殺",
    "事故死", "射殺", "テロ", "爆発", "戦死", "戦争", "侵攻", "ミサイル", "感染拡大",
    "クラスター", "倒産", "破綻", "強姦", "性的暴行", "誘拐", "監禁", "詐欺", "横領",
    "殺人", "傷害",
];

#[derive(Debug, Clone)]
pub struct RssItem {
    pub title: String,
    pub link: String,
}

/// 任意のクエリ文字列から Google News RSS URL を組み立てる。
pub fn build_google_news_rss_url(query: &str) -> String {
    let encoded = urlencode(query);
    format!("https://news.google.com/rss/search?q={encoded}&hl=ja&gl=JP&ceid=JP:ja")
}

/// 1 トピックぶんの RSS を取得 → パース → 暗い見出しを除外 → 上位 N 件返す。
pub async fn fetch_topic(query: &str, limit: usize) -> Result<Vec<RssItem>> {
    let url = build_google_news_rss_url(query);
    let body = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .with_context(|| format!("rss get {url}"))?
        .error_for_status()
        .with_context(|| format!("rss status {url}"))?
        .text()
        .await
        .with_context(|| format!("rss text {url}"))?;
    let items = parse_rss_items(&body)?;
    Ok(filter_and_take(items, limit))
}

/// 全 enabled トピックを順に fetch し、`topics_cache` に蓄積する。
/// 失敗したトピックは個別にログ出力して継続。古いキャッシュは 7 日で prune。
pub async fn fetch_all_into_cache(state: &Arc<AppState>) -> Result<()> {
    let topics = state.db.list_enabled_topics().map_err(|e| anyhow!("{e:#}"))?;
    if topics.is_empty() {
        return Ok(());
    }
    let now = chrono::Utc::now().timestamp();
    for topic in &topics {
        match fetch_topic(topic, 5).await {
            Ok(items) => {
                for it in items {
                    let _ = state.db.insert_topic_cache(topic, &it.title, &it.link, now);
                }
            }
            Err(err) => {
                eprintln!("[topics] '{topic}' の RSS 取得失敗: {err:#}");
            }
        }
    }
    // 7 日経過したものは捨てる (キャッシュ肥大化防止)
    let week_ago = now - 7 * 24 * 60 * 60;
    let _ = state.db.prune_topics_cache(week_ago);
    Ok(())
}

// ---------- 内部 ----------

fn parse_rss_items(xml: &str) -> Result<Vec<RssItem>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut items = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_link: Option<String> = None;
    let mut in_item = false;
    let mut current_tag: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if name == "item" {
                    in_item = true;
                    current_title = None;
                    current_link = None;
                } else if in_item {
                    current_tag = Some(name);
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if name == "item" {
                    in_item = false;
                    if let (Some(t), Some(l)) = (current_title.take(), current_link.take()) {
                        items.push(RssItem {
                            title: t,
                            link: l,
                        });
                    }
                } else if Some(name) == current_tag {
                    current_tag = None;
                }
            }
            Ok(Event::Text(e)) => {
                if in_item {
                    let txt = e.unescape().map(|c| c.into_owned()).unwrap_or_default();
                    match current_tag.as_deref() {
                        Some("title") => current_title = Some(txt),
                        Some("link") => current_link = Some(txt),
                        _ => {}
                    }
                }
            }
            Ok(Event::CData(e)) => {
                if in_item {
                    // <title><![CDATA[...]]></title> 形式に対応
                    let txt = String::from_utf8_lossy(e.as_ref()).into_owned();
                    match current_tag.as_deref() {
                        Some("title") => current_title = Some(txt),
                        Some("link") => current_link = Some(txt),
                        _ => {}
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow!("RSS パース失敗: {e}")),
            _ => {}
        }
        buf.clear();
    }
    Ok(items)
}

fn filter_and_take(items: Vec<RssItem>, limit: usize) -> Vec<RssItem> {
    items
        .into_iter()
        .filter(|it| !is_dark_headline(&it.title))
        .take(limit)
        .collect()
}

fn is_dark_headline(title: &str) -> bool {
    DARK_KEYWORDS.iter().any(|kw| title.contains(kw))
}

fn urlencode(s: &str) -> String {
    // RFC 3986 の unreserved 以外を %xx エンコード
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_keyword_filters_obvious_cases() {
        assert!(is_dark_headline("〇〇選手が事故死"));
        assert!(is_dark_headline("テロ事件の続報"));
        assert!(!is_dark_headline("新作アニメ公開"));
        assert!(!is_dark_headline("Rust 1.85 がリリース"));
    }

    #[test]
    fn google_news_url_encodes_query() {
        let url = build_google_news_rss_url("Rust 1.85");
        assert!(url.contains("q=Rust%201.85"));
        assert!(url.contains("hl=ja"));
        assert!(url.contains("ceid=JP:ja"));
    }

    #[test]
    fn parse_basic_rss() {
        let xml = r#"<?xml version="1.0"?><rss><channel>
            <item><title>テスト 1</title><link>https://a/1</link></item>
            <item><title><![CDATA[テスト 2]]></title><link>https://a/2</link></item>
            <item><title>事故死のニュース</title><link>https://a/3</link></item>
        </channel></rss>"#;
        let items = parse_rss_items(xml).unwrap();
        assert_eq!(items.len(), 3);
        let filtered = filter_and_take(items, 10);
        // 事故死は除外、2 件残る
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].title, "テスト 1");
        assert_eq!(filtered[1].title, "テスト 2");
    }

    #[test]
    fn urlencode_handles_japanese() {
        let s = urlencode("アニメ");
        // UTF-8 で 9 バイト (3 文字 × 3 バイト)、全部 %xx で 27 文字
        assert_eq!(s.len(), 27);
        assert!(s.starts_with("%"));
    }
}

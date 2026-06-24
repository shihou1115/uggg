//! M5-D: 更新通知 (`update_feed_url` をベースにした緩い更新案内)。
//!
//! - `update_feed_url` (settings) が未設定なら no-op
//! - JSON フィード: `{ "latest": "0.2.0", "url": "https://...", "notes": "..." }`
//! - 比較は major.minor.patch を u32 三項組で。プレリリースタグは無視 (本開発はシンプル運用)
//! - 重複告知防止: `app_settings."update_notice_seen:<version>"` に "1" を書いて、同じ版は再告知しない
//!
//! spec §5: 自動更新は行わない (コード署名がないため)。本機能は **手動 DL & 再インストール** を促す案内のみ。

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use tauri::AppHandle;

use crate::state::AppState;
use crate::system::notify::{self, NoticeKind};

/// 自バージョン (CARGO_PKG_VERSION) と feed の `latest` を比較。
/// 新しいバージョンが見つかれば notify(UpdateAvailable) を 1 度だけ発火する。
pub async fn check_update_once(app: &AppHandle, state: &Arc<AppState>) -> Result<()> {
    let feed_url = {
        let s = state.settings.lock().expect("settings poisoned");
        s.update_feed_url.clone()
    };
    let Some(url) = feed_url else {
        return Ok(());
    };
    let feed: UpdateFeed = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .with_context(|| format!("update feed 取得: {url}"))?
        .error_for_status()
        .with_context(|| format!("update feed status: {url}"))?
        .json()
        .await
        .with_context(|| "update feed の JSON 解析に失敗")?;

    let current = parse_version(env!("CARGO_PKG_VERSION"))
        .ok_or_else(|| anyhow!("自バージョン文字列が parse できません"))?;
    let latest =
        parse_version(&feed.latest).ok_or_else(|| anyhow!("latest 文字列が parse できません"))?;

    if !is_newer(latest, current) {
        return Ok(());
    }

    let seen_key = format!("update_notice_seen:{}", feed.latest);
    if let Ok(Some(_)) = state.db.get_setting(&seen_key) {
        return Ok(()); // 同じ版は二度告知しない
    }
    notify::notify(
        app,
        state,
        NoticeKind::UpdateAvailable {
            version: feed.latest.clone(),
        },
    )
    .await;
    let _ = state.db.set_setting(&seen_key, "1");
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateFeed {
    latest: String,
    #[allow(dead_code)]
    url: Option<String>,
    #[allow(dead_code)]
    notes: Option<String>,
}

/// "0.2.0" / "0.2.0-dev.3" → Some((major, minor, patch))。プレリリース部は捨てる。
fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let core = v.split(['-', '+']).next().unwrap_or(v);
    let mut it = core.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next()?.parse().ok()?;
    Some((major, minor, patch))
}

fn is_newer(latest: (u32, u32, u32), current: (u32, u32, u32)) -> bool {
    latest > current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        assert_eq!(parse_version("0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
    }

    #[test]
    fn parse_pre_release_drops_suffix() {
        assert_eq!(parse_version("0.1.0-dev.3"), Some((0, 1, 0)));
        assert_eq!(parse_version("1.0.0+meta"), Some((1, 0, 0)));
    }

    #[test]
    fn parse_invalid() {
        assert_eq!(parse_version("v0.1.0"), None);
        assert_eq!(parse_version("0.1"), None);
        assert_eq!(parse_version("abc"), None);
    }

    #[test]
    fn is_newer_basic() {
        assert!(is_newer((0, 2, 0), (0, 1, 9)));
        assert!(is_newer((1, 0, 0), (0, 9, 9)));
        assert!(!is_newer((0, 1, 0), (0, 1, 0)));
        assert!(!is_newer((0, 1, 0), (0, 2, 0)));
    }
}

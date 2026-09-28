//! M5-D: 更新通知 (`update_feed_url` をベースにした緩い更新案内)。
//!
//! - `update_feed_url` (settings) が未設定なら no-op
//! - JSON フィード: `{ "latest": "0.2.0", "url": "https://...", "notes": "..." }`
//! - 比較は major.minor.patch を u32 三項組で。プレリリースタグは無視 (本開発はシンプル運用)
//! - 重複告知防止: `app_settings."update_notice_seen:<version>"` に "1" を書いて、同じ版は再告知しない。
//!   **書くのは届いたときだけ**（v0.5.6 項目 6。以前は届いたかを見ずに書き、隠している間に出るとその版は二度と出なかった）
//!
//! spec §5: 自動更新は行わない (コード署名がないため)。本機能は **手動 DL & 再インストール** を促す案内のみ。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use tauri::AppHandle;

use crate::state::AppState;
use crate::system::log::{url_for_log, StripUrl};
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
        // 小さな JSON なので全体に上限を付ける（v0.5.6 項目 2 の掃討。上限が無いと、手動の
        // 「更新を確認」が相手の沈黙で戻らなくなる）。
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .strip_url()
        // フィードの URL はユーザーの設定。ホストまでしか書かない（spec §5、v0.5.8 項目 1）
        .with_context(|| format!("update feed 取得: {}", url_for_log(&url)))?
        .error_for_status()
        .strip_url()
        .with_context(|| format!("update feed status: {}", url_for_log(&url)))?
        .json()
        .await
        .strip_url()
        .with_context(|| "update feed の JSON 解析に失敗")?;

    let current = parse_version(env!("CARGO_PKG_VERSION"))
        .ok_or_else(|| anyhow!("自バージョン文字列が parse できません"))?;
    let latest =
        parse_version(&feed.latest).ok_or_else(|| anyhow!("latest 文字列が parse できません"))?;

    if !is_newer(latest, current) {
        return Ok(());
    }

    let seen_key = format!("update_notice_seen:{}", feed.latest);
    let done = matches!(state.db.get_setting(&seen_key), Ok(Some(_)));
    // **告知済みで出さなかったことも 1 行残す**（v0.5.7 の実機検証 E-11 の 7a。出ないときに「対象が無い」のか
    // 「告知済み」なのかがログから分からなかった）。確かめるのは起動の 30 秒後と 24 時間ごとだけ。
    if done {
        crate::ulog!("[update] 新しい版 {} があります（告知済みなので出しません）", feed.latest);
    }
    // 同じ版は二度告知しない。**済みにするのは届いたときだけ**（v0.5.6 項目 6）
    notify::once_reached(
        done,
        || {
            notify::notify(
                app,
                state,
                NoticeKind::UpdateAvailable {
                    version: feed.latest.clone(),
                },
            )
        },
        || {
            let _ = state.db.set_setting(&seen_key, "1");
        },
    )
    .await;
    Ok(())
}

/// **Irodori のランタイムに更新があれば告知する**（v0.5.7 項目 7、spec §6.0）。以前は設定パネルを開いた人にしか
/// 見えず、乗り換え（数 GB の更新で届く）が届いたことに気付けなかった。**導入済みの人だけ**、**更新の錠が
/// 空いているときだけ**確かめる（`status()` は記録に基準値を書き足し、python.exe を起動することもある）。
/// 更新の対象の組ごとに 1 回、**届いたときだけ済みにする**（`once_reached`。v0.5.6 項目 6）。
pub async fn check_irodori_update_once(app: &AppHandle, state: &Arc<AppState>) -> Result<()> {
    let root = crate::tts::voice_ref::irodori_root()?;
    let targets = tauri::async_runtime::spawn_blocking(move || {
        irodori_update_targets(
            crate::tts::irodori_download::assets_ready(&root),
            crate::tts::irodori_download::is_busy_for(&root),
            || crate::tts::irodori_download::status(&root).outdated,
        )
    })
    .await
    .map_err(|e| anyhow!("Irodori の更新の確認が中断しました: {e}"))?;
    let Some(targets) = targets else {
        crate::ulog!("[update] Irodori のランタイムの更新の告知: 対象なし（未導入・更新中・最新のどれか）");
        return Ok(());
    };
    let seen_key = irodori_update_seen_key(&targets);
    let done = matches!(state.db.get_setting(&seen_key), Ok(Some(_)));
    // **告知済みで出さなかったことも 1 行残す**（v0.5.7 の実機検証 E-11 の 7a で、出ない理由がログから分からなかった）
    if done {
        crate::ulog!(
            "[update] Irodori のランタイムに更新があります（{}。この組は告知済みなので出しません）",
            targets.join(" / ")
        );
    }
    notify::once_reached(
        done,
        || {
            notify::notify(
                app,
                state,
                NoticeKind::IrodoriUpdateAvailable { targets: targets.clone() },
            )
        },
        || {
            let _ = state.db.set_setting(&seen_key, "1");
        },
    )
    .await;
    Ok(())
}

/// 告知する更新の対象（純関数）。**導入済みで、錠が空いていて、対象があるときだけ** `Some`。
/// `outdated` は錠を見たあとでしか呼ばない（呼ぶと記録に書き足しうる）。
pub(crate) fn irodori_update_targets(
    present: bool,
    busy: bool,
    outdated: impl FnOnce() -> Vec<String>,
) -> Option<Vec<String>> {
    if !present || busy {
        return None;
    }
    let targets = outdated();
    (!targets.is_empty()).then_some(targets)
}

/// 同じ更新の対象の組は二度告知しない（組が変われば、それは新しい更新なのでまた告知する）。
pub(crate) fn irodori_update_seen_key(targets: &[String]) -> String {
    let mut sorted = targets.to_vec();
    sorted.sort();
    format!("irodori_update_notice_seen:{}", sorted.join(","))
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

    /// **告知しなかった理由もログに残す**（v0.5.7 の実機検証 E-11 の 7a）。対象が無いときと、告知済みで出さないとき。
    /// 告知済みかどうかは 1 回だけ読み、その同じ値を `once_reached` に渡す（読み直すとログと判断が食い違いうる）。
    #[test]
    fn a_skipped_update_notice_leaves_its_reason_in_the_log() {
        let src = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/system/update.rs"))
            .unwrap()
            .replace("\r\n", "\n");
        let body_of = |name: &str| -> String {
            let rest = &src[src.find(name).unwrap_or_else(|| panic!("{name} が無い"))..];
            rest[..rest.find("\n}\n").unwrap()].to_string()
        };
        for (name, logged) in [
            ("pub async fn check_update_once", "（告知済みなので出しません）"),
            ("pub async fn check_irodori_update_once", "この組は告知済みなので出しません"),
        ] {
            let body = body_of(name);
            let done = body.find("let done = matches!(state.db.get_setting(&seen_key), Ok(Some(_)));").expect(name);
            let log = body.find(logged).unwrap_or_else(|| panic!("{name}: 告知済みで出さないことをログに残していない"));
            let once = body.find("notify::once_reached(\n        done,").unwrap_or_else(|| panic!("{name}: 読んだ値を渡していない"));
            assert!(done < log && log < once, "{name}: 順序");
            assert_eq!(body.matches("get_setting(&seen_key)").count(), 1, "{name}: 告知済みを 2 回読んでいる");
        }
        assert!(body_of("pub async fn check_irodori_update_once").contains("対象なし（未導入・更新中・最新のどれか）"));
    }

    /// **導入済みで、更新の錠が空いていて、対象があるときだけ告知する**（v0.5.7 項目 7）。錠が握られている・
    /// 未導入のときは**対象を確かめること自体をしない**（`status()` は記録に書き足し、python.exe を起動しうる）。
    #[test]
    fn the_irodori_update_is_told_only_when_installed_idle_and_outdated() {
        use std::cell::Cell;
        let asked = Cell::new(0);
        let outdated = |v: &'static [&'static str]| {
            let asked = &asked;
            move || {
                asked.set(asked.get() + 1);
                v.iter().map(|s| s.to_string()).collect::<Vec<_>>()
            }
        };
        assert_eq!(irodori_update_targets(false, false, outdated(&["transformers"])), None, "未導入");
        assert_eq!(irodori_update_targets(true, true, outdated(&["transformers"])), None, "錠が握られている");
        assert_eq!(asked.get(), 0, "未導入・錠のときは対象を確かめない");
        assert_eq!(irodori_update_targets(true, false, outdated(&[])), None, "最新");
        assert_eq!(
            irodori_update_targets(true, false, outdated(&["model_synth", "transformers"])),
            Some(vec!["model_synth".to_string(), "transformers".to_string()])
        );
    }

    /// 同じ対象の組は二度告知しない（並びが違っても同じ組）。組が変われば新しい更新として告知する。
    #[test]
    fn the_irodori_update_is_told_once_per_set_of_targets() {
        let a = irodori_update_seen_key(&["transformers".into(), "model_synth".into()]);
        let b = irodori_update_seen_key(&["model_synth".into(), "transformers".into()]);
        let c = irodori_update_seen_key(&["model_synth".into()]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("irodori_update_notice_seen:"));
    }

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

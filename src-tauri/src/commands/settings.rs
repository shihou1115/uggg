use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};

use crate::state::{AppState, Settings};

pub(crate) const SETTINGS_KEY: &str = "settings";

#[tauri::command]
pub fn get_settings(state: State<'_, Arc<AppState>>) -> Settings {
    state.settings.lock().expect("settings poisoned").clone()
}

#[tauri::command]
pub fn set_settings(
    settings: Settings,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Settings, String> {
    let mut next = settings;
    next.clamp();

    // メモリ反映前の値と比較する (🔕 backoff リセット・天気キャッシュ消去の判定に使う)。
    let prev = state.settings.lock().expect("settings poisoned").clone();

    // M9/M11: Situation*/Regular* を OFF→ON に戻したら 🔕 backoff をリセットする
    // (reviewer 指摘。恒久 throttle が再有効化後も残り、理由の見えないまま間隔が
    // 絞られるのを防ぐ)。
    for cat in crate::system::governance::feedback_reenabled(&prev, &next) {
        crate::system::governance::reset_backoff(state.inner(), cat);
    }

    // M11: 天気の「解除」(weather_ready: true→false、同意撤回) では
    // app_settings の weather_cache も消す (regular-talk-design §9.2、設定行為 = 同意
    // の対称。地名・キャッシュを残さない)。
    if prev.weather_ready() && !next.weather_ready() {
        crate::system::weather::clear_cache(state.inner());
    }

    // M14: 時事ネタの同意を撤回 (topics_enabled: true→false) したら、既に織り込み済みの
    // 独り言ストックを捨てる (foundation-design §3.6)。同意を外したのに、補充済みの
    // 時事ネタ入りの文を最大 7 日間喋り続けるのを防ぐ (天気の「解除」と同じ作法)。
    if prev.topics_enabled && !next.topics_enabled {
        match state.db.clear_monologue_cache_with_topics() {
            Ok(n) if n > 0 => println!("[settings] 時事ネタ入りの独り言ストックを {n} 件削除"),
            Ok(_) => {}
            Err(err) => crate::ulog!("[settings] 独り言ストックの掃除に失敗: {err:#}"),
        }
    }

    // v0.5.1: ゴーストを切り替えたら、そのゴーストが `ghost.json` で宣言している
    // `default_shell` へシェルも追従させる (spec §4.5.6)。
    //
    // v0.4.1 まで `default_shell` は出荷されているのに `GhostManifest` が宣言して
    // おらず、serde が黙って捨てていた。DnD で入れたゴーストが自前シェルを
    // 連れてこず、ユーザーが手でシェルも選び直す必要があった。
    //
    // **ユーザーが同じ操作でシェルも明示的に変えた場合は、そちらを優先する**
    // (設定画面はゴーストとシェルを同時に保存するため、両方変えたときに
    // ゴースト側の宣言で上書きすると手動選択が握り潰される)。
    if next.ghost_id != prev.ghost_id && next.shell_id == prev.shell_id {
        if let Some(shell) = default_shell_of(&app, &next.ghost_id) {
            if shell != next.shell_id {
                crate::ulog!(
                    "[settings] ゴースト {} の default_shell に追従: {} -> {}",
                    next.ghost_id, next.shell_id, shell
                );
                next.shell_id = shell;
            }
        }
    }

    // 永続化 (app_settings."settings" に JSON で保存)
    let json = serde_json::to_string(&next)
        .map_err(|e| format!("Settings の JSON シリアライズ失敗: {e}"))?;
    state
        .db
        .set_setting(SETTINGS_KEY, &json)
        .map_err(|err| format!("{err:#}"))?;

    // メモリ反映
    {
        let mut guard = state.settings.lock().expect("settings poisoned");
        *guard = next.clone();
    }

    // フロントへ変更通知 (settings-changed)
    let _ = app.emit("settings-changed", &next);
    Ok(next)
}

/// AppState::initialize で呼び出す: 起動時に DB から Settings を復元する。
/// レコードが無い / パース失敗時は引数の `current` をそのまま返す (デフォルト値が温存される)。
pub fn load_persisted_settings(db: &crate::db::Db, current: Settings) -> Settings {
    let stored = match db.get_setting(SETTINGS_KEY) {
        Ok(Some(v)) => v,
        _ => return current,
    };
    match serde_json::from_str::<Settings>(&stored) {
        Ok(mut s) => {
            s.clamp();
            s
        }
        Err(_) => current,
    }
}

/// 設定画面に出す当月のコスト状況 (spec §4.2.7)。
///
/// `manual.md` は以前から「設定 → AI・拡張ページで上限と現在の状況を確認して
/// ください」と案内していたが、**表示先が存在しなかった**。
#[derive(Debug, serde::Serialize)]
pub struct CostStatusView {
    /// 当月の推定利用額 (USD)。料金表は概算で誤差 ±20% を想定する。
    pub current_usd: f64,
    /// 設定中の月額上限 (USD)。
    pub limit_usd: f64,
    /// 上限を設けていない (0 以下)。
    pub unlimited: bool,
    /// 上限に対する比率。
    pub ratio: f64,
    pub reached_80: bool,
    pub exceeded: bool,
    /// 現在のモデルが料金表に載っているか。
    pub pricing_known: bool,
    /// **料金を計算できないのに課金されうる**構成か。
    ///
    /// 未掲載モデルはコスト 0 で記録されるため上限が発動しない。ローカル LLM
    /// なら 0 が正しいので、`base_url` がローカルを指していない場合のみ真になる。
    pub pricing_unknown_remote: bool,
}

/// `llm_base_url` がローカルの LLM サーバを指しているか。
fn base_url_is_local(base_url: Option<&str>) -> bool {
    let Some(url) = base_url else {
        // 未設定 = OpenAI 公式。ローカルではない。
        return false;
    };
    // 部分一致だと `https://localhost.example.com` のような**リモート**を
    // ローカルと誤判定するので、ホスト部を正確に切り出して完全一致で見る。
    let u = url.trim().to_ascii_lowercase();
    let after_scheme = u.split_once("//").map(|(_, rest)| rest).unwrap_or(&u);
    // 認証情報が付く形 (user:pass@host) にも備えて @ の後ろを取る。
    let hostport = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    // IPv6 は [::1]:1234 の形。角括弧の中をホストとして扱う。
    let host = if let Some(rest) = hostport.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        hostport.split(':').next().unwrap_or("")
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "0.0.0.0")
}

#[tauri::command]
pub fn get_cost_status(state: State<'_, Arc<AppState>>) -> Result<CostStatusView, String> {
    let settings = state.settings.lock().expect("settings poisoned").clone();
    let status = crate::system::cost::check_status(&state.db, settings.monthly_limit_usd)
        .map_err(|e| format!("コスト集計に失敗しました: {e:#}"))?;
    let pricing_known = crate::dialogue::llm::is_priced(&settings.llm_model);
    Ok(CostStatusView {
        current_usd: status.current_usd,
        limit_usd: status.limit_usd,
        unlimited: status.unlimited,
        ratio: status.ratio,
        reached_80: status.reached_80,
        exceeded: status.exceeded,
        pricing_known,
        pricing_unknown_remote: !pricing_known
            && !base_url_is_local(settings.llm_base_url.as_deref()),
    })
}

#[cfg(test)]
mod cost_status_tests {
    use super::base_url_is_local;

    #[test]
    fn local_endpoints_are_detected() {
        for u in [
            "http://127.0.0.1:1234/v1",
            "http://localhost:11434/v1",
            "HTTP://LOCALHOST:1234/V1",
            "http://[::1]:8080/v1",
        ] {
            assert!(base_url_is_local(Some(u)), "{u}");
        }
    }

    #[test]
    fn remote_endpoints_are_not_local() {
        // base_url 未設定 = OpenAI 公式。ローカル扱いにしてはいけない。
        assert!(!base_url_is_local(None));
        for u in [
            "https://api.openai.com/v1",
            "https://openrouter.ai/api/v1",
            // ホスト名にそれらしい語が入っていても騙されないこと。
            "https://localhost.example.com/v1",
            "https://127.0.0.1.evil.example/v1",
            "https://api.example.com/localhost/v1",
        ] {
            assert!(!base_url_is_local(Some(u)), "{u}");
        }
    }
}

/// 指定ゴーストの `ghost.json` が宣言する `default_shell`。
///
/// 起動中のゴーストとは限らない（切替先を先読みする）ため、`state.ghost` の
/// ロード済み bundle ではなくファイルを直接読む。読めない・宣言が無い・
/// 実体のシェルが無い場合は None（追従しない）。
fn default_shell_of(app: &AppHandle, ghost_id: &str) -> Option<String> {
    let assets = crate::state::resolve_assets_dir(app).ok()?;
    let raw = std::fs::read_to_string(assets.join("ghosts").join(ghost_id).join("ghost.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let shell = v.get("default_shell")?.as_str()?.trim().to_string();
    if shell.is_empty() {
        return None;
    }
    // 実体が無いシェルへ切り替えると起動できなくなるので、存在確認してから採用する。
    assets
        .join("shells")
        .join(&shell)
        .join("shell.json")
        .is_file()
        .then_some(shell)
}

#[cfg(test)]
mod default_shell_tests {
    /// 出荷している既定ゴーストが `default_shell` を宣言していること。
    ///
    /// v0.5.1 でこの値を読むようになった（ゴースト切替でシェルが追従、spec §4.5.6）。
    /// v0.4.1 までは出荷されているのに `GhostManifest` が宣言しておらず、serde が
    /// 黙って捨てていた。
    #[test]
    fn default_ghost_declares_default_shell_that_exists() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri の親")
            .to_path_buf();
        let raw = std::fs::read_to_string(root.join("ghosts/default/ghost.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let shell = v
            .get("default_shell")
            .and_then(|x| x.as_str())
            .expect("既定ゴーストに default_shell が無い");
        assert!(
            root.join("shells").join(shell).join("shell.json").is_file(),
            "default_shell '{shell}' の実体が無い（追従先が存在しないと切替で壊れる）"
        );
    }
}

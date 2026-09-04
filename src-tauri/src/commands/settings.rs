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

//! 天気基盤 (M11、spec §4.7.2 / regular-talk-design §2・§3)。
//!
//! 責務 (§3.1):
//! 1. 予報の取得と鮮度管理 (`ensure_fresh` / `refresh_cache`)
//! 2. 定例会話・降雨の一言・advanced 会話への材料提供 (`today_material` / `tomorrow_material`)
//! 3. WMO weather code → 日本語ラベル変換 (`weather_label`、§3.4)
//!
//! API は Open-Meteo (forecast、キー不要・無料。裁定は §2.1)。HTTP は既存実装
//! (calendar/topics) と同型: reqwest 都度生成・15 秒タイムアウト・User-Agent 未設定 (§11-4)。
//!
//! キャッシュは `app_settings["weather_cache"]` に JSON で保存する (新テーブルなし、§3.2)。
//! 専用の delete API を DB に持たないため、「解除」時のクリアは空文字を書き込むことで
//! 表現する (`clear_cache`。空文字は JSON として parse 不能 = `load_cache` が自然に None を返す)。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// 定期取得の間隔 (daily watcher の 60 秒 tick 内で判定、§3.3)。
pub const WEATHER_FETCH_INTERVAL_SECS: i64 = 3 * 3600;
/// これを超えたキャッシュは材料として使わず天気項目を省く (spec §4.7.2 既定 6 時間、§3.3)。
pub const WEATHER_STALE_SECS: i64 = 6 * 3600;
/// 降雨判定の降水確率しきい値 (§3.3)。
pub const RAIN_PROB_THRESHOLD: u8 = 50;

/// 座標の同一判定の許容誤差。clamp() で小数 1 桁に丸められているため、
/// 同一設定であれば bit 単位で一致するはずだが浮動小数の保険として持つ。
const COORD_EPSILON: f64 = 1e-6;

const WEATHER_CACHE_KEY: &str = "weather_cache";

/// 天気キャッシュ (§3.2)。Serialize + Deserialize は DB 行型と違い双方向
/// (JSON キャッシュの読み戻しに必要)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeatherCache {
    /// 取得時刻 (UTC 秒)。
    pub fetched_ts: i64,
    /// 取得に使った丸め座標 (設定変更の検知用)。
    pub latitude: f64,
    pub longitude: f64,
    /// [今日, 明日] (地点ローカル日付、API の timezone=auto が返す)。
    pub daily: Vec<DailyWeather>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyWeather {
    /// "YYYY-MM-DD" (地点ローカル日付)。
    pub date: String,
    /// WMO weather code。
    pub weather_code: u8,
    pub temp_max: f64,
    pub temp_min: f64,
    /// 0-100。
    pub precip_prob_max: u8,
}

// ===== WMO weather code → 日本語ラベル (§3.4、確定表) =====

/// WMO code → 日本語ラベル。表にない code はラベルなし (天気項目を省く)。
pub fn weather_label(code: u8) -> Option<&'static str> {
    match code {
        0 => Some("快晴"),
        1 => Some("晴れ"),
        2 => Some("晴れ時々くもり"),
        3 => Some("くもり"),
        45 | 48 => Some("霧"),
        51 | 53 | 55 | 56 | 57 => Some("霧雨"),
        61 | 63 | 65 | 66 | 67 => Some("雨"),
        71 | 73 | 75 | 77 => Some("雪"),
        80 | 81 | 82 => Some("にわか雨"),
        85 | 86 => Some("にわか雪"),
        95 | 96 | 99 => Some("雷雨"),
        _ => None,
    }
}

/// 雨系 code か (降雨判定に含む。雪系は含めない、§3.4)。
fn is_rain_code(code: u8) -> bool {
    matches!(
        code,
        51 | 53 | 55 | 56 | 57 | 61 | 63 | 65 | 66 | 67 | 80 | 81 | 82 | 95 | 96 | 99
    )
}

/// 降雨判定: 降水確率がしきい値以上、または雨系 code (§3.3)。
pub fn is_rain(d: &DailyWeather) -> bool {
    d.precip_prob_max >= RAIN_PROB_THRESHOLD || is_rain_code(d.weather_code)
}

// ===== 材料の取り出し (date 文字列一致、§3.3) =====

/// 今日の材料。ローカル今日の日付と `daily[].date` が一致する要素 (無ければ None)。
pub fn today_material(cache: &WeatherCache) -> Option<&DailyWeather> {
    material_for_date(cache, chrono::Local::now().date_naive())
}

/// 明日の材料。夜の定例会話 (M12) が消費する。M11 時点では未結線のため
/// `#[allow(dead_code)]` (§3 の weather.rs 責務として today_material と対で今 M に含める)。
#[allow(dead_code)]
pub fn tomorrow_material(cache: &WeatherCache) -> Option<&DailyWeather> {
    material_for_date(cache, chrono::Local::now().date_naive() + chrono::Duration::days(1))
}

fn material_for_date(cache: &WeatherCache, date: chrono::NaiveDate) -> Option<&DailyWeather> {
    let target = date.to_string();
    cache.daily.iter().find(|d| d.date == target)
}

// ===== 鮮度管理・キャッシュ (§3.2・§3.3) =====

fn load_cache(state: &Arc<AppState>) -> Option<WeatherCache> {
    let raw = state.db.get_setting(WEATHER_CACHE_KEY).ok().flatten()?;
    serde_json::from_str(&raw).ok()
}

fn save_cache(state: &Arc<AppState>, cache: &WeatherCache) {
    if let Ok(json) = serde_json::to_string(cache) {
        let _ = state.db.set_setting(WEATHER_CACHE_KEY, &json);
    }
}

/// 天気データの完全消去 (§9.2「解除」= 同意撤回の対称)。
pub fn clear_cache(state: &Arc<AppState>) {
    let _ = state.db.set_setting(WEATHER_CACHE_KEY, "");
}

/// オンデマンドの鮮度確認 (§3.3)。キャッシュが `WEATHER_STALE_SECS` 以内かつ
/// 現在の設定座標と一致すればそれを返す。超えていれば 1 回取得を試み、
/// 成功なら新キャッシュ、失敗なら None (天気項目を省く)。天気未設定なら常に None。
pub async fn ensure_fresh(state: &Arc<AppState>) -> Option<WeatherCache> {
    let (lat, lon) = {
        let s = state.settings.lock().expect("settings poisoned");
        if !s.weather_ready() {
            return None;
        }
        (s.weather_latitude?, s.weather_longitude?)
    };
    let now = chrono::Utc::now().timestamp();
    let cached = load_cache(state);
    let stale = match &cached {
        None => true,
        Some(c) => {
            now - c.fetched_ts > WEATHER_STALE_SECS
                || (c.latitude - lat).abs() > COORD_EPSILON
                || (c.longitude - lon).abs() > COORD_EPSILON
        }
    };
    if !stale {
        return cached;
    }
    match fetch_forecast(lat, lon).await {
        Ok(fresh) => {
            save_cache(state, &fresh);
            Some(fresh)
        }
        Err(err) => {
            eprintln!("[weather] ensure_fresh の取得に失敗: {err:#}");
            None
        }
    }
}

/// 定期取得 (daily watcher の 3h tick、§5.1)。鮮度に関わらず取得を試み、
/// 成功したらキャッシュを更新する。失敗時は既存キャッシュを維持する
/// (calendar watcher の fetch_all_calendars と同型)。天気未設定なら no-op。
pub async fn refresh_cache(state: &Arc<AppState>) -> bool {
    let (lat, lon) = {
        let s = state.settings.lock().expect("settings poisoned");
        match (s.weather_ready(), s.weather_latitude, s.weather_longitude) {
            (true, Some(lat), Some(lon)) => (lat, lon),
            _ => return false,
        }
    };
    match fetch_forecast(lat, lon).await {
        Ok(cache) => {
            save_cache(state, &cache);
            true
        }
        Err(err) => {
            eprintln!("[weather] 定期取得に失敗: {err:#}");
            false
        }
    }
}

// ===== 取得 (Open-Meteo forecast API、§2.2) =====

pub fn build_forecast_url(latitude: f64, longitude: f64) -> String {
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={latitude}&longitude={longitude}\
         &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max\
         &timezone=auto&forecast_days=2"
    )
}

#[derive(Debug, Deserialize)]
struct ForecastResponseRaw {
    daily: ForecastDailyRaw,
}

#[derive(Debug, Deserialize)]
struct ForecastDailyRaw {
    time: Vec<String>,
    weather_code: Vec<Option<u8>>,
    temperature_2m_max: Vec<Option<f64>>,
    temperature_2m_min: Vec<Option<f64>>,
    precipitation_probability_max: Vec<Option<u8>>,
}

/// レスポンス本文 → Vec<DailyWeather>。いずれかの値が欠けている日は丸ごと省く
/// (天気項目を省く原則を取得段階でも適用、部分的に壊れたデータで発話しない)。
fn parse_forecast_body(raw: &str) -> Result<Vec<DailyWeather>> {
    let resp: ForecastResponseRaw =
        serde_json::from_str(raw).context("天気予報の JSON パース失敗")?;
    let d = resp.daily;
    let n = d.time.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let (Some(code), Some(tmax), Some(tmin), Some(prob)) = (
            d.weather_code.get(i).copied().flatten(),
            d.temperature_2m_max.get(i).copied().flatten(),
            d.temperature_2m_min.get(i).copied().flatten(),
            d.precipitation_probability_max.get(i).copied().flatten(),
        ) else {
            continue;
        };
        out.push(DailyWeather {
            date: d.time[i].clone(),
            weather_code: code,
            temp_max: tmax,
            temp_min: tmin,
            precip_prob_max: prob,
        });
    }
    Ok(out)
}

/// 予報の取得 (§2.2)。タイムアウト 15 秒、User-Agent 指定なし。
pub async fn fetch_forecast(latitude: f64, longitude: f64) -> Result<WeatherCache> {
    let url = build_forecast_url(latitude, longitude);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("HTTP クライアント構築失敗")?;
    let body = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("天気予報の取得に失敗: {url}"))?
        .error_for_status()
        .with_context(|| format!("天気予報の取得が HTTP エラー: {url}"))?
        .text()
        .await
        .context("天気予報の応答取得に失敗")?;
    let daily = parse_forecast_body(&body)?;
    Ok(WeatherCache {
        fetched_ts: chrono::Utc::now().timestamp(),
        latitude,
        longitude,
        daily,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(date: &str, code: u8, prob: u8) -> DailyWeather {
        DailyWeather {
            date: date.to_string(),
            weather_code: code,
            temp_max: 30.0,
            temp_min: 20.0,
            precip_prob_max: prob,
        }
    }

    #[test]
    fn weather_label_covers_full_table() {
        let cases: &[(u8, &str)] = &[
            (0, "快晴"),
            (1, "晴れ"),
            (2, "晴れ時々くもり"),
            (3, "くもり"),
            (45, "霧"),
            (48, "霧"),
            (51, "霧雨"),
            (53, "霧雨"),
            (55, "霧雨"),
            (56, "霧雨"),
            (57, "霧雨"),
            (61, "雨"),
            (63, "雨"),
            (65, "雨"),
            (66, "雨"),
            (67, "雨"),
            (71, "雪"),
            (73, "雪"),
            (75, "雪"),
            (77, "雪"),
            (80, "にわか雨"),
            (81, "にわか雨"),
            (82, "にわか雨"),
            (85, "にわか雪"),
            (86, "にわか雪"),
            (95, "雷雨"),
            (96, "雷雨"),
            (99, "雷雨"),
        ];
        for (code, label) in cases {
            assert_eq!(weather_label(*code), Some(*label), "code={code}");
        }
        // 未知 code はラベルなし
        for code in [4u8, 50, 60, 100, 200] {
            assert_eq!(weather_label(code), None, "code={code}");
        }
    }

    #[test]
    fn rain_judgement_by_code_or_probability() {
        // 雨系 code は確率が低くても降雨扱い
        assert!(is_rain(&sample("2026-07-24", 61, 10)));
        assert!(is_rain(&sample("2026-07-24", 80, 0)));
        // 雷雨も降雨扱い
        assert!(is_rain(&sample("2026-07-24", 95, 0)));
        // 雪系は降雨に含めない (確率が低ければ false)
        assert!(!is_rain(&sample("2026-07-24", 71, 10)));
        // 晴れでも確率がしきい値以上なら降雨扱い
        assert!(is_rain(&sample("2026-07-24", 1, 50)));
        assert!(!is_rain(&sample("2026-07-24", 1, 49)));
        // 晴れ・低確率は降雨ではない
        assert!(!is_rain(&sample("2026-07-24", 0, 0)));
    }

    #[test]
    fn material_for_date_matches_by_string() {
        let cache = WeatherCache {
            fetched_ts: 1000,
            latitude: 35.7,
            longitude: 139.7,
            daily: vec![
                sample("2026-07-24", 1, 10),
                sample("2026-07-25", 61, 80),
            ],
        };
        let today = chrono::NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
        let tomorrow = chrono::NaiveDate::from_ymd_opt(2026, 7, 25).unwrap();
        let missing = chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap();
        assert_eq!(material_for_date(&cache, today).unwrap().weather_code, 1);
        assert_eq!(material_for_date(&cache, tomorrow).unwrap().weather_code, 61);
        // 日付不一致 (深夜跨ぎで daily[0] が過去日になっていた場合等) は None
        assert!(material_for_date(&cache, missing).is_none());
    }

    #[test]
    fn build_forecast_url_contains_required_params() {
        let url = build_forecast_url(35.7, 139.7);
        assert!(url.starts_with("https://api.open-meteo.com/v1/forecast?"));
        assert!(url.contains("latitude=35.7"));
        assert!(url.contains("longitude=139.7"));
        assert!(url.contains("timezone=auto"));
        assert!(url.contains("forecast_days=2"));
        assert!(url.contains("daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max"));
    }

    #[test]
    fn parse_forecast_body_maps_fields_by_index() {
        let raw = r#"{
            "daily": {
                "time": ["2026-07-24", "2026-07-25"],
                "weather_code": [1, 61],
                "temperature_2m_max": [30.1, 28.4],
                "temperature_2m_min": [22.0, 21.0],
                "precipitation_probability_max": [10, 80]
            }
        }"#;
        let daily = parse_forecast_body(raw).unwrap();
        assert_eq!(daily.len(), 2);
        assert_eq!(daily[0].date, "2026-07-24");
        assert_eq!(daily[0].weather_code, 1);
        assert_eq!(daily[0].temp_max, 30.1);
        assert_eq!(daily[0].temp_min, 22.0);
        assert_eq!(daily[0].precip_prob_max, 10);
        assert_eq!(daily[1].date, "2026-07-25");
        assert_eq!(daily[1].weather_code, 61);
        assert_eq!(daily[1].precip_prob_max, 80);
    }

    #[test]
    fn parse_forecast_body_skips_days_with_null_fields() {
        // 2 日目の weather_code が null → その日だけ省く (§3.3 の「省く」原則を取得段階でも適用)
        let raw = r#"{
            "daily": {
                "time": ["2026-07-24", "2026-07-25"],
                "weather_code": [1, null],
                "temperature_2m_max": [30.1, 28.4],
                "temperature_2m_min": [22.0, 21.0],
                "precipitation_probability_max": [10, 80]
            }
        }"#;
        let daily = parse_forecast_body(raw).unwrap();
        assert_eq!(daily.len(), 1);
        assert_eq!(daily[0].date, "2026-07-24");
    }

    #[test]
    fn parse_forecast_body_rejects_malformed_json() {
        assert!(parse_forecast_body("not json").is_err());
        assert!(parse_forecast_body(r#"{"daily": {}}"#).is_err());
    }
}

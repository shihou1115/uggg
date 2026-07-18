//! カレンダー参照: ICS 取得・パース・RRULE near-term 展開 (M10、spec §4.6.4 /
//! daily-support-design §2.3・§7.4)。**読み取り専用**（書き込み・双方向同期・OAuth なし）。
//!
//! 実装の割り切り (§11.1 で確定):
//! - ICS パーサは**自前**（依存を増やさない）。行 unfolding → VEVENT ブロック →
//!   SUMMARY/DTSTART/DTEND/UID/RRULE/EXDATE/RECURRENCE-ID/STATUS を拾う。
//! - **TZ は日本前提の簡易解決**（§2.5 の理想の near-term 実装）:
//!   末尾 `Z`=UTC / `VALUE=DATE`=その日のローカル 0:00 / それ以外（TZID 付き・浮動）=
//!   ローカル TZ 扱い。任意 TZID の VTIMEZONE 完全解決は将来（§11.2）。
//! - **RRULE は near-term 展開のみ**: 表示窓＋通知窓（今日〜N 日）だけ発生行を作る。
//!   FREQ=DAILY/WEEKLY を INTERVAL/BYDAY/UNTIL/COUNT/EXDATE 込みで、MONTHLY/YEARLY は
//!   同日ステップの best-effort。解釈できない RRULE は**当日分だけ**を `unsupported` 印つきで残す。
//!
//! 純粋部分（unfold/パース/展開）は下部の `mod tests` を正とする。取得（File/Url）と
//! DB 反映は `fetch_source_into_cache` が行い、`tasks::spawn_calendar_watcher` が呼ぶ。

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Timelike};

use crate::state::{AppState, CalendarSource};
use crate::tools::reminder::local_to_utc_ts;

/// 終日予定の通知基準時刻（ローカル 8:00。§2.5 / 時間帯マッピングの「朝」と共有）。
pub const ALL_DAY_NOTIFY_TOD_SECS: i64 = 8 * 3600;

/// パース済みの 1 予定（展開前）。start/end はローカル壁時計（UTC 化は展開時）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VEvent {
    pub uid: String,
    pub summary: String,
    pub start: IcsTime,
    pub end: Option<IcsTime>,
    pub rrule: Option<String>,
    pub exdates: Vec<IcsTime>,
    pub recurrence_id: Option<IcsTime>,
    /// STATUS:CANCELLED を反映。
    pub cancelled: bool,
}

/// ICS の時刻値（多形式）。UTC 化は §2.5 に従い `to_utc_ts` で行う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IcsTime {
    /// 終日（VALUE=DATE）。その日のローカル 0:00 を start とする。
    Date(NaiveDate),
    /// 末尾 Z（UTC 絶対時刻）。
    Utc(NaiveDateTime),
    /// 浮動時刻・TZID 付き（簡易にローカル TZ 扱い）。
    Local(NaiveDateTime),
}

impl IcsTime {
    pub fn is_all_day(&self) -> bool {
        matches!(self, IcsTime::Date(_))
    }

    /// UTC 秒へ。Date は「その日のローカル 0:00」、Utc はそのまま、Local はローカル→UTC。
    pub fn to_utc_ts(&self) -> i64 {
        match self {
            IcsTime::Date(d) => local_to_utc_ts(d.and_hms_opt(0, 0, 0).expect("midnight")),
            IcsTime::Utc(dt) => dt.and_utc().timestamp(),
            IcsTime::Local(dt) => local_to_utc_ts(*dt),
        }
    }

    /// 繰り返し展開の「日付」部分（BYDAY/INTERVAL のステップ基準）。
    fn date(&self) -> NaiveDate {
        match self {
            IcsTime::Date(d) => *d,
            IcsTime::Utc(dt) | IcsTime::Local(dt) => dt.date(),
        }
    }

    /// 日付を差し替える（展開で各発生回の時刻を作る。時刻・種別は保持）。
    fn with_date(&self, date: NaiveDate) -> IcsTime {
        match self {
            IcsTime::Date(_) => IcsTime::Date(date),
            IcsTime::Utc(dt) => IcsTime::Utc(date.and_time(dt.time())),
            IcsTime::Local(dt) => IcsTime::Local(date.and_time(dt.time())),
        }
    }
}

/// 展開後の 1 発生インスタンス（DB へ UPSERT する形）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub uid: String,
    pub recurrence_id: Option<String>,
    pub summary: String,
    pub start_ts: i64,
    pub end_ts: Option<i64>,
    pub all_day: bool,
    pub status: String,
    pub unsupported: bool,
}

impl Occurrence {
    /// notify_key = summary|start|end のハッシュ（§2.3、notified 差分検知用）。
    pub fn notify_key(&self) -> String {
        let mut h = DefaultHasher::new();
        self.summary.hash(&mut h);
        self.start_ts.hash(&mut h);
        self.end_ts.hash(&mut h);
        format!("{:016x}", h.finish())
    }
}

// ===== unfolding + ブロック分割 =====

/// RFC5545 の行折り畳みを解除する（次行が空白/タブ始まりなら前行の続き）。CRLF/LF 両対応。
fn unfold(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in raw.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix([' ', '\t']) {
            if let Some(last) = out.last_mut() {
                last.push_str(rest);
                continue;
            }
        }
        out.push(line.to_string());
    }
    out
}

/// プロパティ行を (name, params, value) に分解する。`SUMMARY;X=y:値` → ("SUMMARY", ";X=y", "値")。
fn split_property(line: &str) -> Option<(String, String, String)> {
    let colon = line.find(':')?;
    let (head, value) = line.split_at(colon);
    let value = &value[1..];
    let (name, params) = match head.find(';') {
        Some(semi) => (&head[..semi], &head[semi..]),
        None => (head, ""),
    };
    Some((name.to_ascii_uppercase(), params.to_string(), value.to_string()))
}

/// ICS 全体から VEVENT を抽出してパースする。壊れたブロックはスキップ（fail-soft）。
pub fn parse_ics(raw: &str) -> Vec<VEvent> {
    let lines = unfold(raw);
    let mut out = Vec::new();
    let mut cur: Option<PartialEvent> = None;
    for line in &lines {
        let upper = line.to_ascii_uppercase();
        if upper == "BEGIN:VEVENT" {
            cur = Some(PartialEvent::default());
            continue;
        }
        if upper == "END:VEVENT" {
            if let Some(pe) = cur.take() {
                if let Some(ev) = pe.finish() {
                    out.push(ev);
                }
            }
            continue;
        }
        let Some(pe) = cur.as_mut() else { continue };
        let Some((name, params, value)) = split_property(line) else { continue };
        match name.as_str() {
            "UID" => pe.uid = Some(value),
            "SUMMARY" => pe.summary = Some(unescape(&value)),
            "DTSTART" => pe.start = parse_ics_time(&params, &value),
            "DTEND" => pe.end = parse_ics_time(&params, &value),
            "RRULE" => pe.rrule = Some(value),
            "RECURRENCE-ID" => pe.recurrence_id = parse_ics_time(&params, &value),
            "STATUS" => pe.cancelled = value.eq_ignore_ascii_case("CANCELLED"),
            "EXDATE" => {
                for part in value.split(',') {
                    if let Some(t) = parse_ics_time(&params, part) {
                        pe.exdates.push(t);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[derive(Default)]
struct PartialEvent {
    uid: Option<String>,
    summary: Option<String>,
    start: Option<IcsTime>,
    end: Option<IcsTime>,
    rrule: Option<String>,
    exdates: Vec<IcsTime>,
    recurrence_id: Option<IcsTime>,
    cancelled: bool,
}

impl PartialEvent {
    fn finish(self) -> Option<VEvent> {
        let start = self.start?;
        // UID 欠落は summary+start のハッシュで代替（§2.3）
        let uid = self.uid.unwrap_or_else(|| {
            let mut h = DefaultHasher::new();
            self.summary.as_deref().unwrap_or("").hash(&mut h);
            start.to_utc_ts().hash(&mut h);
            format!("nouid-{:016x}", h.finish())
        });
        Some(VEvent {
            uid,
            summary: self.summary.unwrap_or_else(|| "(無題)".to_string()),
            start,
            end: self.end,
            rrule: self.rrule,
            exdates: self.exdates,
            recurrence_id: self.recurrence_id,
            cancelled: self.cancelled,
        })
    }
}

/// `\n` `\,` `\;` `\\` のアンエスケープ（RFC5545 TEXT）。
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// DTSTART/DTEND/EXDATE 等の値をパースする。params は `;TZID=...;VALUE=DATE` 等。
fn parse_ics_time(params: &str, value: &str) -> Option<IcsTime> {
    let up = params.to_ascii_uppercase();
    let value = value.trim();
    if up.contains("VALUE=DATE") && !up.contains("DATE-TIME") {
        // YYYYMMDD
        let d = NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
        return Some(IcsTime::Date(d));
    }
    // YYYYMMDDTHHMMSS[Z]
    if let Some(stripped) = value.strip_suffix('Z') {
        let dt = NaiveDateTime::parse_from_str(stripped, "%Y%m%dT%H%M%S").ok()?;
        return Some(IcsTime::Utc(dt));
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S") {
        // TZID 付き・浮動ともローカル扱い（§2.5 簡易）
        return Some(IcsTime::Local(dt));
    }
    // 日付のみが VALUE 指定なしで来るケース
    if let Ok(d) = NaiveDate::parse_from_str(value, "%Y%m%d") {
        return Some(IcsTime::Date(d));
    }
    None
}

// ===== RRULE near-term 展開 =====

/// パースした RRULE の near-term 対応サブセット。
struct Rrule {
    freq: Freq,
    interval: i64,
    /// WEEKLY の BYDAY（月=0..日=6）。空なら DTSTART の曜日。
    byday: Vec<u32>,
    until_ts: Option<i64>,
    count: Option<i64>,
    /// near-term 展開が非対応（未知 FREQ 等）。
    unsupported: bool,
}

#[derive(PartialEq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

fn parse_rrule(s: &str) -> Rrule {
    let mut freq = None;
    let mut interval = 1i64;
    let mut byday = Vec::new();
    let mut until_ts = None;
    let mut count = None;
    for part in s.split(';') {
        let Some((k, v)) = part.split_once('=') else { continue };
        match k.to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = match v.to_ascii_uppercase().as_str() {
                    "DAILY" => Some(Freq::Daily),
                    "WEEKLY" => Some(Freq::Weekly),
                    "MONTHLY" => Some(Freq::Monthly),
                    "YEARLY" => Some(Freq::Yearly),
                    _ => None,
                };
            }
            "INTERVAL" => interval = v.parse().unwrap_or(1).max(1),
            "COUNT" => count = v.parse().ok(),
            "UNTIL" => {
                until_ts = parse_ics_time("", v).map(|t| t.to_utc_ts());
            }
            "BYDAY" => {
                for d in v.split(',') {
                    // 末尾 2 文字が曜日略号（先行の序数 +1MO 等は near-term では無視）
                    let code = d.trim_start_matches(|c: char| c == '+' || c == '-' || c.is_ascii_digit());
                    if let Some(w) = weekday_code(code) {
                        byday.push(w);
                    }
                }
            }
            _ => {}
        }
    }
    let unsupported = freq.is_none();
    Rrule {
        freq: freq.unwrap_or(Freq::Daily),
        interval,
        byday,
        until_ts,
        count,
        unsupported,
    }
}

fn weekday_code(s: &str) -> Option<u32> {
    Some(match s.to_ascii_uppercase().as_str() {
        "MO" => 0,
        "TU" => 1,
        "WE" => 2,
        "TH" => 3,
        "FR" => 4,
        "SA" => 5,
        "SU" => 6,
        _ => return None,
    })
}

/// 1 つの VEvent を表示窓 [window_start, window_end)（UTC 秒）へ展開する。
/// EXDATE は除外。RECURRENCE-ID 付き（親の 1 回の上書き）はそのインスタンス 1 件のみ。
pub fn expand(ev: &VEvent, window_start: i64, window_end: i64) -> Vec<Occurrence> {
    let status = if ev.cancelled { "cancelled" } else { "confirmed" };
    let duration = event_duration(ev);
    let all_day = ev.start.is_all_day();
    let make = |start: IcsTime, unsupported: bool| -> Occurrence {
        let start_ts = start.to_utc_ts();
        Occurrence {
            uid: ev.uid.clone(),
            recurrence_id: ev.recurrence_id.map(|t| t.to_utc_ts().to_string()),
            summary: ev.summary.clone(),
            start_ts,
            end_ts: duration.map(|d| start_ts + d),
            all_day,
            status: status.to_string(),
            unsupported,
        }
    };

    // 単発（RRULE なし。RECURRENCE-ID 上書きもこの経路で 1 件）
    let Some(rrule_str) = &ev.rrule else {
        let ts = ev.start.to_utc_ts();
        if in_window(ts, window_start, window_end) {
            return vec![make(ev.start, false)];
        }
        return vec![];
    };

    let rrule = parse_rrule(rrule_str);
    if rrule.unsupported {
        // 対応できない RRULE: 当日分（DTSTART が窓内なら）だけ unsupported 印で残す（§2.3）
        let ts = ev.start.to_utc_ts();
        if in_window(ts, window_start, window_end) {
            return vec![make(ev.start, true)];
        }
        return vec![];
    }

    let exdates: HashSet<i64> = ev.exdates.iter().map(|t| t.to_utc_ts()).collect();
    let mut out = Vec::new();
    let base_date = ev.start.date();
    let mut cursor = base_date;
    let mut emitted = 0i64;
    // near-term の安全上限（無限ループ防止）: 窓は最大でも通知窓込み ~数十日
    let mut guard = 0;
    let max_iter = 4000;
    while guard < max_iter {
        guard += 1;
        // COUNT / UNTIL の停止判定は「発生し得た候補」基準
        let dates = occurrence_dates(&rrule, base_date, cursor);
        let mut advanced = false;
        for d in dates {
            let occ = ev.start.with_date(d);
            let ts = occ.to_utc_ts();
            if let Some(until) = rrule.until_ts {
                if ts > until {
                    return out;
                }
            }
            if let Some(cnt) = rrule.count {
                if emitted >= cnt {
                    return out;
                }
            }
            emitted += 1;
            advanced = true;
            if ts >= window_end {
                // これ以降は窓の外（日付は単調増加）→ 打ち切り
                return out;
            }
            if in_window(ts, window_start, window_end) && !exdates.contains(&ts) {
                out.push(make(occ, false));
            }
        }
        // 次の周期へ
        cursor = advance_cursor(&rrule, cursor);
        if !advanced && cursor > date_from_ts(window_end) + Duration::days(1) {
            break;
        }
    }
    out
}

/// この cursor 周期で発生する日付群（WEEKLY は BYDAY 分、それ以外は 1 日）。
fn occurrence_dates(rrule: &Rrule, base: NaiveDate, cursor: NaiveDate) -> Vec<NaiveDate> {
    match rrule.freq {
        Freq::Weekly => {
            let days = if rrule.byday.is_empty() {
                vec![base.weekday().num_days_from_monday()]
            } else {
                rrule.byday.clone()
            };
            // cursor はその週の月曜。各曜日の日付を出す
            let monday = cursor - Duration::days(cursor.weekday().num_days_from_monday() as i64);
            let mut v: Vec<NaiveDate> = days
                .iter()
                .map(|w| monday + Duration::days(*w as i64))
                .filter(|d| *d >= base)
                .collect();
            v.sort();
            v
        }
        _ => vec![cursor],
    }
}

fn advance_cursor(rrule: &Rrule, cursor: NaiveDate) -> NaiveDate {
    match rrule.freq {
        Freq::Daily => cursor + Duration::days(rrule.interval),
        Freq::Weekly => {
            let monday = cursor - Duration::days(cursor.weekday().num_days_from_monday() as i64);
            monday + Duration::weeks(rrule.interval)
        }
        Freq::Monthly => add_months(cursor, rrule.interval),
        // 2/29 起点で翌年に同日が無い場合は月末 (2/28) へ丸める
        // (unwrap_or(cursor) だと cursor が進まず以後展開されない、M10 reviewer 指摘)
        Freq::Yearly => add_months(cursor, rrule.interval * 12),
    }
}

fn add_months(d: NaiveDate, months: i64) -> NaiveDate {
    let total = (d.year() as i64) * 12 + (d.month0() as i64) + months;
    let year = (total.div_euclid(12)) as i32;
    let month0 = total.rem_euclid(12) as u32;
    // 月末調整（存在しない日は月末へ）
    let mut day = d.day();
    loop {
        if let Some(nd) = NaiveDate::from_ymd_opt(year, month0 + 1, day) {
            return nd;
        }
        if day <= 1 {
            return NaiveDate::from_ymd_opt(year, month0 + 1, 1).unwrap_or(d);
        }
        day -= 1;
    }
}

fn event_duration(ev: &VEvent) -> Option<i64> {
    match &ev.end {
        Some(end) => Some(end.to_utc_ts() - ev.start.to_utc_ts()),
        None => None,
    }
}

fn in_window(ts: i64, start: i64, end: i64) -> bool {
    ts >= start && ts < end
}

fn date_from_ts(ts: i64) -> NaiveDate {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.naive_utc().date())
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
}

/// ICS 本文らしさの最小検証 (BEGIN:VCALENDAR を含むか)。
/// URL ソースがエラーページ HTML を返したときに「予定 0 件の正常応答」と
/// 誤認して既存キャッシュを消さないためのガード。
fn looks_like_ics(raw: &str) -> bool {
    raw.contains("BEGIN:VCALENDAR")
}

/// UID ごとの RECURRENCE-ID (上書きされた個別回の元時刻) を集める。
/// 親系列 (RRULE 持ち) の展開時に EXDATE と同様に除外する。
fn collect_recurrence_overrides(
    events: &[VEvent],
) -> std::collections::HashMap<String, Vec<IcsTime>> {
    let mut map: std::collections::HashMap<String, Vec<IcsTime>> = std::collections::HashMap::new();
    for ev in events {
        if let Some(rid) = ev.recurrence_id {
            map.entry(ev.uid.clone()).or_default().push(rid);
        }
    }
    map
}

// ===== 取得 + DB 反映 =====

/// 1 ソースを取得して calendar_cache へ反映する（watcher / refresh_calendar が呼ぶ）。
/// File は読込、Url は HTTP GET（15 秒タイムアウト、topics.rs と同水準）。
/// **取得失敗・非 ICS 応答は Err で早期 return し既存キャッシュを維持する**
/// （spec §4.6.4 オフライン時キャッシュ表示。エラーページ HTML 等で
/// delete_stale が走りキャッシュ全消去、を防ぐ — M10 reviewer 指摘）。
/// window は今日 0:00（ローカル）〜 now + display_days。
pub async fn fetch_source_into_cache(
    state: &Arc<AppState>,
    source_id: i64,
    source: &CalendarSource,
    display_days: i64,
) -> Result<usize> {
    let raw = match source {
        CalendarSource::File { path } => {
            std::fs::read_to_string(path).with_context(|| format!("ICS 読込失敗: {path}"))?
        }
        CalendarSource::Url { url } => {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .context("HTTP クライアント構築失敗")?;
            let resp = client
                .get(url)
                .send()
                .await
                .with_context(|| format!("ICS 取得失敗: {url}"))?
                .error_for_status()
                .with_context(|| format!("ICS 取得が HTTP エラー: {url}"))?;
            resp.text().await.context("ICS 本文の取得失敗")?
        }
    };
    if !looks_like_ics(&raw) {
        anyhow::bail!("ICS ではない応答 (BEGIN:VCALENDAR なし)。既存キャッシュを維持します");
    }

    let now = chrono::Utc::now().timestamp();
    let now_local = chrono::Local::now().naive_local();
    let window_start = local_to_utc_ts(now_local.date().and_hms_opt(0, 0, 0).expect("midnight"));
    let window_end = now + display_days * 86_400;

    let events = parse_ics(&raw);
    // RECURRENCE-ID による個別回の上書き (移動・取消) を親系列の展開から除外する。
    // 除外しないと「移動」で旧時刻の行が二重に残り、「取消」は ICS 内の並び順に
    // 依存してしか効かない (M10 reviewer 指摘)。
    let overrides = collect_recurrence_overrides(&events);
    let mut count = 0;
    for ev in &events {
        let expanded = if ev.rrule.is_some() && ev.recurrence_id.is_none() {
            let mut parent = ev.clone();
            if let Some(ex) = overrides.get(&ev.uid) {
                parent.exdates.extend(ex.iter().copied());
            }
            expand(&parent, window_start, window_end)
        } else {
            expand(ev, window_start, window_end)
        };
        for occ in expanded {
            state
                .db
                .upsert_calendar_event(
                    source_id,
                    &occ.uid,
                    occ.recurrence_id.as_deref(),
                    &occ.summary,
                    occ.start_ts,
                    occ.end_ts,
                    occ.all_day,
                    &occ.status,
                    &occ.notify_key(),
                    occ.unsupported,
                    now,
                )
                .with_context(|| format!("upsert 失敗: {}", occ.summary))?;
            count += 1;
        }
    }
    // 今回の取得で消えた発生行を削除（notified は生存行で保持）
    state.db.delete_stale_calendar(source_id, now)?;
    Ok(count)
}

/// 終日予定の通知基準時刻（その日のローカル 8:00 の UTC 秒）。
pub fn all_day_notify_ts(start_ts: i64) -> i64 {
    let date = date_from_ts_local(start_ts);
    local_to_utc_ts(
        date.and_hms_opt(
            (ALL_DAY_NOTIFY_TOD_SECS / 3600) as u32,
            0,
            0,
        )
        .expect("8:00"),
    )
}

fn date_from_ts_local(ts: i64) -> NaiveDate {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).date_naive())
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
}

/// 時刻付き予定の HH:MM ラベル（辞書 {time} 用、ローカル）。
pub fn time_label(start_ts: i64, all_day: bool) -> String {
    if all_day {
        return "今日".to_string();
    }
    let dt = chrono::DateTime::from_timestamp(start_ts, 0)
        .map(|d| d.with_timezone(&chrono::Local))
        .unwrap_or_else(chrono::Local::now);
    format!("{}:{:02}", dt.hour(), dt.minute())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_ts(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        local_to_utc_ts(
            NaiveDate::from_ymd_opt(y, mo, d)
                .unwrap()
                .and_hms_opt(h, mi, 0)
                .unwrap(),
        )
    }

    #[test]
    fn unfold_joins_continuation_lines() {
        // RFC5545: 折り畳みは CRLF + 空白 1 文字。unfold は先頭 1 文字のみ剥がす。
        let raw = "SUMMARY:long\r\n wrapped\r\nUID:x";
        let lines = unfold(raw);
        assert_eq!(lines[0], "SUMMARY:longwrapped");
        assert_eq!(lines[1], "UID:x");
    }

    #[test]
    fn parse_single_timed_event() {
        let ics = "BEGIN:VEVENT\nUID:a\nSUMMARY:会議\nDTSTART:20260720T090000Z\nDTEND:20260720T100000Z\nEND:VEVENT";
        let evs = parse_ics(ics);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].summary, "会議");
        assert_eq!(evs[0].start, IcsTime::Utc(NaiveDate::from_ymd_opt(2026, 7, 20).unwrap().and_hms_opt(9, 0, 0).unwrap()));
        // 展開: 窓内なら 1 件、UTC はそのまま
        let occ = expand(&evs[0], 0, i64::MAX / 2);
        assert_eq!(occ.len(), 1);
        assert!(!occ[0].all_day);
        assert_eq!(occ[0].end_ts.unwrap() - occ[0].start_ts, 3600);
    }

    #[test]
    fn all_day_uses_local_midnight() {
        let ics = "BEGIN:VEVENT\nUID:b\nSUMMARY:祝日\nDTSTART;VALUE=DATE:20260721\nEND:VEVENT";
        let evs = parse_ics(ics);
        assert!(evs[0].start.is_all_day());
        let occ = expand(&evs[0], 0, i64::MAX / 2);
        assert!(occ[0].all_day);
        assert_eq!(occ[0].start_ts, local_ts(2026, 7, 21, 0, 0));
    }

    #[test]
    fn weekly_byday_expands_within_window() {
        // 毎週 月・水、2026-07-20(月) 開始。窓 = 7/20..7/27
        let ics = "BEGIN:VEVENT\nUID:w\nSUMMARY:ジム\nDTSTART:20260720T190000\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE\nEND:VEVENT";
        let evs = parse_ics(ics);
        let ws = local_ts(2026, 7, 20, 0, 0);
        let we = local_ts(2026, 7, 27, 0, 0);
        let occ = expand(&evs[0], ws, we);
        // 7/20(月),7/22(水) の 2 件（7/27 は窓外）
        assert_eq!(occ.len(), 2);
        assert_eq!(occ[0].start_ts, local_ts(2026, 7, 20, 19, 0));
        assert_eq!(occ[1].start_ts, local_ts(2026, 7, 22, 19, 0));
        assert!(occ.iter().all(|o| !o.unsupported));
    }

    #[test]
    fn daily_interval_count_and_exdate() {
        // 毎 2 日・COUNT=3、7/20 開始、7/22 を EXDATE で除外
        let ics = "BEGIN:VEVENT\nUID:d\nSUMMARY:薬\nDTSTART:20260720T080000\nRRULE:FREQ=DAILY;INTERVAL=2;COUNT=3\nEXDATE:20260722T080000\nEND:VEVENT";
        let evs = parse_ics(ics);
        let ws = local_ts(2026, 7, 1, 0, 0);
        let we = local_ts(2026, 8, 1, 0, 0);
        let occ = expand(&evs[0], ws, we);
        // 候補 7/20, 7/22(除外), 7/24 → 2 件
        assert_eq!(occ.len(), 2);
        assert_eq!(occ[0].start_ts, local_ts(2026, 7, 20, 8, 0));
        assert_eq!(occ[1].start_ts, local_ts(2026, 7, 24, 8, 0));
    }

    #[test]
    fn until_stops_expansion() {
        let ics = "BEGIN:VEVENT\nUID:u\nSUMMARY:朝会\nDTSTART:20260720T090000\nRRULE:FREQ=DAILY;UNTIL=20260722T235900Z\nEND:VEVENT";
        let evs = parse_ics(ics);
        let ws = local_ts(2026, 7, 1, 0, 0);
        let we = local_ts(2026, 8, 1, 0, 0);
        let occ = expand(&evs[0], ws, we);
        // 7/20,7/21,7/22（UNTIL 当日まで）
        assert_eq!(occ.len(), 3);
    }

    #[test]
    fn unsupported_rrule_keeps_first_only() {
        // near-term 非対応の FREQ（HOURLY）→ DTSTART の 1 件を unsupported 印で
        let ics = "BEGIN:VEVENT\nUID:h\nSUMMARY:謎\nDTSTART:20260720T090000\nRRULE:FREQ=HOURLY;INTERVAL=3\nEND:VEVENT";
        let evs = parse_ics(ics);
        let ws = local_ts(2026, 7, 1, 0, 0);
        let we = local_ts(2026, 8, 1, 0, 0);
        let occ = expand(&evs[0], ws, we);
        assert_eq!(occ.len(), 1);
        assert!(occ[0].unsupported);
    }

    #[test]
    fn cancelled_status_marks_occurrence() {
        let ics = "BEGIN:VEVENT\nUID:c\nSUMMARY:中止\nDTSTART:20260720T090000\nSTATUS:CANCELLED\nEND:VEVENT";
        let evs = parse_ics(ics);
        let ws = local_ts(2026, 7, 1, 0, 0);
        let we = local_ts(2026, 8, 1, 0, 0);
        let occ = expand(&evs[0], ws, we);
        assert_eq!(occ[0].status, "cancelled");
    }

    #[test]
    fn notify_key_changes_with_summary() {
        let base = Occurrence {
            uid: "x".into(), recurrence_id: None, summary: "A".into(),
            start_ts: 1000, end_ts: Some(1100), all_day: false,
            status: "confirmed".into(), unsupported: false,
        };
        let mut changed = base.clone();
        changed.summary = "B".into();
        assert_ne!(base.notify_key(), changed.notify_key());
        assert_eq!(base.notify_key(), base.notify_key());
    }

    #[test]
    fn escaped_summary_unescaped() {
        let ics = "BEGIN:VEVENT\nUID:e\nSUMMARY:A\\, B\\nC\nDTSTART:20260720T090000\nEND:VEVENT";
        let evs = parse_ics(ics);
        assert_eq!(evs[0].summary, "A, B\nC");
    }

    #[test]
    fn recurrence_override_suppresses_parent_occurrence() {
        // 毎日 9:00 の親系列 + 7/21 の回を 15:00 へ移動する上書き VEVENT
        // (上書きが親より「前」に並ぶ順序でも効くこと = 順序非依存)
        let ics = "BEGIN:VEVENT\nUID:r\nSUMMARY:朝会(移動)\nDTSTART:20260721T150000\nRECURRENCE-ID:20260721T090000\nEND:VEVENT\nBEGIN:VEVENT\nUID:r\nSUMMARY:朝会\nDTSTART:20260720T090000\nRRULE:FREQ=DAILY;COUNT=3\nEND:VEVENT";
        let events = parse_ics(ics);
        let overrides = collect_recurrence_overrides(&events);
        assert_eq!(overrides.get("r").map(|v| v.len()), Some(1));
        // 親展開に上書き回の元時刻を除外として合流させると 7/21 9:00 が消える
        let parent = events.iter().find(|e| e.rrule.is_some()).unwrap();
        let mut merged = parent.clone();
        merged.exdates.extend(overrides["r"].iter().copied());
        let ws = local_ts(2026, 7, 1, 0, 0);
        let we = local_ts(2026, 8, 1, 0, 0);
        let occ = expand(&merged, ws, we);
        let starts: Vec<i64> = occ.iter().map(|o| o.start_ts).collect();
        assert!(starts.contains(&local_ts(2026, 7, 20, 9, 0)));
        assert!(!starts.contains(&local_ts(2026, 7, 21, 9, 0)), "上書き回の元時刻は親から除外");
        assert!(starts.contains(&local_ts(2026, 7, 22, 9, 0)));
        // 上書き VEVENT 自身は単発として新時刻で 1 件
        let ov = events.iter().find(|e| e.recurrence_id.is_some()).unwrap();
        let occ2 = expand(ov, ws, we);
        assert_eq!(occ2.len(), 1);
        assert_eq!(occ2[0].start_ts, local_ts(2026, 7, 21, 15, 0));
    }

    #[test]
    fn yearly_from_leap_day_advances() {
        // 2/29 起点の YEARLY: 翌年は 2/28 に丸めて前進する (据置で止まらない)
        let d = NaiveDate::from_ymd_opt(2028, 2, 29).unwrap();
        let rr = parse_rrule("FREQ=YEARLY");
        let next = advance_cursor(&rr, d);
        assert_eq!(next, NaiveDate::from_ymd_opt(2029, 2, 28).unwrap());
    }

    #[test]
    fn non_ics_body_is_rejected() {
        assert!(!looks_like_ics("<html><body>404 Not Found</body></html>"));
        assert!(looks_like_ics("BEGIN:VCALENDAR\nBEGIN:VEVENT\nEND:VEVENT\nEND:VCALENDAR"));
    }
}

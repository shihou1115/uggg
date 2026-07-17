//! 統合リマインダー (M5-B → M7 拡張、spec §4.6.1 / daily-support-design §7.1)。
//!
//! - `parse_reminder`: 発話から予定を抽出する決定論パーサ (low、LLM 不使用)。
//!   相対 (「N 分後」)・絶対時刻 (「18時に」)・相対日 (「明日の朝」)・日付 (「7月20日」)・
//!   繰り返し (「毎日 9 時」「毎週月曜」「月・水・金の9時」) に対応する。
//! - 時刻・TZ 契約 (§2.5): DB は UTC 秒、解釈・繰り返し計算はローカル TZ。
//!   ローカル→UTC 変換は本モジュールの `local_to_utc_ts` に集約する
//!   (DST の存在しない時刻は +1h 繰り上げ、重複する時刻は先 = 早い方)。
//! - 発火は `tasks::spawn_reminder_watcher` が `due_active_reminders` をポーリングし、
//!   `system::deliver::deliver_event` (辞書 events.reminder_fired) で配達する。
//!
//! 語彙・書式の確定表はテスト (`mod tests`) を正とする (§11.1-1 実装 PR 確定事項)。
//! 時間帯マッピング既定: 朝=8:00 / 昼=12:00 / 夕方=17:00 / 夜・晩=20:00。

use std::sync::Arc;

use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDateTime, NaiveTime, TimeZone};

use crate::db::{ReminderFilter, ReminderKind, ReminderRow};
use crate::state::AppState;

/// 時間帯マッピング既定 (秒)。終日予定の通知時刻 (§2.5) とも共有する想定。
pub const TOD_MORNING: i32 = 8 * 3600;
pub const TOD_NOON: i32 = 12 * 3600;
pub const TOD_EVENING: i32 = 17 * 3600;
pub const TOD_NIGHT: i32 = 20 * 3600;
/// 繰り返し・相対日で時刻が省略されたときの既定 (朝 8:00)。
pub const TOD_DEFAULT: i32 = TOD_MORNING;

/// 抽出した予定の種類 (daily-support-design §7.1)。
/// `AtTime` はローカル壁時計のまま持ち、UTC 化は登録時に行う (§2.5 の変換一元化)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schedule {
    /// 「N 分後」等の相対指定。
    Offset { secs: i64 },
    /// 絶対時刻・日付 (今日/明日の HH:MM、M月D日 等)。
    AtTime { local: NaiveDateTime },
    /// 毎日 HH:MM。time_of_day はローカル 0:00 からの秒。
    Daily { time_of_day: i32 },
    /// 毎週。weekday_mask は bit0=月 .. bit6=日。
    Weekly { weekday_mask: u8, time_of_day: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedReminder {
    pub schedule: Schedule,
    /// リマインダー本文。空文字なら呼び出し側で扱いを決める。
    pub body: String,
}

// ===== ローカル TZ ⇔ UTC (§2.5) =====

/// ローカル壁時計 → UTC 秒。存在しない時刻 (DST gap) は +1h 繰り上げ、
/// 重複する時刻 (DST 終了) は先 (早い方) を採用する。
pub fn local_to_utc_ts(local: NaiveDateTime) -> i64 {
    use chrono::LocalResult;
    match chrono::Local.from_local_datetime(&local) {
        LocalResult::Single(t) => t.timestamp(),
        LocalResult::Ambiguous(first, _second) => first.timestamp(),
        LocalResult::None => match chrono::Local.from_local_datetime(&(local + Duration::hours(1))) {
            LocalResult::Single(t) => t.timestamp(),
            LocalResult::Ambiguous(first, _) => first.timestamp(),
            // +1h しても解決しない TZ は現実には無い。保険として UTC 扱い。
            LocalResult::None => local.and_utc().timestamp(),
        },
    }
}

fn tod_to_time(time_of_day: i32) -> NaiveTime {
    let s = time_of_day.clamp(0, 24 * 3600 - 1) as u32;
    NaiveTime::from_num_seconds_from_midnight_opt(s, 0)
        .unwrap_or_else(|| NaiveTime::from_hms_opt(0, 0, 0).expect("midnight"))
}

/// 繰り返しの次回発火時刻 (ローカル)。`after` より厳密に未来の最初の候補を返す。
/// once や mask=0 の weekly は None (呼び出し側で再発火停止すること)。
pub fn next_recurring_local(
    kind: ReminderKind,
    weekday_mask: u8,
    time_of_day: i32,
    after: NaiveDateTime,
) -> Option<NaiveDateTime> {
    let t = tod_to_time(time_of_day);
    match kind {
        ReminderKind::Once => None,
        ReminderKind::Daily => {
            let cand = after.date().and_time(t);
            Some(if cand > after { cand } else { (after.date() + Duration::days(1)).and_time(t) })
        }
        ReminderKind::Weekly => {
            if weekday_mask == 0 {
                return None;
            }
            for d in 0..=7 {
                let date = after.date() + Duration::days(d);
                let bit = 1u8 << date.weekday().num_days_from_monday();
                if weekday_mask & bit != 0 {
                    let cand = date.and_time(t);
                    if cand > after {
                        return Some(cand);
                    }
                }
            }
            None
        }
    }
}

/// 繰り返しの次回発火時刻 (UTC 秒)。watcher の reschedule 用。
pub fn next_recurring_due_ts(
    kind: ReminderKind,
    weekday_mask: u8,
    time_of_day: i32,
    after_local: NaiveDateTime,
) -> Option<i64> {
    next_recurring_local(kind, weekday_mask, time_of_day, after_local).map(local_to_utc_ts)
}

/// 初回 due (UTC 秒)。Offset は now_utc 起点、他はローカル→UTC 変換。
pub fn first_due_ts(schedule: &Schedule, now_utc: i64, now_local: NaiveDateTime) -> Option<i64> {
    match schedule {
        Schedule::Offset { secs } => Some(now_utc.saturating_add(*secs)),
        Schedule::AtTime { local } => Some(local_to_utc_ts(*local)),
        Schedule::Daily { time_of_day } => {
            next_recurring_due_ts(ReminderKind::Daily, 0, *time_of_day, now_local)
        }
        Schedule::Weekly { weekday_mask, time_of_day } => {
            next_recurring_due_ts(ReminderKind::Weekly, *weekday_mask, *time_of_day, now_local)
        }
    }
}

/// weekday_mask (bit0=月..bit6=日) を「月・水・金」形式に整形する。確認文・UI 用。
pub fn weekday_mask_names(mask: u8) -> String {
    const NAMES: [&str; 7] = ["月", "火", "水", "木", "金", "土", "日"];
    let mut out = String::new();
    for (i, name) in NAMES.iter().enumerate() {
        if mask & (1 << i) != 0 {
            if !out.is_empty() {
                out.push('・');
            }
            out.push_str(name);
        }
    }
    out
}

// ===== パーサ =====

/// 発話から予定を抽出する。マッチしなければ None。
/// `now` は現在のローカル壁時計 (`chrono::Local::now().naive_local()`)。
/// マッチ優先順: 相対 (N分後) → 繰り返し (毎〜) → 曜日列挙 → 日付 → 相対日 → 単独曜日 → 時刻のみ。
pub fn parse_reminder(text: &str, now: NaiveDateTime) -> Option<ParsedReminder> {
    // 疑問文は登録依頼ではなく雑談・質問 (「明日の予定は？」等)。パーサ全体で拒否する。
    let trimmed = text.trim_end();
    if trimmed.ends_with('?') || trimmed.ends_with('？') {
        return None;
    }
    let norm: Vec<char> = normalize(text);
    // weak = 時刻が明示されない (8:00 補完) or 時刻のみ、の緩いマッチ。
    // 本文が雑談語尾 (chatter_body) ならパース全体を打ち切る (後続の
    // さらに弱いマッチャーに落とすと別解釈で誤登録するため)。
    let finish = |(schedule, start, end, weak): Hit| {
        let body = extract_body(&norm, start, end);
        if weak && chatter_body(&body) {
            return None;
        }
        Some(ParsedReminder { schedule, body })
    };
    for m in [match_offset, match_recurring, match_weekday_enum, match_date] {
        if let Some(hit) = m(&norm, now) {
            return finish(hit);
        }
    }
    // 相対日は三値: 「今日+過去時刻」は後続マッチャー (時刻のみ → 翌日繰り上げ) に
    // 落とすと「明日の 9 時」に化けてしまうため、パース全体を打ち切る。
    match match_relative_day(&norm, now) {
        RelDayResult::Hit(hit) => return finish(hit),
        RelDayResult::PastToday => return None,
        RelDayResult::Miss => {}
    }
    for m in [match_bare_weekday, match_time_only] {
        if let Some(hit) = m(&norm, now) {
            return finish(hit);
        }
    }
    None
}

/// マッチ結果: (予定, 開始文字位置, 終了文字位置, weak)。
/// weak = 時刻の明示がない緩いマッチ (chatter_body の否定リストを適用する)。
type Hit = (Schedule, usize, usize, bool);

/// 全角→半角の正規化 (数字・コロン・スラッシュ・スペース)。
fn normalize(text: &str) -> Vec<char> {
    text.chars()
        .map(|c| match c {
            '０'..='９' => char::from_u32('0' as u32 + (c as u32 - '０' as u32)).unwrap_or(c),
            '：' => ':',
            '／' => '/',
            '　' => ' ',
            _ => c,
        })
        .collect()
}

fn starts_with(c: &[char], i: usize, pat: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    c.len() >= i + p.len() && c[i..i + p.len()] == p[..]
}

/// i 位置から連続する数字 (最大 4 桁) を読む。(値, 消費文字数)。
fn digits_at(c: &[char], i: usize) -> Option<(u32, usize)> {
    let mut n: u32 = 0;
    let mut len = 0;
    while i + len < c.len() && len < 4 {
        let ch = c[i + len];
        if let Some(d) = ch.to_digit(10) {
            n = n * 10 + d;
            len += 1;
        } else {
            break;
        }
    }
    if len == 0 {
        None
    } else {
        Some((n, len))
    }
}

fn prev_is_digit(c: &[char], i: usize) -> bool {
    i > 0 && c[i - 1].is_ascii_digit()
}

/// 「(午前|午後)?H時(半|M分)?」「H:MM」を i 位置から読む。(0:00 からの秒, 消費文字数)。
fn parse_time_at(c: &[char], i: usize) -> Option<(i32, usize)> {
    let mut j = i;
    let mut pm = false;
    if starts_with(c, j, "午後") {
        pm = true;
        j += 2;
    } else if starts_with(c, j, "午前") {
        j += 2;
    }
    if prev_is_digit(c, j) {
        return None;
    }
    let (h_raw, hl) = digits_at(c, j)?;
    j += hl;
    let minutes: u32;
    if j < c.len() && c[j] == ':' {
        let (m, ml) = digits_at(c, j + 1)?;
        if ml == 0 || m > 59 {
            return None;
        }
        j += 1 + ml;
        minutes = m;
    } else if j < c.len() && c[j] == '時' {
        // 「N時間」は時刻ではなく期間 (相対指定は match_offset の担当)
        if starts_with(c, j + 1, "間") {
            return None;
        }
        j += 1;
        if starts_with(c, j, "半") {
            minutes = 30;
            j += 1;
        } else if let Some((m, ml)) = digits_at(c, j) {
            if starts_with(c, j + ml, "分") && m <= 59 {
                minutes = m;
                j += ml + 1;
            } else {
                minutes = 0;
            }
        } else {
            minutes = 0;
        }
    } else {
        return None;
    }
    let mut hour = h_raw;
    if pm && hour < 12 {
        hour += 12;
    }
    if hour > 23 {
        return None;
    }
    Some(((hour * 3600 + minutes * 60) as i32, j - i))
}

/// 時間帯語 (朝/昼/夕方/夜/晩) を i 位置から読む。
fn parse_timeband_at(c: &[char], i: usize) -> Option<(i32, usize)> {
    for (word, tod) in [
        ("夕方", TOD_EVENING),
        ("朝", TOD_MORNING),
        ("昼", TOD_NOON),
        ("夜", TOD_NIGHT),
        ("晩", TOD_NIGHT),
    ] {
        if starts_with(c, i, word) {
            return Some((tod, word.chars().count()));
        }
    }
    None
}

/// 時刻 or 時間帯。任意の「の」を前に許す (「明日の朝」「毎週月曜の9時」)。
fn parse_time_or_band_at(c: &[char], i: usize) -> Option<(i32, usize)> {
    let skip = if starts_with(c, i, "の") { 1 } else { 0 };
    if let Some((tod, len)) = parse_time_at(c, i + skip) {
        return Some((tod, skip + len));
    }
    parse_timeband_at(c, i + skip).map(|(tod, len)| (tod, skip + len))
}

/// 曜日 1 文字 (+任意の「曜」「曜日」)。(bit index 0=月..6=日, 消費文字数)。
fn parse_weekday_at(c: &[char], i: usize) -> Option<(u8, usize)> {
    const CHARS: [char; 7] = ['月', '火', '水', '木', '金', '土', '日'];
    let ch = *c.get(i)?;
    let idx = CHARS.iter().position(|w| *w == ch)? as u8;
    let mut len = 1;
    if starts_with(c, i + len, "曜") {
        len += 1;
        if starts_with(c, i + len, "日") {
            len += 1;
        }
    }
    Some((idx, len))
}

/// 雑談らしい本文 (直後が「は/が/も」) か。時刻が明示されない緩いマッチの誤登録ガード。
fn looks_like_chatter(c: &[char], after: usize) -> bool {
    matches!(c.get(after), Some('は') | Some('が') | Some('も'))
}

/// 本文の末尾が雑談らしい (平叙文・感想) か。**弱いマッチ** (時刻省略で 8:00 補完した
/// 繰り返し/相対日/日付、および時刻のみマッチ) にだけ適用する否定リスト。
/// 誤登録は Notice として静音を越えて鳴り続けるため、再現率より精度を優先する。
/// 形態素解析は持たないので網羅ではなく、よくある語尾と状態形容詞の狙い撃ち (§11.1-1)。
fn chatter_body(body: &str) -> bool {
    const SUFFIXES: [&str; 33] = [
        // 終助詞・推量・接続止め
        "ね", "なあ", "なぁ", "よ", "わ", "かな", "かも", "っけ", "でしょ", "だろう", "だろ",
        "けど", "のに", "です", "ます",
        // 過去・進行 (報告・感想)
        "た", "てる", "てた",
        // あいまいな程度表現 (「2/3くらい」等)
        "くらい", "ぐらい", "ごろ", "頃",
        // 状態形容詞 (「毎朝眠い」「毎日暑い」等)
        "眠い", "ねむい", "暑い", "寒い", "つらい", "辛い", "しんどい", "だるい", "忙しい",
        "きつい", "痛い",
    ];
    let b = body.trim_end();
    SUFFIXES.iter().any(|s| b.ends_with(s))
}

// --- 各マッチャー: Some((Schedule, 開始文字位置, 終了文字位置)) ---

/// 「N分後 / N時間後 / N秒後」(M5-B 互換)。
fn match_offset(c: &[char], _now: NaiveDateTime) -> Option<Hit> {
    for i in 0..c.len() {
        if prev_is_digit(c, i) {
            continue;
        }
        let Some((n, dl)) = digits_at(c, i) else { continue };
        if n == 0 {
            continue;
        }
        let j = i + dl;
        let (unit_secs, ul) = if starts_with(c, j, "分後") {
            (60i64, 2)
        } else if starts_with(c, j, "時間後") {
            (3600i64, 3)
        } else if starts_with(c, j, "秒後") {
            (1i64, 2)
        } else {
            continue;
        };
        let secs = (n as i64).saturating_mul(unit_secs);
        return Some((Schedule::Offset { secs }, i, j + ul, false));
    }
    None
}

/// 「毎日/毎朝/毎昼/毎晩/毎夜 (+時刻)」「毎週<曜日列> (+時刻)」。
fn match_recurring(c: &[char], _now: NaiveDateTime) -> Option<Hit> {
    for i in 0..c.len() {
        if c[i] != '毎' {
            continue;
        }
        // 毎朝/毎昼/毎晩/毎夜: 既定時間帯、直後に明示時刻があれば上書き
        for (word, default_tod) in [("毎朝", TOD_MORNING), ("毎昼", TOD_NOON), ("毎晩", TOD_NIGHT), ("毎夜", TOD_NIGHT)] {
            if starts_with(c, i, word) {
                let mut end = i + 2;
                let mut tod = default_tod;
                let mut weak = true; // 時刻の明示がなければ弱いマッチ
                // 「毎朝7時」「毎晩の22時」のような明示時刻は既定を上書きする
                let skip = if starts_with(c, end, "の") { 1 } else { 0 };
                if let Some((t, len)) = parse_time_at(c, end + skip) {
                    tod = t;
                    end += skip + len;
                    weak = false;
                } else if looks_like_chatter(c, end) {
                    continue; // 「毎朝は無理」等の雑談
                }
                return Some((Schedule::Daily { time_of_day: tod }, i, end, weak));
            }
        }
        if starts_with(c, i, "毎日") {
            let mut end = i + 2;
            let mut weak = false;
            let tod = match parse_time_or_band_at(c, end) {
                Some((t, len)) => {
                    end += len;
                    t
                }
                None => {
                    if looks_like_chatter(c, end) {
                        continue; // 「毎日は無理」等の雑談
                    }
                    weak = true;
                    TOD_DEFAULT
                }
            };
            return Some((Schedule::Daily { time_of_day: tod }, i, end, weak));
        }
        if starts_with(c, i, "毎週") {
            let mut j = i + 2;
            let mut mask: u8 = 0;
            loop {
                let Some((bit, len)) = parse_weekday_at(c, j) else { break };
                mask |= 1 << bit;
                j += len;
                // 区切り (・ 、 , と) の直後にまた曜日が来る場合のみ列挙を続ける
                let sep = matches!(c.get(j), Some('・') | Some('、') | Some(',') | Some('と'));
                if sep && parse_weekday_at(c, j + 1).is_some() {
                    j += 1;
                } else {
                    break;
                }
            }
            if mask == 0 {
                continue;
            }
            let mut end = j;
            let mut weak = false;
            let tod = match parse_time_or_band_at(c, end) {
                Some((t, len)) => {
                    end += len;
                    t
                }
                None => {
                    if looks_like_chatter(c, end) {
                        continue;
                    }
                    weak = true;
                    TOD_DEFAULT
                }
            };
            return Some((Schedule::Weekly { weekday_mask: mask, time_of_day: tod }, i, end, weak));
        }
    }
    None
}

/// 「月・水・金の9時」形式 (毎週プレフィックス無しの曜日列挙 = 毎週扱い)。
/// 誤登録防止のため、2 曜日以上 (・区切り) かつ**時刻の明示**を必須にする。
fn match_weekday_enum(c: &[char], _now: NaiveDateTime) -> Option<Hit> {
    for i in 0..c.len() {
        let Some((first, flen)) = parse_weekday_at(c, i) else { continue };
        let mut mask: u8 = 1 << first;
        let mut j = i + flen;
        let mut count = 1;
        while c.get(j) == Some(&'・') {
            let Some((bit, len)) = parse_weekday_at(c, j + 1) else { break };
            mask |= 1 << bit;
            j += 1 + len;
            count += 1;
        }
        if count < 2 {
            continue;
        }
        let (tod, tlen) = parse_time_or_band_at(c, j)?;
        return Some((Schedule::Weekly { weekday_mask: mask, time_of_day: tod }, i, j + tlen, false));
    }
    None
}

/// 「M月D日 (+時刻)」「M/D (+時刻)」。過去日付は翌年に繰り上げ。
fn match_date(c: &[char], now: NaiveDateTime) -> Option<Hit> {
    for i in 0..c.len() {
        if prev_is_digit(c, i) {
            continue;
        }
        let Some((month, ml)) = digits_at(c, i) else { continue };
        if !(1..=12).contains(&month) {
            continue;
        }
        let mut j = i + ml;
        let sep_kanji = c.get(j) == Some(&'月');
        let sep_slash = c.get(j) == Some(&'/');
        if !sep_kanji && !sep_slash {
            continue;
        }
        j += 1;
        let Some((day, dl)) = digits_at(c, j) else { continue };
        if !(1..=31).contains(&day) {
            continue;
        }
        j += dl;
        if sep_kanji {
            if c.get(j) != Some(&'日') {
                continue;
            }
            j += 1;
        }
        let mut end = j;
        let mut weak = false;
        let tod = match parse_time_or_band_at(c, end) {
            Some((t, len)) => {
                end += len;
                t
            }
            None => {
                if looks_like_chatter(c, end) {
                    continue;
                }
                weak = true;
                TOD_DEFAULT
            }
        };
        // 今年→過ぎていれば来年。無効な日付 (2/30 等) はマッチ失敗。
        let build = |year: i32| -> Option<NaiveDateTime> {
            chrono::NaiveDate::from_ymd_opt(year, month, day).map(|d| d.and_time(tod_to_time(tod)))
        };
        let local = match build(now.year()) {
            Some(dt) if dt > now => dt,
            _ => build(now.year() + 1)?,
        };
        return Some((Schedule::AtTime { local }, i, end, weak));
    }
    None
}

/// match_relative_day の結果。PastToday はパース全体の打ち切り (parse_reminder 参照)。
enum RelDayResult {
    Hit(Hit),
    /// 「今日の9時」を過ぎてから言われた等。低パーサでは扱わない (advanced 上乗せの領分)。
    PastToday,
    Miss,
}

/// 「今日/明日/明後日 (+の) (+時刻|時間帯)」。今日は時刻必須かつ未来のみ。
fn match_relative_day(c: &[char], now: NaiveDateTime) -> RelDayResult {
    // 明後日 を 明日 より先に判定する
    const WORDS: [(&str, i64, bool); 7] = [
        ("明後日", 2, false),
        ("あさって", 2, false),
        ("明日", 1, false),
        ("あした", 1, false),
        ("あす", 1, false),
        ("今日", 0, true),
        ("きょう", 0, true),
    ];
    for i in 0..c.len() {
        for (word, days, time_required) in WORDS {
            if !starts_with(c, i, word) {
                continue;
            }
            let wlen = word.chars().count();
            let mut end = i + wlen;
            let tod = match parse_time_or_band_at(c, end) {
                Some((t, len)) => {
                    end += len;
                    Some(t)
                }
                None => None,
            };
            if time_required && tod.is_none() {
                continue;
            }
            if tod.is_none() && looks_like_chatter(c, end) {
                continue; // 「明日は晴れ」等
            }
            let weak = tod.is_none();
            let tod = tod.unwrap_or(TOD_DEFAULT);
            let local = (now.date() + Duration::days(days)).and_time(tod_to_time(tod));
            if local <= now {
                return RelDayResult::PastToday;
            }
            return RelDayResult::Hit((Schedule::AtTime { local }, i, end, weak));
        }
    }
    RelDayResult::Miss
}

/// 「(次の)X曜(日) (+時刻)」単独 = 次の該当曜日 1 回きり。
/// 誤登録防止のため、時刻の明示か直後の「に」を必須にする。
fn match_bare_weekday(c: &[char], now: NaiveDateTime) -> Option<Hit> {
    for i in 0..c.len() {
        let start = i;
        let mut j = i;
        if starts_with(c, i, "次の") {
            j += 2;
        }
        let Some((bit, wlen)) = parse_weekday_at(c, j) else { continue };
        // 「曜」なしの単独 1 文字 (「月」等) は誤爆するため必須
        if wlen < 2 {
            continue;
        }
        let mut end = j + wlen;
        let tod = match parse_time_or_band_at(c, end) {
            Some((t, len)) => {
                end += len;
                Some(t)
            }
            None => None,
        };
        if tod.is_none() && c.get(end) != Some(&'に') {
            continue;
        }
        let weak = tod.is_none();
        let tod = tod.unwrap_or(TOD_DEFAULT);
        let local = next_recurring_local(ReminderKind::Weekly, 1 << bit, tod, now)?;
        return Some((Schedule::AtTime { local }, start, end, weak));
    }
    None
}

/// 「HH時 / HH:MM / 午後3時」のみ。過ぎていれば翌日。
/// 文脈が最も弱いマッチ (雑談中の時刻言及と区別できない) のため常に weak 扱い。
fn match_time_only(c: &[char], now: NaiveDateTime) -> Option<Hit> {
    for i in 0..c.len() {
        if prev_is_digit(c, i) {
            continue;
        }
        let Some((tod, len)) = parse_time_at(c, i) else { continue };
        let today = now.date().and_time(tod_to_time(tod));
        let local = if today > now { today } else { today + Duration::days(1) };
        return Some((Schedule::AtTime { local }, i, i + len, true));
    }
    None
}

/// 時刻表現の後ろを本文として取り出す。先頭の助詞は 1 つだけ食う (M5-B 踏襲)。
/// 後ろが空なら時刻表現の前を使う (「買い物 明日10時」→「買い物」)。
fn extract_body(c: &[char], start: usize, end: usize) -> String {
    let after: String = c[end.min(c.len())..].iter().collect();
    let mut after = after.as_str();
    for prefix in ["には", "に", "で", "の", "、", ",", " "] {
        if let Some(rest) = after.strip_prefix(prefix) {
            after = rest;
            break;
        }
    }
    let after = after.trim();
    if !after.is_empty() {
        return after.to_string();
    }
    let before: String = c[..start].iter().collect();
    let mut before = before.trim_end();
    for suffix in ["には", "に", "で", "を", "は", "、", ","] {
        if let Some(rest) = before.strip_suffix(suffix) {
            before = rest;
            break;
        }
    }
    before.trim().to_string()
}

// ===== DB 窓口 (コマンド・watcher・会話経路から使用) =====

/// Schedule → (kind, weekday_mask, time_of_day) の DB メタ。
pub fn schedule_meta(schedule: &Schedule) -> (ReminderKind, u8, i32) {
    match schedule {
        Schedule::Offset { .. } | Schedule::AtTime { .. } => (ReminderKind::Once, 0, 0),
        Schedule::Daily { time_of_day } => (ReminderKind::Daily, 0, *time_of_day),
        Schedule::Weekly { weekday_mask, time_of_day } => {
            (ReminderKind::Weekly, *weekday_mask, *time_of_day)
        }
    }
}

/// パース済みの予定を DB に登録する共通経路 (会話・パネルの両方から使う)。
/// 本文が空のときは `default_body` を使う。
pub fn register(
    state: &Arc<AppState>,
    parsed: &ParsedReminder,
    default_body: &str,
) -> Result<i64> {
    let body = if parsed.body.is_empty() {
        default_body
    } else {
        parsed.body.as_str()
    };
    let now = chrono::Utc::now().timestamp();
    let now_local = chrono::Local::now().naive_local();
    let due_ts = first_due_ts(&parsed.schedule, now, now_local)
        .ok_or_else(|| anyhow::anyhow!("予定時刻を計算できませんでした"))?;
    let (kind, weekday_mask, time_of_day) = schedule_meta(&parsed.schedule);
    state
        .db
        .insert_reminder_ex(due_ts, body, now, kind, weekday_mask, time_of_day)
}

pub fn add(state: &Arc<AppState>, text: &str, offset_secs: i64) -> Result<i64> {
    let now = chrono::Utc::now().timestamp();
    let due_ts = now.saturating_add(offset_secs);
    state.db.insert_reminder(due_ts, text, now)
}

pub fn list(state: &Arc<AppState>, filter: ReminderFilter) -> Result<Vec<ReminderRow>> {
    state.db.list_reminders(filter)
}

pub fn delete(state: &Arc<AppState>, id: i64) -> Result<()> {
    state.db.delete_reminder(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    /// 2026-07-16 (木) 10:00 を基準時刻とする。
    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, 16)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
    }

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> Schedule {
        Schedule::AtTime {
            local: NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_opt(h, mi, 0).unwrap(),
        }
    }

    fn parse(text: &str) -> ParsedReminder {
        parse_reminder(text, now()).unwrap_or_else(|| panic!("parse失敗: {text}"))
    }

    // --- 相対 (M5-B 互換) ---

    #[test]
    fn offset_minutes_hours_seconds() {
        let r = parse("3分後にお茶を飲む");
        assert_eq!(r.schedule, Schedule::Offset { secs: 180 });
        assert_eq!(r.body, "お茶を飲む");
        assert_eq!(parse("1時間後に休憩").schedule, Schedule::Offset { secs: 3600 });
        assert_eq!(parse("30秒後にお知らせ").schedule, Schedule::Offset { secs: 30 });
        let r = parse("10分後で温まる予定");
        assert_eq!(r.schedule, Schedule::Offset { secs: 600 });
        assert_eq!(r.body, "温まる予定");
    }

    #[test]
    fn offset_fullwidth_digits() {
        assert_eq!(parse("５分後にストレッチ").schedule, Schedule::Offset { secs: 300 });
    }

    #[test]
    fn offset_body_can_be_empty() {
        let r = parse("5分後");
        assert_eq!(r.schedule, Schedule::Offset { secs: 300 });
        assert_eq!(r.body, "");
    }

    // --- 絶対時刻 ---

    #[test]
    fn time_today_or_tomorrow() {
        // now=10:00: 18時は今日、9時は翌日へ
        let r = parse("18時に洗濯物");
        assert_eq!(r.schedule, at(2026, 7, 16, 18, 0));
        assert_eq!(r.body, "洗濯物");
        assert_eq!(parse("9時に薬").schedule, at(2026, 7, 17, 9, 0));
    }

    #[test]
    fn time_colon_half_and_minutes() {
        assert_eq!(parse("18:30にミーティング").schedule, at(2026, 7, 16, 18, 30));
        assert_eq!(parse("18時半に散歩").schedule, at(2026, 7, 16, 18, 30));
        assert_eq!(parse("18時15分に散歩").schedule, at(2026, 7, 16, 18, 15));
        assert_eq!(parse("１８：３０に全角").schedule, at(2026, 7, 16, 18, 30));
    }

    #[test]
    fn time_am_pm() {
        assert_eq!(parse("午後3時に買い出し").schedule, at(2026, 7, 16, 15, 0));
        assert_eq!(parse("午前9時に電話").schedule, at(2026, 7, 17, 9, 0));
    }

    #[test]
    fn body_falls_back_to_prefix() {
        let r = parse("買い物メモ 18時");
        assert_eq!(r.schedule, at(2026, 7, 16, 18, 0));
        assert_eq!(r.body, "買い物メモ");
        let r = parse("薬を21時に");
        assert_eq!(r.body, "薬");
    }

    // --- 相対日 ---

    #[test]
    fn relative_days() {
        let r = parse("明日の朝ゴミ出し");
        assert_eq!(r.schedule, at(2026, 7, 17, 8, 0));
        assert_eq!(r.body, "ゴミ出し");
        // 時刻省略は朝 8:00 既定
        assert_eq!(parse("明日ゴミ出し").schedule, at(2026, 7, 17, 8, 0));
        assert_eq!(parse("あさっての19時に電話").schedule, at(2026, 7, 18, 19, 0));
        assert_eq!(parse("明後日の夜に洗濯").schedule, at(2026, 7, 18, 20, 0));
        assert_eq!(parse("今日の23時に戸締まり").schedule, at(2026, 7, 16, 23, 0));
        assert_eq!(parse("明日の夕方に買い物").schedule, at(2026, 7, 17, 17, 0));
    }

    #[test]
    fn today_requires_future_time() {
        // 今日は時刻必須 + 過去は拒否 (advanced 上乗せの領分)
        assert!(parse_reminder("今日ゴミ出し", now()).is_none());
        assert!(parse_reminder("今日の9時に", now()).is_none());
    }

    #[test]
    fn tomorrow_chatter_is_rejected() {
        assert!(parse_reminder("明日は晴れるかな", now()).is_none());
        assert!(parse_reminder("明日が楽しみ", now()).is_none());
    }

    // --- 日付 ---

    #[test]
    fn month_day_forms() {
        let r = parse("7月20日の10時に病院");
        assert_eq!(r.schedule, at(2026, 7, 20, 10, 0));
        assert_eq!(r.body, "病院");
        assert_eq!(parse("7/20の10時に病院").schedule, at(2026, 7, 20, 10, 0));
        // 時刻省略は朝 8:00
        assert_eq!(parse("7月20日に燃えないゴミ").schedule, at(2026, 7, 20, 8, 0));
        // 過去日付は翌年へ
        assert_eq!(parse("1月2日に帰省").schedule, at(2027, 1, 2, 8, 0));
    }

    #[test]
    fn invalid_dates_rejected() {
        assert!(parse_reminder("13月1日に", now()).is_none());
        assert!(parse_reminder("2月30日に", now()).is_none());
    }

    // --- 繰り返し ---

    #[test]
    fn daily_forms() {
        let r = parse("毎日9時に薬");
        assert_eq!(r.schedule, Schedule::Daily { time_of_day: 9 * 3600 });
        assert_eq!(r.body, "薬");
        assert_eq!(parse("毎朝ストレッチ").schedule, Schedule::Daily { time_of_day: TOD_MORNING });
        assert_eq!(parse("毎晩22時に日記").schedule, Schedule::Daily { time_of_day: 22 * 3600 });
        assert_eq!(parse("毎夜歯磨き").schedule, Schedule::Daily { time_of_day: TOD_NIGHT });
        // 時刻省略の毎日は朝 8:00
        assert_eq!(parse("毎日水やり").schedule, Schedule::Daily { time_of_day: TOD_DEFAULT });
    }

    #[test]
    fn weekly_forms() {
        let r = parse("毎週月曜9時にゴミ出し");
        assert_eq!(r.schedule, Schedule::Weekly { weekday_mask: 0b0000001, time_of_day: 9 * 3600 });
        assert_eq!(r.body, "ゴミ出し");
        // 列挙 (と / ・)
        assert_eq!(
            parse("毎週月・木の9時に燃えるゴミ").schedule,
            Schedule::Weekly { weekday_mask: 0b0001001, time_of_day: 9 * 3600 }
        );
        assert_eq!(
            parse("毎週火曜と金曜の20時にジム").schedule,
            Schedule::Weekly { weekday_mask: 0b0010010, time_of_day: 20 * 3600 }
        );
        // 時刻省略は朝 8:00
        assert_eq!(
            parse("毎週日曜に掃除").schedule,
            Schedule::Weekly { weekday_mask: 0b1000000, time_of_day: TOD_DEFAULT }
        );
    }

    #[test]
    fn weekday_enum_without_maishuu() {
        // 「月・水・金の9時」は毎週扱い。時刻必須。
        let r = parse("月・水・金の9時に薬");
        assert_eq!(r.schedule, Schedule::Weekly { weekday_mask: 0b0010101, time_of_day: 9 * 3600 });
        assert!(parse_reminder("月・水・金は忙しい", now()).is_none());
    }

    #[test]
    fn bare_weekday_is_one_shot() {
        // now = 2026-07-16 (木)。月曜=7/20、金曜=明日 7/17。
        let r = parse("月曜に会議");
        assert_eq!(r.schedule, at(2026, 7, 20, 8, 0));
        assert_eq!(r.body, "会議");
        assert_eq!(parse("金曜の15時に提出").schedule, at(2026, 7, 17, 15, 0));
        // 同じ曜日 (木) で時刻が過ぎていれば来週へ
        assert_eq!(parse("木曜の9時に朝会").schedule, at(2026, 7, 23, 9, 0));
        // 「曜」なしの単独 1 文字や「Xは〜」は拾わない
        assert!(parse_reminder("月がきれい", now()).is_none());
        assert!(parse_reminder("月曜は忙しい", now()).is_none());
    }

    // --- 非マッチ ---

    #[test]
    fn no_match_cases() {
        for text in ["今何時?", "こんにちは", "3 後にお茶", "0分後", "昨日の話", "時間がない"] {
            assert!(parse_reminder(text, now()).is_none(), "誤マッチ: {text}");
        }
    }

    #[test]
    fn chatter_is_not_registered() {
        // 時刻が明示されない弱いマッチ + 雑談語尾 → 登録しない (reviewer 指摘の再発防止)
        for text in [
            "毎朝眠い",
            "毎朝は無理しない",
            "毎日暑いね",
            "毎日がんばってる",
            "毎週月曜は憂鬱",
            "明日は晴れるかな",
            "明日の予定は？",
            "3時に起きた",
            "昨日18時に帰った",
            "2/3くらいできた",
            "月曜には忙しい",
            "12時だね",
        ] {
            assert!(parse_reminder(text, now()).is_none(), "雑談を誤登録: {text}");
        }
        // 時刻が明示された強いマッチは語尾ガードの対象外 (登録意図が明確)
        assert!(parse_reminder("毎朝7時にラジオ体操", now()).is_some());
        assert!(parse_reminder("毎週月曜9時にゴミ出し", now()).is_some());
        // 弱いマッチでも通常の名詞句は通る
        assert!(parse_reminder("毎朝ストレッチ", now()).is_some());
        assert!(parse_reminder("明日ゴミ出し", now()).is_some());
        assert!(parse_reminder("18時に洗濯物", now()).is_some());
    }

    // --- 次回計算 ---

    #[test]
    fn next_recurring_daily() {
        // 10:00 起点: 今日の 11:00 / 翌日の 9:00
        let n = next_recurring_local(ReminderKind::Daily, 0, 11 * 3600, now()).unwrap();
        assert_eq!(n, NaiveDate::from_ymd_opt(2026, 7, 16).unwrap().and_hms_opt(11, 0, 0).unwrap());
        let n = next_recurring_local(ReminderKind::Daily, 0, 9 * 3600, now()).unwrap();
        assert_eq!(n, NaiveDate::from_ymd_opt(2026, 7, 17).unwrap().and_hms_opt(9, 0, 0).unwrap());
        // ちょうど同時刻は翌日 (厳密に未来)
        let n = next_recurring_local(ReminderKind::Daily, 0, 10 * 3600, now()).unwrap();
        assert_eq!(n, NaiveDate::from_ymd_opt(2026, 7, 17).unwrap().and_hms_opt(10, 0, 0).unwrap());
    }

    #[test]
    fn next_recurring_weekly() {
        // 木 10:00 起点、月・木 9:00 → 次は月曜 7/20
        let mask = 0b0001001;
        let n = next_recurring_local(ReminderKind::Weekly, mask, 9 * 3600, now()).unwrap();
        assert_eq!(n, NaiveDate::from_ymd_opt(2026, 7, 20).unwrap().and_hms_opt(9, 0, 0).unwrap());
        // 木 11:00 なら今日
        let n = next_recurring_local(ReminderKind::Weekly, mask, 11 * 3600, now()).unwrap();
        assert_eq!(n, NaiveDate::from_ymd_opt(2026, 7, 16).unwrap().and_hms_opt(11, 0, 0).unwrap());
        // mask=0 と once は None
        assert!(next_recurring_local(ReminderKind::Weekly, 0, 0, now()).is_none());
        assert!(next_recurring_local(ReminderKind::Once, 0, 0, now()).is_none());
    }

    #[test]
    fn weekday_names_format() {
        assert_eq!(weekday_mask_names(0b0010101), "月・水・金");
        assert_eq!(weekday_mask_names(0b1000000), "日");
        assert_eq!(weekday_mask_names(0), "");
    }
}

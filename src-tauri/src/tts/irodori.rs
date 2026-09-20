//! Irodori-TTS HTTP クライアント + サイドカー保持 (architecture §7.4 / §8, M4c Phase D)。
//!
//! Python サイドカー (`%APPDATA%\ugg\irodori\python\python.exe sidecar.py`) と
//! OpenAI 互換 HTTP (`POST /v1/audio/speech` / `POST /v1/voice_ref/generate`) で通信する。
//!
//! 本ファイルは:
//! - `IrodoriClient`: サイドカーのライフサイクル + HTTP クライアントを保持
//! - `ensure_sidecar_running` / `synthesize` / `generate_voice_ref` / `shutdown` を提供
//!
//! ヘルスチェック (10 秒間隔) / アイドル監視 (5 分で自動 kill) は Phase E で `tasks.rs` に
//! 並べる予定。本ファイルは「呼ばれたら起動済を確認 → HTTP 叩く」だけ。
//!
//! `synthesize` は現状 Phase D の sidecar.py モックモードで `mock` 起動した場合は
//! 正弦波 wav を返す。Phase G で `--mock` を外して実 Aratako/Irodori-TTS モデルに結線する。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use thiserror::Error;

use crate::tts::sidecar::{self, SidecarHandle};

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// サイドカー stderr の 1 行を `irodori-download` event へ転送すべきかを判定する pure 関数。
/// `[hf-download]` で始まる行だけ画面へ送る。**それ以外は捨てずに `ugg.log` へ残す**
/// （v0.5.5 項目 1。`route_stderr_line` を参照）。
pub(crate) fn is_hf_progress_line(line: &str) -> bool {
    line.starts_with("[hf-download]")
}

/// サイドカーの 500 body を**ログと告知に載せてよい形**にする (v0.5.5 項目 1、spec §6.0)。
///
/// サイドカーは合成時の例外を `HTTPException(500, f"Irodori 合成失敗: {exc}")` に包んで返す
/// （`sidecar.py`）。`{exc}` は tokenizer 由来などで**発話テキストを含みうる**ので、
/// 送った本文とキャプションを伏せてから 300 文字で頭打ちにする
/// （spec §3.3 の送信物 / v0.5.3 項目 7「診断ログに会話本文を残さない」を破らないため）。
///
/// **合成の例外そのものは stderr には出ない**（`HTTPException` は応答として返るだけ）。
/// stderr が運ぶのは起動時の import 失敗・モデル DL の進捗と失敗・起動時の文字コードの 1 行と、
/// 要求の途中の診断の行（合成の所要時間、参照音声の事前変換の失敗とやり直し。v0.5.6 項目 1）。
/// 事前変換には固定文を渡し、診断の行は型名と数値だけなので、発話本文は入らない。
/// ただしランタイム自身が stderr に何を書くかは分からないので、stderr の行も
/// `sanitize_stderr_line` で伏せる（v0.5.6 項目 2）。
pub(crate) fn sanitize_sidecar_error(body: &str, secrets: &[&str]) -> String {
    let mut out = body.to_string();
    for secret in secrets {
        // 短すぎる文字列で置換すると、無関係な語まで潰れて診断にならない。
        if secret.chars().count() >= 4 {
            out = out.replace(secret, "«伏字»");
            // **500 の本文は JSON**（`{"detail": "..."}`）なので、`"` `\` 改行を含む発話は
            // エスケープされた形で載り、そのままの文字列とは一致しない（2026-09-14 監査で発覚）。
            // starlette は `ensure_ascii=False` で日本語はそのまま、serde_json も同じ規則で書く。
            if let Ok(json) = serde_json::to_string(secret) {
                let escaped = &json[1..json.len() - 1];
                if escaped != *secret {
                    out = out.replace(escaped, "«伏字»");
                }
            }
        }
    }
    crate::dialogue::llm::truncate_for_log(&out)
}

/// 伏せるために覚えておく、直近に送った本文とキャプションの数（v0.5.6 項目 2）。
/// 1 回の合成で本文とキャプションの 2 つを覚えるので、8 回分。
const RECENT_SECRETS: usize = 16;

/// サイドカーの stderr の 1 行を、ログと画面に載せてよい形にする（v0.5.6 項目 2、spec §3.3）。
///
/// stderr の行は要求と対応が付かず、要求が終わったあとにも届く（asyncio の後始末など）ので、
/// 直近に送った本文とキャプションを全部伏せる。**そのままの形では一致しないことがある**ので、
/// 断片にも分けて伏せる（長いものから置き換える）:
/// - **行ごと**: stderr は行ごとに届くので、複数行の本文が丸ごと 1 行に載ることは無い
/// - **Python が書き換える文字の位置で区切った断片**: `repr` は「表示できない文字」を `‍` の
///   ように書くので、全角空白（U+3000）や絵文字の結合（U+200D。溜息の絵文字 `😮‍💨` に入る）を
///   含む本文は、そのままの形でも JSON の形でも一致しない
///
/// **限界**: 照合で伏せるので、ランタイムが正規化した形（NFKC など）や独自のエスケープで書けば
/// 取りこぼす。白名簿にする（決まった語以外を全部伏せる）のは spec §6.0 の「見つけたが今回の裁定に
/// 含めていないもの」に置いてある。いまのランタイムには、本文を stderr に書く経路は見つかっていない
/// （`log_fn=None` で呼び、例外文にも本文は入らない）。これは v0.5.7 の新しいランタイム向けの守り。
pub(crate) fn sanitize_stderr_line(line: &str, recent: &[String]) -> String {
    sanitize_sidecar_error(line, &secret_variants(recent.iter().map(String::as_str)))
}

/// 伏せる対象を、断片も含めて長いものから並べる（500 の本文と stderr の両方で使う）。
fn secret_variants<'a>(secrets: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    for secret in secrets {
        out.push(secret);
        out.extend(secret.lines().map(str::trim));
        out.extend(secret.split(rewritten_by_python).map(str::trim));
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.chars().count()));
    out.dedup();
    out
}

/// Python が `repr` で書き換える文字か（`str.isprintable()` が偽になるもののうち、実際に発話へ
/// 入りうるもの）。ここで本文を区切り、断片も伏せる対象にする。
fn rewritten_by_python(c: char) -> bool {
    c.is_control()
        // 空白のうち、半角スペース以外（全角空白 U+3000 を含む）
        || (c.is_whitespace() && c != ' ')
        // 書式用の文字（U+200D の ZWJ、方向指定、U+FEFF など）
        || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{feff}')
}

/// サイドカーの stderr の 1 行の行き先（pure）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StderrRoute {
    /// モデル取得の進捗。画面（`irodori-download` イベント）へ。
    Progress(String),
    /// それ以外。`ugg.log` へ。
    Log(String),
}

/// 伏せてから行き先を決める。**伏せるのは振り分けより前** — 画面へ流す行も伏せる。
pub(crate) fn route_stderr_line(line: &str, recent: &[String]) -> StderrRoute {
    let line = sanitize_stderr_line(line, recent);
    if is_hf_progress_line(&line) {
        StderrRoute::Progress(line)
    } else {
        StderrRoute::Log(line)
    }
}

/// `shutdown_if_idle` の核ロジック (pure)。port 有 + last_used != 0 + 経過 >= idle_secs で true。
pub(crate) fn should_shutdown_for_idle(
    has_port: bool,
    last_used: i64,
    now: i64,
    idle_secs: i64,
) -> bool {
    if !has_port {
        return false;
    }
    if last_used == 0 {
        return false;
    }
    now.saturating_sub(last_used) >= idle_secs
}

/// Irodori サイドカーへの HTTP クライアント。
pub struct IrodoriClient {
    client: reqwest::Client,
    /// 起動済みサイドカーの port/pid/child。lock を取って ensure → 取り出して使う。
    /// Mutex は std (同期) を使うが、`await` を跨いで保持しないこと (drop してから HTTP 叩く)。
    sidecar: StdMutex<Option<SidecarHandle>>,
    /// 最後に synthesize / voice_ref_generate を呼んだ unix 秒。
    /// アイドル監視 (`tasks::spawn_irodori_idle_watcher`) がこれを見て 5 分未使用なら shutdown する。
    /// 起動直後を「使用中」扱いにするため 0 は「起動なし」を表す sentinel。
    last_used: AtomicI64,
    /// 最後に `notify(IrodoriUnavailable)` を発火した unix 秒 (0 = 一度も発火していない)。
    /// `synthesize_voice` のフォールバック経路と `spawn_irodori_health_watcher` は両方とも
    /// `should_notify_unavailable` で 5 分クールダウンを共有し、無限ループ + 連続発話を防ぐ。
    /// 経緯: notify は `app.emit("dialogue", ...)` でフロントへ流れ、フロントは `synthesize_voice`
    /// を再 invoke する。irodori が落ちている状態で notify を打つと再帰的に同じ failure が
    /// trigger され、フォールバック経路の voicevox 合成が際限なくキューに積まれる。
    last_notified_unavailable: AtomicI64,
    /// この unix 秒までは `ensure_sidecar_running` を skip して即 `SidecarStart` を返す。
    /// 0 = 制限なし。GPU が永続的に取れない環境で health watcher が 3 連続失敗 → shutdown
    /// → 次 synth で再起動 → 90 秒 churn を繰り返すのを防ぐため、health watcher 経路から
    /// 20 分の sticky cooldown を設定する。voicevox fallback は引き続き動く。
    disable_until: AtomicI64,
    /// 直近に送った本文とキャプション。サイドカーの stderr を伏せるのに使う（v0.5.6 項目 2）。
    /// stderr を読むタスクと共有するので `Arc`。
    recent_secrets: Arc<StdMutex<VecDeque<String>>>,
}

impl IrodoriClient {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            sidecar: StdMutex::new(None),
            last_used: AtomicI64::new(0),
            last_notified_unavailable: AtomicI64::new(0),
            disable_until: AtomicI64::new(0),
            recent_secrets: Arc::new(StdMutex::new(VecDeque::new())),
        }
    }

    /// 送る本文とキャプションを覚える（**送る前に**。stderr は応答より先に届きうる）。
    ///
    /// ロックが毒になっていても中身を使う（覚えられないと伏せられない ＝ ログに本文が残る）。
    fn remember_secrets(&self, texts: &[&str]) {
        let mut recent = self
            .recent_secrets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for t in texts {
            // 短いものは伏せない（`sanitize_sidecar_error` と同じ基準）ので覚えない。
            if t.chars().count() >= 4 {
                recent.push_back(t.to_string());
            }
        }
        while recent.len() > RECENT_SECRETS {
            recent.pop_front();
        }
    }

    /// サイドカーの stderr の 1 行を受ける関数（起動のたびに作ってポンプへ渡す）。
    /// 行き先を差し替えられるようにしてあるのは、伏字が実際に通ることをテストで確かめるため。
    fn stderr_sink_to(
        &self,
        mut deliver: impl FnMut(StderrRoute) + Send + 'static,
    ) -> impl FnMut(&str) + Send + 'static {
        let recent = self.recent_secrets.clone();
        move |line: &str| {
            let snapshot: Vec<String> = recent
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .cloned()
                .collect();
            deliver(route_stderr_line(line, &snapshot));
        }
    }

    /// サイドカーの stderr の 1 行を、画面（進捗）と `ugg.log`（それ以外）へ流す。
    fn stderr_sink(&self, app: Option<AppHandle>) -> impl FnMut(&str) + Send + 'static {
        self.stderr_sink_to(move |route| {
            // [hf-download] 接頭辞の行は irodori-download イベントへ転送し、それ以外は ugg.log へ残す。
            match route {
                StderrRoute::Progress(line) => {
                    if let Some(app) = &app {
                        let _ = app.emit("irodori-download", line);
                    }
                }
                // **進捗以外を捨てない** (v0.5.5 項目 1)。捨てていたため、サイドカーが
                // 異常終了してもユーザーに出るのは「HTTP 通信に失敗しました」だけで、
                // 原因に辿り着く手段がアプリ側に 1 つも無かった。
                // 平時に来るのは、起動ごとに文字コードの 1 行（`[stdio]`、v0.5.6 項目 2）と、
                // 合成 1 回ごとに所要時間の 1 行（v0.5.6 項目 1）。uvicorn は `--log-level warning` で
                // 起動しており、合成の例外は `HTTPException` として応答に載るので、それ以外はほぼ来ない。
                // 所要時間の行は 100 バイト程度で、ugg.log（2MB で 1 世代）に約 2 万行入る。
                // 伏せたうえで 300 文字で切り詰め済み（`sanitize_sidecar_error`）。
                StderrRoute::Log(line) => crate::ulog!("[irodori:py] {line}"),
            }
        })
    }

    /// `secs` 秒間、`ensure_sidecar_running` を即エラーで弾く sticky cooldown を設定する。
    /// 既存の disable_until より新しい場合のみ更新 (短い方には縮めない)。
    pub fn disable_for(&self, secs: i64) {
        let until = now_secs().saturating_add(secs);
        let mut current = self.disable_until.load(Ordering::Acquire);
        while until > current {
            match self.disable_until.compare_exchange(
                current,
                until,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    fn currently_disabled(&self) -> bool {
        let until = self.disable_until.load(Ordering::Acquire);
        until > 0 && now_secs() < until
    }

    fn touch_last_used(&self) {
        self.last_used.store(now_secs(), Ordering::Relaxed);
    }

    /// 5 分のクールダウンを介した `notify(IrodoriUnavailable)` のゲート。
    /// 直近 5 分以内に発火していれば false を返して呼び出し側 (synthesize_voice / health_watcher) は
    /// notify を skip する。`compare_exchange` で並行呼び出しでも一度しか true を返さない。
    /// 時計が後ろに巻き戻った (NTP 補正) 場合は last を now に同期して継続。
    pub fn should_notify_unavailable(&self) -> bool {
        self.should_notify_unavailable_at(now_secs())
    }

    /// `should_notify_unavailable` の純粋部分 (テスト用に `now` を外から差し込める)。
    pub fn should_notify_unavailable_at(&self, now: i64) -> bool {
        const COOLDOWN_SECS: i64 = 5 * 60;
        loop {
            let last = self.last_notified_unavailable.load(Ordering::Acquire);
            // last==0 sentinel は「未発火」、cooldown 内なら譲る。
            // 時計が後ろに飛んで last > now になった場合は cooldown 計算が壊れるので強制発火させて last を同期。
            if last != 0 && last <= now && (now - last) < COOLDOWN_SECS {
                return false;
            }
            match self.last_notified_unavailable.compare_exchange(
                last,
                now,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                // 他スレッドが先に更新 → 自分は譲り、次のループで再判定 (たいてい cooldown 内で false に落ちる)
                Err(_) => continue,
            }
        }
    }

    /// 起動済みサイドカーの `/health` を 1 回 ping して true/false を返す (未起動なら true 扱い = no-op)。
    /// `tasks::spawn_irodori_health_watcher` から呼ばれる。
    pub async fn health_ping(&self) -> bool {
        let Some(port) = self.current_port() else {
            return true;
        };
        let url = format!("http://127.0.0.1:{port}/health");
        match self
            .client
            .get(&url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// 起動済みサイドカーがあって、`now_secs` 時点で `idle_secs` 以上呼ばれていなければ shutdown する。
    /// `tasks::spawn_irodori_idle_watcher` が定期的に呼ぶ。
    /// 戻り値: shutdown を実行したら `true`、対象なしまたはアイドル未到達なら `false`。
    pub async fn shutdown_if_idle(&self, now_secs: i64, idle_secs: i64) -> Result<bool, TtsError> {
        let has_port = self.current_port().is_some();
        let last = self.last_used.load(Ordering::Relaxed);
        if !should_shutdown_for_idle(has_port, last, now_secs, idle_secs) {
            return Ok(false);
        }
        self.shutdown().await?;
        Ok(true)
    }

    /// サイドカーが起動済みか確認し、未起動なら立ち上げる。port を返す。
    /// `mock=true` で sidecar.py を `--mock` で起動 (Phase D 検証用)。
    /// `app` を渡すとサイドカー stderr の `[hf-download]` 行が `irodori-download` イベント
    /// に転送される (M4c Phase G、実モデル初回起動時の HF DL 進捗表示用)。テスト等は None で可。
    ///
    /// `disable_for()` で sticky cooldown が立っている間は即 `SidecarStart` で弾く。
    /// GPU 永続不在環境で 90 秒 churn を繰り返すのを防ぐ。
    pub async fn ensure_sidecar_running(
        &self,
        asset_root: &Path,
        mock: bool,
        app: Option<AppHandle>,
    ) -> Result<u16, TtsError> {
        if let Some(port) = self.current_port() {
            return Ok(port);
        }
        if self.currently_disabled() {
            return Err(TtsError::SidecarStart(
                "直近の失敗により一時停止中です (cooldown)。voicevox 経路で発話します".to_string(),
            ));
        }
        // **導入・更新の最中は新しく起動しない** (v0.5.5 項目 4、spec §6.0)。
        // 入れ替え中の `site-packages` で起動すると、半分だけ新しい状態で読み込む。
        // すでに動いているものは止めない（上で `current_port()` を返している）— 走っている
        // 発話を切らないため。ここで弾くと `decide_fallback` が voicevox へ流す。
        if crate::tts::irodori_download::is_busy() {
            return Err(TtsError::SidecarStart(
                "Irodori ランタイムの導入または更新が進行中です。voicevox 経路で発話します"
                    .to_string(),
            ));
        }
        let script = asset_root.join("sidecar.py");
        // 行き先の判定と伏字は `route_stderr_line`（pure 関数）でテストする。
        let on_stderr = self.stderr_sink(app);
        let handle = sidecar::start_sidecar(asset_root, &script, mock, on_stderr)
            .await
            .map_err(|e| TtsError::SidecarStart(format!("{e:#}")))?;
        self.adopt_sidecar(handle).await
    }

    /// 起動し終えたサイドカーを採用する。
    ///
    /// **起動前の busy 判定だけでは足りない**（2026-09-14 監査で発覚）。起動には数秒かかり、
    /// その間に更新が始まると、更新側の `shutdown()` は**まだ保存されていないハンドル**を見て
    /// 何もしない。そのまま保存すると、入れ替え中の `site-packages` で起動したサイドカーが居座る。
    /// **保存と同じ錠の中で busy を見直す。** 更新側は busy を立ててから `shutdown()`（同じ錠）を
    /// 呼ぶので、どちらが先に錠を取っても取りこぼさない。
    async fn adopt_sidecar(&self, handle: SidecarHandle) -> Result<u16, TtsError> {
        enum Adopt {
            Stored(u16),
            // 別スレッドが先に起動済み。自分の handle を捨てて相手を使う。
            Redundant(u16, SidecarHandle),
            // 起動の最中に導入・更新が始まった。
            Busy(SidecarHandle),
        }
        let decision = {
            let mut guard = self.sidecar.lock().expect("irodori sidecar poisoned");
            if let Some(existing) = guard.as_ref() {
                Adopt::Redundant(existing.port, handle)
            } else if crate::tts::irodori_download::is_busy() {
                Adopt::Busy(handle)
            } else {
                let port = handle.port;
                *guard = Some(handle);
                Adopt::Stored(port)
            }
        };
        match decision {
            Adopt::Stored(port) => Ok(port),
            Adopt::Redundant(existing_port, redundant) => {
                let _ = sidecar::shutdown_sidecar(redundant, &self.client).await;
                Ok(existing_port)
            }
            Adopt::Busy(started) => {
                let _ = sidecar::shutdown_sidecar(started, &self.client).await;
                Err(TtsError::SidecarStart(
                    "起動の途中で Irodori ランタイムの導入または更新が始まったため、起動したサイドカーを止めました。voicevox 経路で発話します"
                        .to_string(),
                ))
            }
        }
    }

    /// 起動時の孤児掃除（`sidecar::sweep_orphans`）が使う HTTP クライアント。
    /// タイムアウトや proxy 設定を掃除側で作り直さないよう、同じものを貸す。
    pub fn http_client(&self) -> reqwest::Client {
        self.client.clone()
    }

    fn current_port(&self) -> Option<u16> {
        let guard = self.sidecar.lock().expect("irodori sidecar poisoned");
        guard.as_ref().map(|h| h.port)
    }

    /// 既存サイドカーがあれば shutdown する。`lifecycle::quit_app` と
    /// `tasks::spawn_irodori_idle_watcher` から呼ばれる。
    pub async fn shutdown(&self) -> Result<(), TtsError> {
        let handle = {
            let mut guard = self.sidecar.lock().expect("irodori sidecar poisoned");
            guard.take()
        };
        // 起動なし状態に戻す (次回 ensure_sidecar_running で再起動できるよう sentinel に)。
        self.last_used.store(0, Ordering::Relaxed);
        if let Some(h) = handle {
            sidecar::shutdown_sidecar(h, &self.client)
                .await
                .map_err(|e| TtsError::Http(format!("shutdown 失敗: {e:#}")))?;
        }
        Ok(())
    }

    /// テキストを Irodori サイドカーに送って WAV (バイト列) を取得する。
    ///
    /// 呼び出し側で参照音声のパスを `voice_ref_path` に渡す。サイドカーはそのファイルを
    /// 参照音声として読んで合成する。Phase D モックモードでは voice_ref_path を無視し
    /// 正弦波を返す。
    pub async fn synthesize(
        &self,
        asset_root: &Path,
        text: &str,
        voice_ref_path: &Path,
        speed: f64,
        caption: Option<String>,
        mock: bool,
        app: Option<AppHandle>,
    ) -> Result<Vec<u8>, TtsError> {
        self.remember_secrets(&[text, caption.as_deref().unwrap_or("")]);
        let port = self.ensure_sidecar_running(asset_root, mock, app).await?;
        self.touch_last_used();
        let url = format!("http://127.0.0.1:{port}/v1/audio/speech");
        let body = SpeechRequest {
            model: "irodori-voice-clone".to_string(),
            input: text.to_string(),
            voice: voice_ref_path.to_string_lossy().into_owned(),
            response_format: "wav".to_string(),
            speed,
            caption,
        };
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| TtsError::Http(format!("{e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            let caption_ref = body.caption.as_deref().unwrap_or("");
            return Err(TtsError::Http(format!(
                "{status}: {}",
                sanitize_sidecar_error(&body_text, &secret_variants([text, caption_ref]))
            )));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| TtsError::Http(format!("body 受信失敗: {e}")))?;
        Ok(bytes.to_vec())
    }

    /// キャプションから参照音声 wav を生成して `out_path` に保存する。
    pub async fn generate_voice_ref(
        &self,
        asset_root: &Path,
        caption: &str,
        out_path: &Path,
        mock: bool,
        app: Option<AppHandle>,
    ) -> Result<PathBuf, TtsError> {
        self.remember_secrets(&[caption]);
        let port = self.ensure_sidecar_running(asset_root, mock, app).await?;
        self.touch_last_used();
        let url = format!("http://127.0.0.1:{port}/v1/voice_ref/generate");
        let body = VoiceRefRequest {
            caption: caption.to_string(),
            out_path: out_path.to_string_lossy().into_owned(),
        };
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| TtsError::Http(format!("{e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            return Err(TtsError::Http(format!(
                "{status}: {}",
                sanitize_sidecar_error(&body_text, &secret_variants([caption]))
            )));
        }
        let r: VoiceRefResponse = resp
            .json()
            .await
            .map_err(|e| TtsError::Http(format!("voice_ref レスポンス解析失敗: {e}")))?;
        Ok(PathBuf::from(r.path))
    }
}

impl Default for IrodoriClient {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Serialize)]
struct SpeechRequest {
    model: String,
    input: String,
    voice: String,
    response_format: String,
    speed: f64,
    /// 台本の caption (docs/script-reader-spec.md §3.2)。None 時は JSON にキー自体を出さない
    /// (旧 sidecar 互換)。`ReadingChunk.caption` (常に `null` を出力) とは対照的な規約。
    #[serde(skip_serializing_if = "Option::is_none")]
    caption: Option<String>,
}

#[derive(Debug, Serialize)]
struct VoiceRefRequest {
    caption: String,
    out_path: String,
}

#[derive(Debug, Deserialize)]
struct VoiceRefResponse {
    #[allow(dead_code)]
    status: String,
    path: String,
}

/// TTS 共通エラー型。Irodori 経路の失敗をフロントへ伝える。
#[derive(Debug, Error)]
pub enum TtsError {
    #[error("Irodori-TTS は未実装です (M4c の後続 Phase で実装)")]
    #[allow(dead_code)]
    NotImplemented,
    #[error("Irodori サイドカーの起動に失敗しました: {0}")]
    SidecarStart(String),
    #[error("HTTP 通信に失敗しました: {0}")]
    Http(String),
    #[error("参照音声 (slot={0}) が未生成です。設定パネルから生成してください")]
    VoiceRefMissing(String),
}

#[cfg(test)]
mod tests {
    /// **更新の最中に新しいサイドカーを立てない** (v0.5.5 項目 4)。
    ///
    /// 入れ替え中の `site-packages` で起動すると、半分だけ新しい状態で読み込む。
    /// `IrodoriBusyGuard` を見ているのはコマンド 2 本だけで、合成の側は見ていなかった。
    ///
    /// 存在しない資産ルートを渡しているので、**弾かれなければ「sidecar.py が無い」**で
    /// 落ちる。エラーの中身で「busy で止めた」と「起動を試みた」を区別できる。
    #[tokio::test]
    async fn a_running_update_blocks_a_new_sidecar() {
        let _serial = crate::tts::irodori_download::lock_busy_for_test();
        let client = super::IrodoriClient::new();
        let nowhere = std::path::Path::new("Z:/ugg-does-not-exist");

        let guard = crate::tts::irodori_download::IrodoriBusyGuard::acquire().unwrap();
        let err = client
            .ensure_sidecar_running(nowhere, true, None)
            .await
            .expect_err("更新中は起動しないこと");
        assert!(
            format!("{err}").contains("進行中"),
            "起動を試みる前に弾くこと: {err}"
        );

        drop(guard);
        let err = client
            .ensure_sidecar_running(nowhere, true, None)
            .await
            .expect_err("資産が無いので別の理由で落ちる");
        assert!(
            !format!("{err}").contains("進行中"),
            "更新が終わったら塞がないこと: {err}"
        );
    }

    /// **起動の途中で更新が始まったら、起動したサイドカーを採用しない**（2026-09-14 監査で発覚）。
    ///
    /// 起動前の busy 判定の後、起動（数秒）の間に更新が始まると、更新側の `shutdown()` は
    /// まだ保存されていないハンドルを見て何もしない。採用の時点で見直さないと、入れ替え中の
    /// `site-packages` で起動したサイドカーが居座る。子プロセスには長く走る `ping` を使う。
    #[tokio::test]
    async fn an_update_that_begins_mid_launch_is_not_ignored() {
        let _serial = crate::tts::irodori_download::lock_busy_for_test();
        let dir = tempfile::tempdir().unwrap();
        let launched = || {
            let child = tokio::process::Command::new("ping")
                .args(["-n", "30", "127.0.0.1"])
                .stdout(std::process::Stdio::null())
                .kill_on_drop(false)
                .spawn()
                .expect("ping を起動できること");
            let pid = child.id().unwrap();
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            (super::SidecarHandle::for_test(dir.path(), port, pid, child), pid)
        };
        let client = super::IrodoriClient::new();

        // 起動の最中に更新が始まった
        let (started, _) = launched();
        let guard = crate::tts::irodori_download::IrodoriBusyGuard::acquire().unwrap();
        let err = client
            .adopt_sidecar(started)
            .await
            .expect_err("更新中に起動し終えたものは採用しない");
        assert!(format!("{err}").contains("導入または更新"), "{err}");
        assert!(client.current_port().is_none(), "居座らせない");
        drop(guard);

        // 更新が無ければ採用する
        let (started, pid) = launched();
        let port = started.port;
        assert_eq!(client.adopt_sidecar(started).await.unwrap(), port);
        assert_eq!(client.current_port(), Some(port), "採用したものを使う");
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
    }

    /// **JSON でエスケープされた発話も伏せる**（2026-09-14 監査で発覚）。
    ///
    /// 500 の本文は `{"detail": "..."}` の JSON なので、`"` や改行を含む発話はエスケープされた
    /// 形で載り、そのままの文字列とは一致しない。読み上げのチャンクや LLM の出力には普通に含まれる。
    #[test]
    fn a_json_escaped_utterance_is_still_hidden() {
        let spoken = "彼は「\"大丈夫\"」と言った\nそして帰った";
        let body = serde_json::json!({ "detail": format!("Irodori 合成失敗: bad input {spoken}") })
            .to_string();
        assert!(!body.contains(spoken), "前提: 本文ではエスケープされている");
        let got = super::sanitize_sidecar_error(&body, &[spoken, ""]);
        assert!(
            !got.contains("大丈夫") && !got.contains("帰った"),
            "発話が残っている: {got}"
        );
        assert!(got.contains("«伏字»"), "{got}");
    }

    /// **発話テキストを診断ログへ残さない** (v0.5.5 項目 1、spec §3.3 / v0.5.3 項目 7)。
    ///
    /// サイドカーは合成時の例外を `f"Irodori 合成失敗: {exc}"` に包んで 500 で返す。
    /// `{exc}` は発話テキストを含みうるので、送った本文とキャプションを伏せてから載せる。
    #[test]
    fn the_spoken_text_never_reaches_the_log() {
        let spoken = "きょうもおつかれさま、ゆっくりやすんでね";
        let caption = "明るく元気な少女の声";
        let body = format!("Irodori 合成失敗: TokenizerError at '{spoken}' (caption={caption})");

        let got = super::sanitize_sidecar_error(&body, &[spoken, caption]);

        assert!(!got.contains(spoken), "発話テキストが残っている: {got}");
        assert!(!got.contains(caption), "キャプションが残っている: {got}");
        assert!(
            got.contains("TokenizerError"),
            "診断に要る部分まで消してはいけない: {got}"
        );
    }

    /// **サイドカーの stderr も伏せる**（v0.5.6 項目 2）。stderr は行ごとに届くので、
    /// 複数行の本文は丸ごとは一致しない。各行でも伏せる。
    #[test]
    fn a_stderr_line_hides_every_line_of_a_recent_utterance() {
        let recent = vec![
            "きょうもおつかれさま\nゆっくりやすんでね".to_string(),
            "明るく元気な少女の声".to_string(),
        ];
        let got = super::sanitize_stderr_line("WARNING: odd token in 'ゆっくりやすんでね'", &recent);
        assert!(!got.contains("ゆっくり"), "{got}");
        assert!(got.contains("WARNING: odd token"), "診断に要る部分は残す: {got}");
        // Python の repr は改行を \n と書く
        let got = super::sanitize_stderr_line(
            "ValueError: 'きょうもおつかれさま\\nゆっくりやすんでね' (明るく元気な少女の声)",
            &recent,
        );
        assert!(
            !got.contains("おつかれ") && !got.contains("ゆっくり") && !got.contains("少女"),
            "{got}"
        );
        // 平時の行は変えない
        let timing = "[irodori] 合成 239 ms（8 ステップ・sway・参照 latent）";
        assert_eq!(super::sanitize_stderr_line(timing, &recent), timing);
    }

    /// **Python が書き換える文字を含む本文も伏せる**（v0.5.6 項目 2 のレビュー指摘）。
    /// `repr` は表示できない文字を `‍` のように書くので、そのままの形では一致しない。
    /// 絵文字の結合（溜息 `😮‍💨`。`preprocess` が残す）と全角空白が対象。
    #[test]
    fn an_utterance_with_characters_python_escapes_is_still_hidden() {
        let spoken = "ふうっ😮\u{200D}💨つかれた";
        let with_wide_space = "きょうも\u{3000}おつかれさま";
        let recent = vec![spoken.to_string(), with_wide_space.to_string()];
        // Python の repr 相当（表示できない文字がエスケープされた形）
        let logged = format!(
            "ValueError: bad token in 'ふうっ😮\\u200d💨つかれた' / 'きょうも\\u3000おつかれさま'"
        );
        let got = super::sanitize_stderr_line(&logged, &recent);
        assert!(
            !got.contains("つかれた") && !got.contains("おつかれさま") && !got.contains("ふうっ"),
            "発話が残っている: {got}"
        );
        assert!(got.contains("ValueError"), "診断に要る部分は残す: {got}");
    }

    /// 伏せるのは振り分けより前（画面へ流す進捗の行も伏せる）。進捗以外はログへ。
    #[test]
    fn stderr_lines_are_hidden_before_they_are_routed() {
        let recent = vec!["ないしょのはなし".to_string()];
        assert_eq!(
            super::route_stderr_line("[hf-download] ないしょのはなし を確認中…", &recent),
            super::StderrRoute::Progress("[hf-download] «伏字» を確認中…".to_string())
        );
        assert_eq!(
            super::route_stderr_line("Traceback: ないしょのはなし", &recent),
            super::StderrRoute::Log("Traceback: «伏字»".to_string())
        );
    }

    /// **覚えた本文が、stderr の受け口を通って実際に伏せられる**（v0.5.6 項目 2）。
    /// 純関数のテストだけだと、受け口が別の入れ物を見ていても・伏字を通さなくても緑になる。
    #[test]
    fn the_stderr_sink_hides_what_the_client_has_sent() {
        let client = super::IrodoriClient::new();
        let seen = std::sync::Arc::new(StdMutex::new(Vec::new()));
        let collected = seen.clone();
        let mut sink = client.stderr_sink_to(move |route| {
            collected.lock().unwrap().push(route);
        });

        client.remember_secrets(&["ないしょの本文です", "ないしょの声色"]);
        sink("ValueError: ないしょの本文です を読めません");
        sink("[hf-download] ないしょの声色 を確認中…");
        sink("[irodori] 合成 239 ms（8 ステップ・sway・参照 latent）");

        let got = seen.lock().unwrap();
        assert_eq!(
            got[0],
            super::StderrRoute::Log("ValueError: «伏字» を読めません".to_string())
        );
        assert_eq!(
            got[1],
            super::StderrRoute::Progress("[hf-download] «伏字» を確認中…".to_string())
        );
        assert_eq!(
            got[2],
            super::StderrRoute::Log(
                "[irodori] 合成 239 ms（8 ステップ・sway・参照 latent）".to_string()
            ),
            "平時の行は変えない"
        );
    }

    /// **送った本文を覚える**（stderr を伏せる材料）。失敗した要求でも、送る前に覚えている
    /// （stderr は応答より先に届きうる）。覚える数には上限がある。
    #[tokio::test]
    async fn what_is_sent_is_remembered_for_hiding_stderr() {
        // 応答しないポート（接続は拒否される）へ送らせる。送る前に覚えているかを見る。
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let dir = tempfile::tempdir().unwrap();
        let child = tokio::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let client = super::IrodoriClient::new();
        *client.sidecar.lock().unwrap() =
            Some(super::SidecarHandle::for_test(dir.path(), port, pid, child));

        let _ = client
            .synthesize(dir.path(), "ないしょの本文です", std::path::Path::new("x.wav"), 1.0, Some("ないしょの声色".to_string()), false, None)
            .await;
        let _ = client
            .generate_voice_ref(dir.path(), "べつの声色の説明", &dir.path().join("o.wav"), false, None)
            .await;
        let recent: Vec<String> = client.recent_secrets.lock().unwrap().iter().cloned().collect();
        assert_eq!(recent, ["ないしょの本文です", "ないしょの声色", "べつの声色の説明"]);

        for i in 0..20 {
            client.remember_secrets(&[&format!("本文その{i}")]);
        }
        let recent = client.recent_secrets.lock().unwrap();
        assert_eq!(recent.len(), super::RECENT_SECRETS);
        assert_eq!(recent.back().map(String::as_str), Some("本文その19"));
        drop(recent);
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
    }

    /// 短すぎる文字列で置換すると無関係な語まで潰れて診断にならない。
    #[test]
    fn short_secrets_are_not_redacted() {
        let got = super::sanitize_sidecar_error("CUDA out of memory", &["a", "of", ""]);
        assert_eq!(got, "CUDA out of memory", "3 文字以下は伏せない");
    }

    /// 長いボディはログへ丸ごと載せない（`llm.rs` と同じ規律）。
    #[test]
    fn a_long_body_is_truncated() {
        let body = "x".repeat(500);
        let got = super::sanitize_sidecar_error(&body, &[]);
        assert!(got.chars().count() < 400, "切り詰めていない: {}", got.chars().count());
        assert!(got.ends_with("…(以下省略)"));
    }

    use super::*;

    #[test]
    fn voice_ref_missing_error_includes_slot() {
        let err = TtsError::VoiceRefMissing("main".to_string());
        let msg = format!("{err}");
        assert!(msg.contains("main"));
        assert!(msg.contains("参照音声"));
    }

    #[test]
    fn sidecar_start_error_keeps_inner_message() {
        let err = TtsError::SidecarStart("python.exe not found".to_string());
        let msg = format!("{err}");
        assert!(msg.contains("python.exe not found"));
    }

    // 実 HTTP を打つテストは Phase G で integration テストに分離。
    // 本モジュールでは TtsError の表現と内部型のシリアライズを最低限カバー。

    #[tokio::test]
    async fn shutdown_if_idle_does_nothing_when_no_sidecar() {
        let client = IrodoriClient::new();
        // 未起動状態 (port=None, last_used=0) では何もしないで false を返す
        let acted = client.shutdown_if_idle(1_000_000, 60).await.unwrap();
        assert!(!acted);
    }

    #[test]
    fn should_notify_unavailable_gates_within_cooldown() {
        let client = IrodoriClient::new();
        // 初回は true (last=0 sentinel → 即発火)
        assert!(client.should_notify_unavailable());
        // 直後の再呼び出しは cooldown 内なので false
        assert!(!client.should_notify_unavailable());
    }

    #[test]
    fn should_notify_unavailable_re_fires_after_300s() {
        let client = IrodoriClient::new();
        assert!(client.should_notify_unavailable_at(1_000));
        // 直後 / 299s 経過は cooldown 内 → false
        assert!(!client.should_notify_unavailable_at(1_000));
        assert!(!client.should_notify_unavailable_at(1_299));
        // 300s ぴったり (>= cooldown) は再発火
        assert!(client.should_notify_unavailable_at(1_300));
    }

    #[test]
    fn should_notify_unavailable_handles_clock_skew_backward() {
        let client = IrodoriClient::new();
        assert!(client.should_notify_unavailable_at(2_000));
        // 時計が後ろに巻き戻った: last (2000) > now (1500) → cooldown 計算が壊れないように発火 + last を 1500 に同期
        assert!(client.should_notify_unavailable_at(1_500));
        // その直後の同時刻呼び出しは cooldown で false
        assert!(!client.should_notify_unavailable_at(1_500));
    }

    #[test]
    fn should_notify_unavailable_only_one_caller_wins_under_concurrency() {
        use std::sync::atomic::{AtomicUsize, Ordering as O};
        use std::sync::Arc;
        // CAS 化したので、同時刻で 16 並列に呼んでも 1 度だけ true を返す
        let client = Arc::new(IrodoriClient::new());
        let true_count = Arc::new(AtomicUsize::new(0));
        let mut handles = vec![];
        for _ in 0..16 {
            let c = client.clone();
            let cnt = true_count.clone();
            handles.push(std::thread::spawn(move || {
                if c.should_notify_unavailable_at(10_000) {
                    cnt.fetch_add(1, O::Relaxed);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(true_count.load(O::Relaxed), 1);
    }

    #[test]
    fn is_hf_progress_line_matches_only_prefix() {
        assert!(is_hf_progress_line("[hf-download] Aratako/X をダウンロード中…"));
        assert!(is_hf_progress_line("[hf-download] 完了"));
        assert!(!is_hf_progress_line(" [hf-download] leading space"));
        assert!(!is_hf_progress_line(""));
        assert!(!is_hf_progress_line(
            "INFO:     127.0.0.1:54321 - GET /health HTTP/1.1 200 OK"
        ));
        assert!(!is_hf_progress_line("sidecar.py: backend 初期化失敗"));
    }

    #[test]
    fn should_shutdown_for_idle_handles_all_branches() {
        // 起動なし → 何もしない
        assert!(!should_shutdown_for_idle(false, 1_000, 999_999, 300));
        // 起動あり + last_used=0 sentinel → 何もしない
        assert!(!should_shutdown_for_idle(true, 0, 999_999, 300));
        // 経過 0 (起動直後) → 何もしない
        assert!(!should_shutdown_for_idle(true, 1_000, 1_000, 300));
        // 経過 299 秒 (threshold 未満) → 何もしない
        assert!(!should_shutdown_for_idle(true, 1_000, 1_299, 300));
        // 経過 300 秒 (threshold 到達) → shutdown
        assert!(should_shutdown_for_idle(true, 1_000, 1_300, 300));
        // 経過 600 秒 (threshold 超え) → shutdown
        assert!(should_shutdown_for_idle(true, 1_000, 1_600, 300));
    }

    #[test]
    fn speech_request_serializes_with_snake_case_fields() {
        let req = SpeechRequest {
            model: "irodori-voice-clone".into(),
            input: "こんにちは".into(),
            voice: "C:/refs/main_1.wav".into(),
            response_format: "wav".into(),
            speed: 1.2,
            caption: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"response_format\":\"wav\""));
        assert!(json.contains("\"voice\":\"C:/refs/main_1.wav\""));
        assert!(json.contains("\"speed\":1.2"));
    }

    // test23 (docs/script-reader-spec.md §5.1): SpeechRequest の caption 直列化。
    // Some → フィールドあり、None → フィールドなし (skip_serializing_if の確認)。
    // 旧 sidecar 互換の根拠 (caption キー自体を送らなければ pydantic 既定値 None で通る)。
    #[test]
    fn test23_speech_request_caption_some_includes_field() {
        let req = SpeechRequest {
            model: "irodori-voice-clone".into(),
            input: "えええ！！".into(),
            voice: "C:/refs/main_1.wav".into(),
            response_format: "wav".into(),
            speed: 1.0,
            caption: Some("驚いて大声で".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"caption\":\"驚いて大声で\""), "unexpected: {json}");
    }

    #[test]
    fn test23_speech_request_caption_none_omits_field() {
        let req = SpeechRequest {
            model: "irodori-voice-clone".into(),
            input: "こんにちは".into(),
            voice: "C:/refs/main_1.wav".into(),
            response_format: "wav".into(),
            speed: 1.0,
            caption: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("caption"), "unexpected: {json}");
    }
}

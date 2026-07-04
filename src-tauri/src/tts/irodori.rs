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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex as StdMutex;
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
/// `[hf-download]` で始まる行だけ通し、uvicorn の INFO ログや warning は捨てる。
pub(crate) fn is_hf_progress_line(line: &str) -> bool {
    line.starts_with("[hf-download]")
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
        }
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
        let script = asset_root.join("sidecar.py");
        // [hf-download] 接頭辞の行のみ irodori-download イベントへ転送する。
        // 他の uvicorn / sidecar.py 標準ログは捨てる (ノイズ抑制 + 機密漏洩防止)。
        // 接頭辞判定は is_hf_progress_line (pure 関数) に切り出してテストでカバー。
        let on_stderr = move |line: &str| {
            if let Some(app) = &app {
                if is_hf_progress_line(line) {
                    let _ = app.emit("irodori-download", line);
                }
            }
        };
        let handle = sidecar::start_sidecar(asset_root, &script, mock, on_stderr)
            .await
            .map_err(|e| TtsError::SidecarStart(format!("{e:#}")))?;
        let port = handle.port;

        // 競合チェック: 別スレッドが先に起動済みなら自分の handle を捨てる。
        let conflict: Option<(u16, SidecarHandle)> = {
            let mut guard = self.sidecar.lock().expect("irodori sidecar poisoned");
            if let Some(existing) = guard.as_ref() {
                Some((existing.port, handle))
            } else {
                *guard = Some(handle);
                None
            }
        };

        match conflict {
            Some((existing_port, redundant)) => {
                let _ = sidecar::shutdown_sidecar(redundant, &self.client).await;
                Ok(existing_port)
            }
            None => Ok(port),
        }
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
            let text = resp.text().await.unwrap_or_default();
            return Err(TtsError::Http(format!("{status}: {text}")));
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
            let text = resp.text().await.unwrap_or_default();
            return Err(TtsError::Http(format!("{status}: {text}")));
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

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
use thiserror::Error;

use crate::tts::sidecar::{self, SidecarHandle};

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
        }
    }

    fn touch_last_used(&self) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.last_used.store(ts, Ordering::Relaxed);
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
        // 未起動なら何もしない
        if self.current_port().is_none() {
            return Ok(false);
        }
        let last = self.last_used.load(Ordering::Relaxed);
        if last == 0 || (now_secs - last) < idle_secs {
            return Ok(false);
        }
        self.shutdown().await?;
        Ok(true)
    }

    /// サイドカーが起動済みか確認し、未起動なら立ち上げる。port を返す。
    /// `mock=true` で sidecar.py を `--mock` で起動 (Phase D 検証用)。
    pub async fn ensure_sidecar_running(&self, asset_root: &Path, mock: bool) -> Result<u16, TtsError> {
        if let Some(port) = self.current_port() {
            return Ok(port);
        }
        let script = asset_root.join("sidecar.py");
        let handle = sidecar::start_sidecar(asset_root, &script, mock)
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
        mock: bool,
    ) -> Result<Vec<u8>, TtsError> {
        let port = self.ensure_sidecar_running(asset_root, mock).await?;
        self.touch_last_used();
        let url = format!("http://127.0.0.1:{port}/v1/audio/speech");
        let body = SpeechRequest {
            model: "irodori-voice-clone".to_string(),
            input: text.to_string(),
            voice: voice_ref_path.to_string_lossy().into_owned(),
            response_format: "wav".to_string(),
            speed,
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
    ) -> Result<PathBuf, TtsError> {
        let port = self.ensure_sidecar_running(asset_root, mock).await?;
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
    fn speech_request_serializes_with_snake_case_fields() {
        let req = SpeechRequest {
            model: "irodori-voice-clone".into(),
            input: "こんにちは".into(),
            voice: "C:/refs/main_1.wav".into(),
            response_format: "wav".into(),
            speed: 1.2,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"response_format\":\"wav\""));
        assert!(json.contains("\"voice\":\"C:/refs/main_1.wav\""));
        assert!(json.contains("\"speed\":1.2"));
    }
}

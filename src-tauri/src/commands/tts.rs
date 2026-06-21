//! TTS コマンド: 合成 / 話者一覧 / 資産有無 / 資産 DL / GitHub PAT。

use std::sync::Arc;

use base64::Engine;
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use crate::state::{AppState, Settings};
use crate::system::notify::{self, NoticeKind};
use crate::system::secrets;
use crate::tts::{download, voicevox};

#[derive(Debug, Clone, Serialize)]
pub struct VoiceOption {
    pub id: u32,
    pub name: String,
}

#[tauri::command]
pub fn voicevox_assets_ready() -> Result<bool, String> {
    let dir = crate::state::voicevox_asset_dir().map_err(|e| format!("{e:#}"))?;
    Ok(voicevox::assets_ready(&dir))
}

/// 合成器の話者/スタイル一覧。
/// 未 init の場合は必要に応じて init を試みる。
#[tauri::command]
pub fn list_voices(state: State<'_, Arc<AppState>>) -> Result<Vec<VoiceOption>, String> {
    ensure_engine(&state)?;
    let guard = state.tts.voicevox.lock().expect("tts poisoned");
    let engine = guard
        .as_ref()
        .ok_or_else(|| "voicevox engine が初期化されていません".to_string())?;
    let json = engine.metas_json()?;
    parse_speakers_to_voice_options(&json)
}

/// メインキャラ・サブキャラの slot に応じて合成。speed/volume は WAV 化後にフロント側で適用。
#[tauri::command]
pub async fn synthesize_voice(
    text: String,
    slot: String,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let settings = state.settings.lock().expect("settings poisoned").clone();
    if !settings.tts_enabled {
        return Err("TTS は無効化されています".to_string());
    }
    let style_id = match slot.as_str() {
        "main" => settings.tts_speaker_main,
        "sub" => settings.tts_speaker_sub,
        other => return Err(format!("未知の slot: {other}")),
    };

    // 合成はブロッキングなので spawn_blocking。
    let state_clone = state.inner().clone();
    let text_owned = text.clone();
    let wav = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        ensure_engine(&state_clone)?;
        let guard = state_clone.tts.voicevox.lock().expect("tts poisoned");
        let engine = guard
            .as_ref()
            .ok_or_else(|| "voicevox engine が初期化されていません".to_string())?;
        engine.synthesize(&text_owned, style_id)
    })
    .await
    .map_err(|e| format!("合成タスクの起動に失敗: {e}"))??;

    Ok(base64::engine::general_purpose::STANDARD.encode(wav))
}

/// 資産 DL。`agreed=true` 必須 (UI で利用規約に同意確認済み)。
/// 進捗は `voicevox-download` イベントを 1 行ずつ emit、完了時に "__done__"。
#[tauri::command]
pub async fn download_voicevox_assets(
    agreed: bool,
    gh_token: Option<String>,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    if !agreed {
        return Err("利用規約への同意が必要です".to_string());
    }
    let asset_dir = crate::state::voicevox_asset_dir().map_err(|e| format!("{e:#}"))?;

    // DL 前に既存 engine を破棄 (DLL を解放しないと上書きできない)。
    {
        let mut guard = state.tts.voicevox.lock().expect("tts poisoned");
        *guard = None;
    }

    let downloader = download::ensure_downloader(&asset_dir).await?;

    // 引数で渡された PAT が None かつ keyring にあればそれを使う。
    let token = match gh_token {
        Some(t) if !t.trim().is_empty() => Some(t),
        _ => secrets::get_api_key("github_token").ok().flatten(),
    };

    let app_clone = app.clone();
    let asset_dir_clone = asset_dir.clone();
    let token_clone = token.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        download::run_downloader(
            &downloader,
            &asset_dir_clone,
            token_clone.as_deref(),
            |line| {
                let _ = app_clone.emit("voicevox-download", line);
            },
        )
    })
    .await
    .map_err(|e| format!("DL タスクの起動に失敗: {e}"))?;

    let _ = app.emit("voicevox-download", "__done__");
    // 完了/失敗をゴーストに告知 (横断方針 §3.1)。
    let state_arc = state.inner().clone();
    match &result {
        Ok(()) => {
            notify::notify(&app, &state_arc, NoticeKind::VoicevoxDlComplete).await;
        }
        Err(err) => {
            notify::notify(
                &app,
                &state_arc,
                NoticeKind::VoicevoxDlFailed {
                    reason: err.clone(),
                },
            )
            .await;
        }
    }
    result
}

// === GitHub PAT 管理 (DL レート制限緩和用、keyring 経由) ===

const GH_TOKEN_KEY: &str = "github_token";

#[tauri::command]
pub fn set_github_token(token: String) -> Result<(), String> {
    if token.trim().is_empty() {
        return Err("トークンが空です".to_string());
    }
    secrets::set_api_key(GH_TOKEN_KEY, token.trim()).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn has_github_token() -> Result<bool, String> {
    secrets::has_api_key(GH_TOKEN_KEY).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn delete_github_token() -> Result<(), String> {
    secrets::delete_api_key(GH_TOKEN_KEY).map_err(|e| format!("{e:#}"))
}

// === 内部: engine 初期化 + speakers JSON のパース ===

fn ensure_engine(state: &Arc<AppState>) -> Result<(), String> {
    {
        let guard = state.tts.voicevox.lock().expect("tts poisoned");
        if guard.is_some() {
            return Ok(());
        }
    }
    let asset_dir = crate::state::voicevox_asset_dir().map_err(|e| format!("{e:#}"))?;
    if !voicevox::assets_ready(&asset_dir) {
        return Err("voicevox 資産が未ダウンロードです".to_string());
    }
    let engine = voicevox::VoicevoxEngine::init(&asset_dir)?;
    let mut guard = state.tts.voicevox.lock().expect("tts poisoned");
    *guard = Some(engine);
    Ok(())
}

/// voicevox の `/speakers` 同形式 JSON を flat な VoiceOption 列に展開。
/// 同一 style_id は重複除外。`name = "話者名 (スタイル名)"`。
fn parse_speakers_to_voice_options(json: &str) -> Result<Vec<VoiceOption>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("speakers JSON パース失敗: {e}"))?;
    let arr = v.as_array().ok_or_else(|| "speakers が配列ではない".to_string())?;
    let mut out: Vec<VoiceOption> = Vec::new();
    let mut seen = std::collections::HashSet::<u32>::new();
    for speaker in arr {
        let speaker_name = speaker
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("(unknown)");
        let Some(styles) = speaker.get("styles").and_then(|x| x.as_array()) else {
            continue;
        };
        for style in styles {
            let Some(id) = style.get("id").and_then(|x| x.as_u64()) else {
                continue;
            };
            let id = id as u32;
            if !seen.insert(id) {
                continue;
            }
            let style_name = style.get("name").and_then(|x| x.as_str()).unwrap_or("");
            out.push(VoiceOption {
                id,
                name: format!("{speaker_name} ({style_name})"),
            });
        }
    }
    out.sort_by_key(|v| v.id);
    Ok(out)
}

/// 設定変更時など、必要なら背景 init をキックする (起動時の事前 init 用)。
#[allow(dead_code)]
pub fn spawn_preinit(state: Arc<AppState>) {
    tokio::task::spawn_blocking(move || {
        let _ = ensure_engine(&state);
    });
}

/// Settings から TTS speed/volume を取り出す。
#[allow(dead_code)]
pub fn tts_params(settings: &Settings) -> (f64, f64) {
    (settings.tts_speed, settings.tts_volume)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_speakers_basic() {
        let json = r#"[
          {"name":"四国めたん","styles":[{"id":2,"name":"ノーマル","type":"talk"},{"id":3,"name":"あまあま","type":"talk"}]},
          {"name":"ずんだもん","styles":[{"id":3,"name":"ノーマル","type":"talk"}]}
        ]"#;
        let opts = parse_speakers_to_voice_options(json).unwrap();
        // id=3 は重複なので 1 件のみ
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].id, 2);
        assert!(opts[0].name.contains("四国めたん"));
    }
}

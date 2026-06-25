//! TTS コマンド: 合成 / 話者一覧 / 資産有無 / 資産 DL / GitHub PAT。
//!
//! M4c で Irodori 用コマンド (GPU 検出 / Irodori 資産 / 参照音声管理) も同居する。
//! `synthesize_voice` は `settings.tts_engine` で voicevox / irodori を振り分ける。

use std::sync::Arc;

use base64::Engine;
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use crate::db::VoiceRefRow;
use crate::state::{AppState, Settings};
use crate::system::notify::{self, NoticeKind};
use crate::system::secrets;
use crate::tts::{download, gpu, irodori_download, preprocess, voice_ref, voicevox};

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
/// `settings.tts_engine` で voicevox / irodori を振り分ける。
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
    if !matches!(slot.as_str(), "main" | "sub") {
        return Err(format!("未知の slot: {slot}"));
    }

    let wav = match settings.tts_engine.as_str() {
        "voicevox_core" | "" => synthesize_voicevox(state.inner().clone(), &settings, &slot, &text).await?,
        "irodori" => synthesize_irodori(state.inner().clone(), &slot, &text).await?,
        other => return Err(format!("未知の TTS エンジン: {other}")),
    };

    Ok(base64::engine::general_purpose::STANDARD.encode(wav))
}

async fn synthesize_voicevox(
    state: Arc<AppState>,
    settings: &Settings,
    slot: &str,
    text: &str,
) -> Result<Vec<u8>, String> {
    let style_id = match slot {
        "main" => settings.tts_speaker_main,
        "sub" => settings.tts_speaker_sub,
        other => return Err(format!("未知の slot: {other}")),
    };
    let text_owned = text.to_string();
    // 合成はブロッキングなので spawn_blocking。
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        ensure_engine(&state)?;
        let guard = state.tts.voicevox.lock().expect("tts poisoned");
        let engine = guard
            .as_ref()
            .ok_or_else(|| "voicevox engine が初期化されていません".to_string())?;
        engine.synthesize(&text_owned, style_id)
    })
    .await
    .map_err(|e| format!("合成タスクの起動に失敗: {e}"))?
}

/// Irodori 経路 (M4c Phase D 以降): サイドカーを起動し HTTP 経由で合成。
/// 漢字→ひらがな前処理は voicevox engine の OpenJtalk を流用 (architecture §7.5)。
async fn synthesize_irodori(
    state: Arc<AppState>,
    slot: &str,
    text: &str,
) -> Result<Vec<u8>, String> {
    let asset_root = voice_ref::irodori_root().map_err(|e| format!("{e:#}"))?;

    // 参照音声を DB から取得 (slot 未登録なら明示エラー)。
    let voice_ref_row = state
        .db
        .get_voice_ref(slot)
        .map_err(|e| format!("{e:#}"))?
        .ok_or_else(|| {
            format!(
                "{}",
                crate::tts::irodori::TtsError::VoiceRefMissing(slot.to_string())
            )
        })?;

    let preprocessed = preprocess_for_irodori(&state, text).unwrap_or_else(|_| text.to_string());
    let (speed, use_real) = {
        let s = state.settings.lock().expect("settings poisoned");
        (s.tts_speed, s.tts_irodori_use_real_model)
    };

    state
        .tts
        .irodori
        .synthesize(
            &asset_root,
            &preprocessed,
            std::path::Path::new(&voice_ref_row.file_path),
            speed,
            !use_real,
        )
        .await
        .map_err(|e| format!("{e}"))
}

/// voicevox の OpenJtalk を使った漢字→ひらがな変換を試みる。
/// voicevox 未 init / 失敗時は元テキストをそのまま使うフォールバック (呼び出し側で吸収)。
fn preprocess_for_irodori(state: &Arc<AppState>, text: &str) -> Result<String, String> {
    ensure_engine(state).map_err(|e| format!("preprocess 用 voicevox 未 init: {e}"))?;
    let guard = state.tts.voicevox.lock().expect("tts poisoned");
    let engine = guard
        .as_ref()
        .ok_or_else(|| "voicevox engine が初期化されていません".to_string())?;
    preprocess::to_hiragana(engine, text).map_err(|e| format!("preprocess: {e}"))
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

// === Irodori (M4c Phase A: スタブ) =====================================

#[derive(Debug, Clone, Serialize)]
pub struct GpuInfo {
    /// Irodori が利用可能な GPU が見つかったか。
    pub available: bool,
    /// 検出された GPU 名 (例 "NVIDIA GeForce RTX 4070")。
    pub name: Option<String>,
    /// 利用不可の理由 (UI 表示用、利用可なら None)。
    pub reason: Option<String>,
}

/// `voice_refs` 行をフロントへ返す形 (file_path は外に出さない)。
#[derive(Debug, Clone, Serialize)]
pub struct VoiceRef {
    pub slot: String,
    pub caption: String,
    pub created_ts: i64,
}

impl From<VoiceRefRow> for VoiceRef {
    fn from(r: VoiceRefRow) -> Self {
        Self {
            slot: r.slot,
            caption: r.caption,
            created_ts: r.created_ts,
        }
    }
}

/// Irodori 用 GPU 検出 (architecture §8.6, M4c Phase B)。
/// Windows DXGI で物理 GPU を列挙し、NVIDIA (VendorId=0x10DE) が見つかれば available=true。
/// 最終的な CUDA 利用可否は Phase D のサイドカー起動時に確認するので、ここは事前フィルタ。
#[tauri::command]
pub async fn irodori_check_gpu() -> GpuInfo {
    match gpu::list_adapters() {
        Ok(adapters) => match gpu::pick_irodori_gpu(&adapters) {
            gpu::IrodoriPick::Found { name } => GpuInfo {
                available: true,
                name: Some(name),
                reason: None,
            },
            gpu::IrodoriPick::NotNvidia { name } => GpuInfo {
                available: false,
                name: Some(name),
                reason: Some(
                    "NVIDIA GPU が見つかりません (Irodori-TTS は CUDA が必要です)".to_string(),
                ),
            },
            gpu::IrodoriPick::NoHardwareGpu => GpuInfo {
                available: false,
                name: None,
                reason: Some("物理 GPU が検出されませんでした".to_string()),
            },
        },
        Err(err) => GpuInfo {
            available: false,
            name: None,
            reason: Some(format!("GPU 検出に失敗しました: {err}")),
        },
    }
}

/// Irodori 資産 (Python embeddable / pip / torch / 共通依存) が揃っているか (Phase C 範囲)。
/// HF モデル本体の判定は Phase D で追加する。
#[tauri::command]
pub async fn irodori_assets_ready() -> bool {
    let Ok(root) = voice_ref::irodori_root() else {
        return false;
    };
    irodori_download::assets_ready(&root)
}

/// Irodori 用 Python ランタイム + 共通依存の初回 DL (architecture §8.2-8.3, M4c Phase C)。
/// 進捗は `irodori-download` イベントで 1 行ずつ emit、完了時に `"__done__"`。
///
/// 段取り:
///   1. Embeddable Python (Windows x64, 約 25MB) を DL → 展開 → `python._pth` 編集
///   2. `get-pip.py` で pip ブートストラップ
///   3. 共通依存 (fastapi / uvicorn / huggingface_hub / numpy / soundfile) を pip install
///   4. torch + torchaudio (CUDA 12.1) を pip install (1〜2GB)
#[tauri::command]
pub async fn download_irodori_assets(
    agreed: bool,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    if !agreed {
        return Err("利用規約への同意が必要です".to_string());
    }
    let asset_root = voice_ref::irodori_root().map_err(|e| format!("{e:#}"))?;
    std::fs::create_dir_all(&asset_root).map_err(|e| format!("資産ルート作成失敗: {e:#}"))?;

    let emit = {
        let app = app.clone();
        move |line: &str| {
            let _ = app.emit("irodori-download", line);
        }
    };

    let result: Result<(), String> = async {
        irodori_download::ensure_python_embeddable(&asset_root, &emit)
            .await
            .map_err(|e| format!("{e:#}"))?;
        irodori_download::ensure_pip(&asset_root, &emit)
            .await
            .map_err(|e| format!("{e:#}"))?;
        irodori_download::install_common_requirements(&asset_root, &emit)
            .await
            .map_err(|e| format!("{e:#}"))?;
        irodori_download::install_torch_cuda(&asset_root, &emit)
            .await
            .map_err(|e| format!("{e:#}"))?;
        Ok(())
    }
    .await;

    let _ = app.emit("irodori-download", "__done__");

    let state_arc = state.inner().clone();
    match &result {
        Ok(()) => {
            notify::notify(&app, &state_arc, NoticeKind::IrodoriDlComplete).await;
        }
        Err(reason) => {
            notify::notify(
                &app,
                &state_arc,
                NoticeKind::IrodoriDlFailed {
                    reason: reason.clone(),
                },
            )
            .await;
        }
    }
    result
}

/// 登録済みの参照音声一覧 (slot/caption/created_ts のみ、ファイルパスは隠す)。
#[tauri::command]
pub fn voice_ref_list(state: State<'_, Arc<AppState>>) -> Result<Vec<VoiceRef>, String> {
    let rows = state.db.list_voice_refs().map_err(|e| format!("{e:#}"))?;
    Ok(rows.into_iter().map(VoiceRef::from).collect())
}

/// 指定 slot の参照音声を削除 (DB 行 + .wav ファイル両方)。
#[tauri::command]
pub fn voice_ref_delete(
    slot: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<VoiceRef>, String> {
    if !matches!(slot.as_str(), "main" | "sub") {
        return Err(format!("未知の slot: {slot}"));
    }
    if let Some(row) = state.db.get_voice_ref(&slot).map_err(|e| format!("{e:#}"))? {
        let path = std::path::PathBuf::from(&row.file_path);
        if let Err(err) = voice_ref::delete_file(&path) {
            // ファイル削除失敗でも DB 行は削除する (パスがずれている場合の救済)。
            eprintln!("[voice_ref] file delete failed: {err:#}");
        }
    }
    state
        .db
        .delete_voice_ref(&slot)
        .map_err(|e| format!("{e:#}"))?;
    voice_ref_list(state)
}

/// 参照音声の新規生成 (キャプション → Irodori サイドカーで .wav 生成 → DB upsert)。
/// 同じ slot に既存があれば置き換え (UNIQUE(slot))、古い wav は削除する。
#[tauri::command]
pub async fn voice_ref_generate(
    slot: String,
    caption: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<VoiceRef>, String> {
    if !matches!(slot.as_str(), "main" | "sub") {
        return Err(format!("未知の slot: {slot}"));
    }
    let caption_trimmed = caption.trim().to_string();
    if caption_trimmed.is_empty() {
        return Err("キャプションが空です".to_string());
    }

    let asset_root = voice_ref::irodori_root().map_err(|e| format!("{e:#}"))?;
    let refs_dir = voice_ref::refs_dir().map_err(|e| format!("{e:#}"))?;

    // 新規 ID は (現時刻秒) を使い <slot>_<id>.wav とする (DB が後で id を発番するが
    // ファイル名先決→DB upsert の順で、UNIQUE(slot) で 1 行を更新する)。
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let out_path = voice_ref::ref_path_in_dir(&refs_dir, &slot, ts)
        .map_err(|e| format!("{e:#}"))?;

    // 既存ファイルがあれば事前削除 (上書きで構わないが、別 ID への切替時に残骸が残らないよう保険)。
    let previous = state.db.get_voice_ref(&slot).map_err(|e| format!("{e:#}"))?;

    let use_real = state
        .settings
        .lock()
        .expect("settings poisoned")
        .tts_irodori_use_real_model;

    // サイドカーへ生成依頼
    state
        .tts
        .irodori
        .generate_voice_ref(&asset_root, &caption_trimmed, &out_path, !use_real)
        .await
        .map_err(|e| format!("{e}"))?;

    // DB upsert
    state
        .db
        .upsert_voice_ref(&slot, &caption_trimmed, &out_path.to_string_lossy(), ts)
        .map_err(|e| format!("{e:#}"))?;

    // 古いファイル削除 (新パスと一致する場合は触らない)
    if let Some(prev) = previous {
        let prev_path = std::path::PathBuf::from(&prev.file_path);
        if prev_path != out_path {
            let _ = voice_ref::delete_file(&prev_path);
        }
    }

    voice_ref_list(state)
}

/// 既存の参照音声で短文プレビュー合成。`synthesize_voice` を Irodori 経路で呼ぶ薄いラッパ。
#[tauri::command]
pub async fn voice_ref_preview(
    slot: String,
    text: String,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    if !matches!(slot.as_str(), "main" | "sub") {
        return Err(format!("未知の slot: {slot}"));
    }
    if text.trim().is_empty() {
        return Err("プレビュー文字列が空です".to_string());
    }
    let state_arc = state.inner().clone();
    let wav = synthesize_irodori(state_arc, &slot, &text).await?;
    Ok(base64::engine::general_purpose::STANDARD.encode(wav))
}

// === GitHub PAT 管理 (DL レート制限緩和用、keyring 経由) ===

const GH_TOKEN_KEY: &str = "github_token";

// GitHub PAT も keyring 経由なので、API キーと同じく blocking pool に逃がす。
#[tauri::command]
pub async fn set_github_token(token: String) -> Result<(), String> {
    if token.trim().is_empty() {
        return Err("トークンが空です".to_string());
    }
    let t = token.trim().to_string();
    tauri::async_runtime::spawn_blocking(move || secrets::set_api_key(GH_TOKEN_KEY, &t))
        .await
        .map_err(|e| format!("keyring task 起動失敗: {e}"))?
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn has_github_token() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(|| secrets::has_api_key(GH_TOKEN_KEY))
        .await
        .map_err(|e| format!("keyring task 起動失敗: {e}"))?
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn delete_github_token() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| secrets::delete_api_key(GH_TOKEN_KEY))
        .await
        .map_err(|e| format!("keyring task 起動失敗: {e}"))?
        .map_err(|e| format!("{e:#}"))
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

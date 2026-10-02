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
use crate::system::notify::{self, NoticeKind, UnavailableKind};
use crate::system::secrets;
use crate::tts::irodori::TtsError;
use crate::tts::{download, gpu, irodori_download, preprocess, voice_ref, voicevox};

/// 合成で選べるエンジンの名前（`synthesize_voice` の戻りの `engine`。設定の `tts_engine` と同じ綴り）。
const ENGINE_VOICEVOX: &str = "voicevox_core";
const ENGINE_IRODORI: &str = "irodori";

/// `synthesize_voice` の戻り。**どのエンジンで鳴らしたか**をフロントへ伝える（v0.6.0 項目 2、spec §6.0）。
/// VOICEVOX のクレジット（規約上の常時表示）は、設定のエンジンではなく**実際に VOICEVOX で鳴らしたか**で
/// 出すため（高品質モードでもフォールバックで VOICEVOX が鳴る間は出す）。
/// 契約: `wav` = base64 の WAV、`engine` = `"voicevox_core"`（VOICEVOX で鳴らした）か `"irodori"`。
#[derive(Debug, Clone, Serialize)]
pub struct SynthesizedVoice {
    pub wav: String,
    pub engine: &'static str,
}

/// Irodori 経路の合成エラーを VOICEVOX へ回すときの種類に振り分ける（**エラーで返すものは無い**。無言にしない）。
/// VoiceRefMissing は故障ではなく「その slot の参照音声がまだ無い」ので、VOICEVOX で喋って 1 回だけ伝える
/// （`irodori_unavailable` は言わない・その 5 分の待ちも共有しない。v0.6.0 項目 3）。
/// それ以外 (SidecarStart / Http / NotImplemented ほか) は故障として VOICEVOX 経路で再合成し、
/// `irodori_unavailable` 系を言う (architecture §8.6)。
///
/// **参照音声のプレビュー（`voice_ref_preview`）はここを通らない**（`synthesize_irodori` を直接呼び、エラーのまま
/// 返す。作ったばかりの声を聴く道具なので、代わりの声で鳴らさない）。
#[derive(Debug, PartialEq)]
pub(crate) enum FallbackAction {
    FallbackForMissingRef,
    FallbackToVoicevox,
}

pub(crate) fn decide_fallback(err: &TtsError) -> FallbackAction {
    match err {
        TtsError::VoiceRefMissing(_) => FallbackAction::FallbackForMissingRef,
        TtsError::NotImplemented
        | TtsError::SidecarStart(_)
        | TtsError::Http(_)
        | TtsError::Updating
        | TtsError::Synth { .. } => FallbackAction::FallbackToVoicevox,
    }
}

/// 合成の失敗を、キャラが言う理由の種類へ（v0.5.7 項目 7、spec §6.0）。以前は理由によらず辞書の
/// `irodori_unavailable`（「GPU 環境が整っていないようだ」）を言い、更新中やモデルが無いときも事実と違う説明をした。
pub(crate) fn unavailable_kind(err: &TtsError) -> UnavailableKind {
    use crate::tts::irodori::SynthFailure;
    match err {
        TtsError::Updating => UnavailableKind::Updating,
        TtsError::Synth { kind: SynthFailure::ModelMissing, .. } => UnavailableKind::ModelMissing,
        TtsError::Synth { kind: SynthFailure::OutOfMemory, .. } => UnavailableKind::Vram,
        TtsError::Synth { kind: SynthFailure::Other, .. }
        | TtsError::SidecarStart(_)
        | TtsError::Http(_)
        | TtsError::NotImplemented
        // 参照音声が無いときはここへ来ない（`FallbackForMissingRef` は `IrodoriVoiceRefMissing` を言う）。
        // match の網羅のために置いてある
        | TtsError::VoiceRefMissing(_) => UnavailableKind::Other,
    }
}

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
///
/// Irodori 経路で **`VoiceRefMissing` 以外**のエラー (サイドカー起動失敗 / HTTP 失敗 / 501 等) が
/// 出たら `notify(IrodoriUnavailable)` を発火 + voicevox_core で再合成して可能なら音声を返す。
/// voicevox も使えない場合は元の irodori エラーを返す (architecture.md §8.6 / §13.4)。
///
/// **`VoiceRefMissing`（読み込み中のゴーストにその slot の参照音声が無い）は VOICEVOX で喋り**、
/// ゴースト × slot ごとに 1 回だけ `IrodoriVoiceRefMissing` を告げる（v0.6.0 項目 3。以前は無言だった）。
/// VOICEVOX で鳴らしたときは、声を決めていないゴーストに 1 回だけ案内する（`GhostVoiceUnset`）。
/// 戻りの `engine` は実際に鳴らしたエンジン（クレジットの表示に使う）。
#[tauri::command]
pub async fn synthesize_voice(
    text: String,
    slot: String,
    caption: Option<String>,
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<SynthesizedVoice, String> {
    let settings = state.settings.lock().expect("settings poisoned").clone();
    if !settings.tts_enabled {
        return Err("TTS は無効化されています".to_string());
    }
    if !matches!(slot.as_str(), "main" | "sub") {
        return Err(format!("未知の slot: {slot}"));
    }
    // 空文字列は None と同義に正規化する (docs/script-reader-spec.md §2.9)。
    let caption = caption.filter(|c| !c.is_empty());
    let state_arc = state.inner().clone();

    let (wav, engine) = match settings.tts_engine.as_str() {
        "voicevox_core" | "" => (
            synthesize_voicevox(state_arc.clone(), &settings, &slot, &text).await?,
            ENGINE_VOICEVOX,
        ),
        "irodori" => {
            let irodori_result =
                synthesize_irodori(state_arc.clone(), &slot, &text, caption.clone(), Some(app.clone()))
                    .await;
            match irodori_result {
                Ok(wav) => (wav, ENGINE_IRODORI),
                Err(err) => match decide_fallback(&err) {
                    // 参照音声が無い slot: VOICEVOX（読み込み中のゴーストの話者）で喋り、1 回だけ伝える。
                    // **`irodori_unavailable` を言わず、その 5 分の待ち（`should_notify_unavailable`）も使わない** —
                    // 故障ではないので事実と違う理由を言わない。
                    FallbackAction::FallbackForMissingRef => {
                        let wav = synthesize_voicevox(state_arc.clone(), &settings, &slot, &text)
                            .await
                            .map_err(|voicevox_err| {
                                format!("{err} (voicevox で代わりに喋ることも失敗: {voicevox_err})")
                            })?;
                        announce_voice_ref_missing_once(&app, &state_arc, &slot).await;
                        (wav, ENGINE_VOICEVOX)
                    }
                    FallbackAction::FallbackToVoicevox => {
                        let reason = format!("{err}");
                        // **理由は毎回ログへ残す** (v0.5.5 項目 1、2026-09-13 実機で発覚)。
                        // 通知は 5 分に 1 回へ絞っている（キャラが同じことを言い続けないため）。
                        // 理由のログをその後ろ（`notify()` の中）にだけ置くと、**2 回目以降の
                        // 失敗の理由がどこにも残らない** — 実機では起動時の失敗が 1 回通知した後、
                        // 更新中の「進行中です」がログから完全に消えていた。
                        // `reason` は生成元で伏字・切り詰め済み（`sanitize_sidecar_error`）。
                        crate::ulog!(
                            "[irodori] 合成に失敗しました（VOICEVOX へ切り替え）: {}",
                            crate::dialogue::llm::truncate_for_log(&reason)
                        );
                        // voicevox を先に試して、成功/失敗で notify の文面を出し分ける。
                        // (順序を逆にすると、voicevox 未 DL のとき『VOICEVOX 経路で発話します』を
                        //  音/吹き出しで案内したのに実際は両方無音、という嘘 UX に陥る)
                        match synthesize_voicevox(state_arc.clone(), &settings, &slot, &text)
                            .await
                        {
                            Ok(wav) => {
                                // 5 分クールダウン + dialogue 再帰防止: irodori.rs::should_notify_unavailable
                                if state_arc.tts.irodori.should_notify_unavailable() {
                                    notify::notify(
                                        &app,
                                        &state_arc,
                                        NoticeKind::IrodoriUnavailable {
                                            reason: reason.clone(),
                                            kind: unavailable_kind(&err),
                                        },
                                    )
                                    .await;
                                }
                                (wav, ENGINE_VOICEVOX)
                            }
                            Err(voicevox_err) => {
                                // 両方失敗: notify はキャラ発話を呼ぶ → やはり両方失敗するだけなので skip。
                                // フロント側 (speaker.ts) が console.warn して括る。reason は両方の理由をマージ。
                                return Err(format!(
                                    "{reason} (voicevox fallback も失敗: {voicevox_err})"
                                ));
                            }
                        }
                    }
                },
            }
        }
        other => return Err(format!("未知の TTS エンジン: {other}")),
    };

    // VOICEVOX で鳴らしたら（フォールバックを含む）、声を決めていないゴーストへ 1 回だけ案内する。
    if engine == ENGINE_VOICEVOX {
        announce_ghost_voice_unset_once(&app, &state_arc).await;
    }

    Ok(SynthesizedVoice {
        wav: base64::engine::general_purpose::STANDARD.encode(wav),
        engine,
    })
}

/// 参照音声が無い slot を VOICEVOX で話したことの告知（**ゴースト × slot、プロセスで 1 回**。v0.6.0 項目 3）。
/// 取り合いは原子的（main と sub を並べて合成しても 2 回出ない）、済みは**届いたときだけ**。告知の台詞自体の
/// 合成も `synthesize_voice` に戻ってくるが、取り合いの鍵が同じなので重ならない。
/// 読み込み中のゴーストが無ければ（辞書も無く、何も言えない）出さない。
pub(crate) async fn announce_voice_ref_missing_once(app: &AppHandle, state: &Arc<AppState>, slot: &str) {
    let Some(ghost_id) = state.loaded_ghost_id() else {
        return;
    };
    notify::once_reached_claimed(
        &state.tts.voice_notices,
        &voice_ref_missing_key(&ghost_id, slot),
        false,
        || notify::notify(app, state, NoticeKind::IrodoriVoiceRefMissing),
        || {},
    )
    .await;
}

/// 参照音声の告知の取り合いの鍵（ゴースト × slot）。
fn voice_ref_missing_key(ghost_id: &str, slot: &str) -> String {
    format!("voice_ref_missing:{ghost_id}:{slot}")
}

/// 声を決めていないゴーストが VOICEVOX で喋ったことの案内（**ゴーストごとに 1 回、届いたときだけ済み**。
/// v0.6.0 項目 2）。済みは `app_settings` の `ghost_voice_hint_seen:<ghost_id>`（再起動しても出ない）。
pub(crate) async fn announce_ghost_voice_unset_once(app: &AppHandle, state: &Arc<AppState>) {
    let Some(ghost_id) = state.loaded_ghost_id() else {
        return;
    };
    let settings = state.settings.lock().expect("settings poisoned").clone();
    ghost_voice_unset_once(
        &state.db,
        &state.tts.voice_notices,
        &settings,
        &ghost_id,
        || notify::notify(app, state, NoticeKind::GhostVoiceUnset),
    )
    .await;
}

/// `announce_ghost_voice_unset_once` の中身（`show` を差し替えられる。AppHandle 無しで流れをテストするため）。
async fn ghost_voice_unset_once<Fut>(
    db: &crate::db::Db,
    gate: &notify::OnceGate,
    settings: &Settings,
    ghost_id: &str,
    show: impl FnOnce() -> Fut,
) -> Option<notify::NoticeOutcome>
where
    Fut: std::future::Future<Output = notify::NoticeOutcome>,
{
    let seen_key = ghost_voice_hint_key(ghost_id);
    let seen = matches!(db.get_setting(&seen_key), Ok(Some(_)));
    notify::once_reached_claimed(
        gate,
        &format!("ghost_voice_unset:{ghost_id}"),
        !ghost_voice_hint_due(settings, ghost_id, seen),
        show,
        || {
            let _ = db.set_setting(&seen_key, "1");
        },
    )
    .await
}

/// 案内済みの記録の鍵（`update_notice_seen:<version>` と同じ作り）。
fn ghost_voice_hint_key(ghost_id: &str) -> String {
    format!("ghost_voice_hint_seen:{ghost_id}")
}

/// 案内が要るか（純粋部分）。**そのゴーストの行が `tts_speakers_by_ghost` に無く**（声を決めていない）、
/// まだ案内していないとき。移行で行が埋まった既存のゴースト（`from_persisted`）や、声を決めたゴーストには出さない。
fn ghost_voice_hint_due(settings: &Settings, ghost_id: &str, seen: bool) -> bool {
    !seen && !settings.tts_speakers_by_ghost.contains_key(ghost_id)
}

async fn synthesize_voicevox(
    state: Arc<AppState>,
    settings: &Settings,
    slot: &str,
    text: &str,
) -> Result<Vec<u8>, String> {
    // 話者は**読み込み中のゴースト**の行、無ければアプリの既定（v0.6.0 項目 2）。設定の ghost_id では引かない
    // （切り替えた直後〜再起動前は読み込み中のゴーストと別物で、旧ゴーストが新ゴーストの声で喋ってしまう）。
    // 旧版の単一の話者設定は移行の読み出し元で、ここでは読まない。
    let speakers = settings.speakers_for(state.loaded_ghost_id().as_deref());
    let style_id = match slot {
        "main" => speakers.main,
        "sub" => speakers.sub,
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
///
/// `synthesize_voice` で `TtsError` variant 単位の fallback 判定をするため、エラーは型のまま返す。
/// 呼び出し側 (voice_ref_preview など) はこの戻りを `format!("{e}")` で String に変換できる。
async fn synthesize_irodori(
    state: Arc<AppState>,
    slot: &str,
    text: &str,
    caption: Option<String>,
    app: Option<AppHandle>,
) -> Result<Vec<u8>, crate::tts::irodori::TtsError> {
    use crate::tts::irodori::TtsError;

    let asset_root = voice_ref::irodori_root()
        .map_err(|e| TtsError::SidecarStart(format!("{e:#}")))?;

    // 参照音声を DB から取得 (slot 未登録なら VoiceRefMissing で明示エラー)。
    // **読み込み中のゴースト**の行を引く（v0.6.0 項目 2。設定の ghost_id ではない）。
    // 読み込み中のゴーストが無い・テーブルが無い（破損 DB で移行が走らなかった）ときは「参照音声なし」。
    let voice_ref_row = match state.loaded_ghost_id() {
        Some(ghost_id) => state
            .db
            .get_ghost_voice_ref(&ghost_id, slot)
            .map_err(|e| TtsError::Http(format!("ghost_voice_refs lookup: {e:#}")))?,
        None => None,
    }
    .ok_or_else(|| TtsError::VoiceRefMissing(slot.to_string()))?;

    // 漢字→かな前処理 (voicevox の OpenJtalk を流用) は spec §4.5.1 で Irodori 経路には必須。
    // voicevox 資産が未 DL 等で前処理ができない場合は warning ログを出して raw text を渡す。
    // (Irodori は漢字の発話精度が不安定だが、合成失敗より「読みが多少不自然」のほうがマシ)。
    let preprocessed = preprocess_for_irodori(&state, text).unwrap_or_else(|err| {
        crate::ulog!("[irodori] kana preprocess fell back to raw text: {err}");
        text.to_string()
    });
    let (speed, use_real, steps) = {
        let s = state.settings.lock().expect("settings poisoned");
        (s.tts_speed, s.tts_irodori_use_real_model, s.tts_irodori_steps)
    };

    state
        .tts
        .irodori
        .synthesize(
            &asset_root,
            &preprocessed,
            std::path::Path::new(&voice_ref_row.file_path),
            speed,
            caption,
            steps,
            !use_real,
            app,
        )
        .await
}

/// voicevox の OpenJtalk を使った漢字→ひらがな変換を試みる。
/// Irodori-TTS V3 の絵文字アノテーション (感情・スタイル制御) は位置を保って残す。
/// voicevox 未 init / 失敗時は元テキストをそのまま使うフォールバック (呼び出し側で吸収)。
fn preprocess_for_irodori(state: &Arc<AppState>, text: &str) -> Result<String, String> {
    ensure_engine(state).map_err(|e| format!("preprocess 用 voicevox 未 init: {e}"))?;
    let guard = state.tts.voicevox.lock().expect("tts poisoned");
    let engine = guard
        .as_ref()
        .ok_or_else(|| "voicevox engine が初期化されていません".to_string())?;
    preprocess::to_hiragana_preserving_emoji(engine, text).map_err(|e| format!("preprocess: {e}"))
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
    // 始まりを残す（v0.5.7 リリース前監査。Irodori の導入・更新と同じ形。終わりは告知の `[notify]` の行に残る）
    crate::ulog!("[voicevox] 資産の取得を始めます");

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

/// `ghost_voice_refs` 行をフロントへ返す形 (file_path は外に出さない)。
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

/// 古くなった Irodori ランタイムを入れ直す (v0.5.4 項目 3、spec §6.0)。
///
/// 初回導入 (`download_irodori_assets`) と別経路にしているのは、目的が違うから。
/// あちらは「何も無いところに一式そろえる」、こちらは「**動いているものを壊さずに
/// 古い分だけ入れ替える**」。後者は**1 つのトランザクション**で行う（v0.5.6 項目 3）:
/// 入っている版を控え、固定の順で入れ直し、途中のどこで失敗しても入れ替えたものを全部戻す
/// （中身は `irodori_download::update_irodori_runtime`）。
///
/// 成功したら**入れ直せた分だけ**記録を書き換える（全部を現在値にすると、
/// 入れ直していない依存まで「最新」と記録してしまう）。
#[tauri::command]
pub async fn update_irodori_runtime(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<String>, String> {
    let root = voice_ref::irodori_root().map_err(|e| format!("{e:#}"))?;
    // 錠のファイルを置く前に作っておく（反証レビュー #13）。
    std::fs::create_dir_all(&root).map_err(|e| format!("資産ルート作成失敗: {e:#}"))?;
    // 初回 DL と同時に走ると、同じ site-packages を 2 経路が触って退避も復元も守れない。
    // **もう 1 つの ugg とも排他にする**（v0.5.6 項目 3f）。
    let _busy = irodori_download::IrodoriBusyGuard::acquire_for(&root).map_err(|e| format!("{e:#}"))?;
    let status = irodori_download::status(&root);
    if status.up_to_date {
        return Ok(Vec::new());
    }
    let emit = {
        let app = app.clone();
        move |line: &str| {
            let _ = app.emit("irodori-download", line);
        }
    };
    prepare_to_replace_runtime(state.inner(), &root, &emit).await?;
    // 対象は `status` が決める。**ここで自前に組み立てない** — 記録が無い環境で
    // `current_pins()` を丸ごと対象にしたところ、入れ直せない `python` が混ざって
    // Err で止まり、この機能が対象にしている環境がちょうど 1 つも更新できなかった。
    let targets = status.outdated;
    // **始まりと終わりを `ugg.log` に残す**（v0.5.7 リリース前監査）。進捗は画面にしか出ないので、更新が途中で
    // 止まったこと（E-11 で dev の再起動により pip の途中で止まった）が、site-packages の日時からしか分からなかった。
    // 始まりの行があって終わりの行が無ければ、途中で止まったと読める。
    crate::ulog!("[irodori] 更新を始めます（対象: {}）", targets.join(" / "));
    // 記録の更新は `update_irodori_runtime` の中で行う。**ここに置いていたため
    // テストから到達できず**、「入れ直したのに `up_to_date` が false のまま」を
    // 自動で検出できなかった。
    let result = irodori_download::update_irodori_runtime(&root, &targets, &emit).await;
    match &result {
        Ok(updated) => crate::ulog!("[irodori] 更新が終わりました（入れ直したもの: {}）", updated.join(" / ")),
        Err(e) => crate::ulog!("[irodori] 更新が終わりませんでした: {e:#}"),
    }
    let updated = result.map_err(|e| format!("{e:#}"))?;
    let _ = app.emit("irodori-download", "__done__");
    Ok(updated)
}

/// 導入・更新の入口の備え（v0.5.6 項目 3e。両方のコマンドが通る）。
///
/// 1. **自分のサイドカーを止める。** 止めないと (a) 入れ直しても合成は旧コードのまま
///    （`ensure_sidecar_running` は版を見ずに既存ポートを返す）(b) 遅延 import するモジュールを差し替えると
///    動いているプロセスが壊れる。次の合成で新しいコードが起動する
/// 2. **起動時の孤児掃除が終わるのを待つ。** 掃除は起動時に投げっぱなしで、誰も待っていなかった
/// 3. **持ち主のいない孤児を止める**（v0.5.6 項目 4。台帳に持ち主の欄が入って、ほかの ugg が使っている
///    ものと見分けられるようになった）
/// 4. **記録にあるサイドカーで生きているものが残っていれば始めない。** 生きているサイドカーは torch の DLL を
///    読み込んだままで、Windows はそれを消すことも上書きすることも拒む — 入れ替えも、失敗したときの全戻しも
///    失敗する（反証レビュー #4）。ほかの ugg が使っているものは止めない
async fn prepare_to_replace_runtime(
    state: &Arc<AppState>,
    root: &std::path::Path,
    emit: &impl Fn(&str),
) -> Result<(), String> {
    let _ = state.tts.irodori.shutdown().await;
    state
        .tts
        .irodori
        .wait_for_startup_sweep(|| emit("前回の実行の後始末（残ったサイドカーの掃除）が終わるのを待っています…"))
        .await;
    let client = state.tts.irodori.http_client();
    let stopped = crate::tts::sidecar::sweep_orphans(root, &client).await;
    if stopped > 0 {
        emit(&format!("前回の実行が残したサイドカーを {stopped} 件止めました"));
    }
    let live = crate::tts::sidecar::live_sidecars(root, &client).await;
    if live.is_empty() {
        return Ok(());
    }
    Err(why_the_runtime_cannot_be_replaced(&live))
}

/// 生きているサイドカーが残って入れ替えを始められないときの説明。**持ち主ごとに、することが違う。**
fn why_the_runtime_cannot_be_replaced(live: &[crate::tts::sidecar::LiveSidecar]) -> String {
    use crate::tts::sidecar::Holder;
    let ports = |holder: Holder| -> Vec<String> {
        live.iter()
            .filter(|s| s.holder == holder)
            .map(|s| s.port.to_string())
            .collect()
    };
    let mut reasons = Vec::new();
    let another = ports(Holder::AnotherUgg);
    if !another.is_empty() {
        reasons.push(format!(
            "ほかの ugg が Irodori を使っています（port {}）。そちらの ugg を終了してから",
            another.join(", ")
        ));
    }
    let orphans = ports(Holder::Nobody);
    if !orphans.is_empty() {
        reasons.push(format!(
            "前回の実行が残したサイドカーを止められませんでした（port {}。合成の途中で応答しないものを含みます）。PC を再起動してから",
            orphans.join(", ")
        ));
    }
    let mine = ports(Holder::Me);
    if !mine.is_empty() {
        reasons.push(format!(
            "この ugg のサイドカーが止まりませんでした（port {}）。ugg を再起動してから",
            mine.join(", ")
        ));
    }
    format!(
        "Irodori のサイドカーが動いているので、入れ替えを始めません（動いたまま入れ替えると、入れ替えも元に戻すことも失敗します）。{}、もう一度お試しください",
        reasons.join("。")
    )
}

/// Irodori 資産の導入状態 (v0.5.4 項目 2、spec §6.0)。
///
/// `irodori_assets_ready` が「**使えるか**」だけを返すのに対し、こちらは
/// 「**最新か**」まで返す。2 つを分けているのは、古いが動いているランタイムを
/// 「未導入」に落とすとフロントが `tts_irodori_use_real_model` を黙って倒して
/// 永続化してしまうため（spec §6.0 項目 2 の訂正を参照）。
#[tauri::command]
pub async fn get_irodori_status() -> Result<irodori_download::IrodoriStatus, String> {
    let root = voice_ref::irodori_root().map_err(|e| format!("{e:#}"))?;
    Ok(irodori_download::status(&root))
}

/// Irodori 資産 (Python embeddable / pip / torch / 共通依存) が揃っているか。
/// **「使えるか」だけを返す。** 最新かどうかは `get_irodori_status` が返す
/// （ここに「最新か」を混ぜると、古いが動いている環境の設定を壊す。spec §6.0 項目 2）。
#[tauri::command]
pub async fn irodori_assets_ready() -> bool {
    let Ok(root) = voice_ref::irodori_root() else {
        return false;
    };
    irodori_download::assets_ready(&root)
}

/// 「ランタイムをダウンロード」を押したときの行き先（v0.5.7 項目 1）。
#[derive(Debug, PartialEq, Eq)]
enum DownloadRoute {
    /// まだ入っていない。全段を入れる。
    FirstInstall,
    /// もう入っている（`assets_ready`）。変わったものだけを 1 つのトランザクションで入れ直す。
    Update,
}

fn download_route(present: bool) -> DownloadRoute {
    if present {
        DownloadRoute::Update
    } else {
        DownloadRoute::FirstInstall
    }
}

/// Irodori 用 Python ランタイム + 共通依存 + 実モデルランタイム + HF モデル本体の初回 DL
/// (architecture §8.2-8.3, M4c Phase C/G)。
/// 進捗は `irodori-download` イベントで 1 行ずつ emit、完了時に `"__done__"`。
///
/// 段取り:
///   1. Embeddable Python (Windows x64, 約 25MB) を DL → 展開 → `python._pth` 編集
///   2. `get-pip.py` で pip ブートストラップ
///   3. 共通依存 (fastapi / uvicorn / huggingface_hub / numpy / soundfile) を pip install
///   4. torch + torchaudio (CUDA 12.8) を pip install (1〜2GB)
///   5. Irodori-TTS ランタイム本体 + 追加 pip 依存 (transformers / peft / accelerate / silentcipher /
///      dacvae / irodori-tts) を pip install (~数百MB) — 実モデル経路で必要 (Phase G)
///   6. HF モデル本体 (Aratako/Irodori-TTS-500M-v3 + VoiceDesign + DACVAE Codec) を取得 (2〜4GB) —
///      サイドカー起動 hot path から切り離すため、ここで先に DL しておく
///
/// **導入済み（`assets_ready`）なら上の段取りは行わず、`update_irodori_runtime` に渡す**（v0.5.7 項目 1）。
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
    // **導入済みの環境では全段を入れ直さず、更新の経路へ振り向ける**（v0.5.7 項目 1、spec §6.0）。
    // この経路は全段を `--upgrade` で入れ直して最後に今の値を記録するだけで、版の控えも全戻しも
    // 合成のゲートも無い。導入済みの環境で押すと、依存の major 移行（transformers 4→5）が
    // 失敗しても戻らないまま起きる。更新の経路は同じ移行を 1 つのトランザクションで行う。
    if download_route(irodori_download::assets_ready(&asset_root)) == DownloadRoute::Update {
        return update_irodori_runtime(app, state).await.map(|_| ());
    }
    // 錠のファイルを置く前に作っておく（まっさらな端末ではまだ無い。反証レビュー #13）。
    std::fs::create_dir_all(&asset_root).map_err(|e| format!("資産ルート作成失敗: {e:#}"))?;
    // 更新と同時に走らせない（同じ site-packages を 2 経路が触る）。**もう 1 つの ugg とも**（v0.5.6 項目 3f）。
    let _busy =
        irodori_download::IrodoriBusyGuard::acquire_for(&asset_root).map_err(|e| format!("{e:#}"))?;

    let emit = {
        let app = app.clone();
        move |line: &str| {
            let _ = app.emit("irodori-download", line);
        }
    };

    let sidecar_py = asset_root.join("sidecar.py");
    // 始まりを残す（v0.5.7 リリース前監査。終わりは下の告知が理由つきで `[notify]` の行に残す）
    crate::ulog!("[irodori] ランタイムの導入を始めます");
    let result: Result<(), String> = async {
        // 更新と同じ入口の備え（v0.5.6 項目 3e）。**初回導入は自分のサイドカーを止めていなかった**
        // （更新の経路だけ止めていた）。Python が入っている環境で押し直すと、pip が掴まれた site-packages を触る。
        prepare_to_replace_runtime(state.inner(), &asset_root, &emit).await?;
        // 空きを確かめてから始める（v0.5.7 項目 8。取得は約 7 GB、入れた後は約 9 GB）
        irodori_download::check_free_space_for_first_install(&asset_root).map_err(|e| format!("{e:#}"))?;
        irodori_download::ensure_python_embeddable(&asset_root, &emit)
            .await
            .map_err(|e| format!("{e:#}"))?;
        irodori_download::ensure_pip(&asset_root, &emit)
            .await
            .map_err(|e| format!("{e:#}"))?;
        irodori_download::install_common_requirements(&asset_root, &emit)
            .await
            .map_err(|e| format!("{e:#}"))?;
        // 透かしの重みを先に取る（v0.5.7 項目 5）。huggingface_hub が入った後、pydub（透かしを効かせる）を
        // 入れる前。失敗しても導入は止めない（透かしが無くても合成はできる）。
        if let Err(err) = irodori_download::prefetch_watermark_weights(&asset_root, &emit) {
            irodori_download::report_watermark_prefetch_failure(&err, &emit);
        }
        irodori_download::install_torch_cuda(&asset_root, &emit)
            .await
            .map_err(|e| format!("{e:#}"))?;
        irodori_download::install_irodori_runtime(&asset_root, &emit)
            .await
            .map_err(|e| format!("{e:#}"))?;
        irodori_download::install_irodori_models(&asset_root, &sidecar_py, &emit)
            .await
            .map_err(|e| format!("{e:#}"))?;
        // **すべて成功したあとにだけ記録する** (v0.5.4 項目 1)。
        // 途中で失敗した状態に記録を残すと、次回「入っている」と誤認する。
        irodori_download::record_installed(&asset_root, &emit).map_err(|e| format!("{e:#}"))?;
        // 全部入れ直せたので、前回の更新の残り（戻しそびれた退避と版の控え）を片付ける（v0.5.6 リリース前監査）。
        // 残すと、次の更新の入口が、いま入れたものを古い退避と控えで書き戻す。
        irodori_download::discard_update_leftovers(&asset_root, &emit);
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

/// 参照音声を扱うゴースト = **読み込み中のゴースト**（v0.6.0 項目 2。引数は変えず、対象だけがゴーストごとになった）。
/// 読み込めていなければ、生成・削除はできない。
fn voice_ref_ghost(state: &AppState) -> Result<String, String> {
    state.loaded_ghost_id().ok_or_else(|| "ゴーストが読み込まれていないため、参照音声を扱えません".to_string())
}

/// 読み込み中のゴーストの参照音声一覧 (slot/caption/created_ts のみ、ファイルパスは隠す)。
#[tauri::command]
pub fn voice_ref_list(state: State<'_, Arc<AppState>>) -> Result<Vec<VoiceRef>, String> {
    let Some(ghost_id) = state.loaded_ghost_id() else {
        return Ok(Vec::new());
    };
    let rows = state
        .db
        .list_ghost_voice_refs(&ghost_id)
        .map_err(|e| format!("{e:#}"))?;
    Ok(rows.into_iter().map(VoiceRef::from).collect())
}

/// 読み込み中のゴーストの指定 slot の参照音声を削除 (DB 行 + .wav ファイル両方)。
#[tauri::command]
pub fn voice_ref_delete(
    slot: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<VoiceRef>, String> {
    if !matches!(slot.as_str(), "main" | "sub") {
        return Err(format!("未知の slot: {slot}"));
    }
    let ghost_id = voice_ref_ghost(&state)?;
    if let Some(row) = state
        .db
        .get_ghost_voice_ref(&ghost_id, &slot)
        .map_err(|e| format!("{e:#}"))?
    {
        let path = std::path::PathBuf::from(&row.file_path);
        if let Err(err) = voice_ref::delete_file(&path) {
            // ファイル削除失敗でも DB 行は削除する (パスがずれている場合の救済)。
            crate::ulog!("[voice_ref] file delete failed: {err:#}");
        }
    }
    state
        .db
        .delete_ghost_voice_ref(&ghost_id, &slot)
        .map_err(|e| format!("{e:#}"))?;
    voice_ref_list(state)
}

/// 参照音声の新規生成 (キャプション → Irodori サイドカーで .wav 生成 → DB upsert)。
/// 読み込み中のゴーストの同じ slot に既存があれば置き換え (UNIQUE(ghost_id, slot))、古い wav は削除する。
#[tauri::command]
pub async fn voice_ref_generate(
    slot: String,
    caption: String,
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Vec<VoiceRef>, String> {
    if !matches!(slot.as_str(), "main" | "sub") {
        return Err(format!("未知の slot: {slot}"));
    }
    let caption_trimmed = caption.trim().to_string();
    if caption_trimmed.is_empty() {
        return Err("キャプションが空です".to_string());
    }

    let ghost_id = voice_ref_ghost(&state)?;
    let asset_root = voice_ref::irodori_root().map_err(|e| format!("{e:#}"))?;
    let refs_dir = voice_ref::refs_dir().map_err(|e| format!("{e:#}"))?;

    // 新規 ID は (現時刻秒) を使い <slot>_<id>.wav とする (DB が後で id を発番するが
    // ファイル名先決→DB upsert の順で、UNIQUE(ghost_id, slot) で 1 行を更新する)。
    // wav の名前と場所はゴーストごとにしない（v0.6.0。既存の行は絶対パスで指し、更新の確かめは
    // 名前の先頭 main_ / sub_ で材料の順位を決めるので、変える利点が無い）。
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let out_path = voice_ref::ref_path_in_dir(&refs_dir, &slot, ts)
        .map_err(|e| format!("{e:#}"))?;

    // 既存ファイルがあれば事前削除 (上書きで構わないが、別 ID への切替時に残骸が残らないよう保険)。
    let previous = state
        .db
        .get_ghost_voice_ref(&ghost_id, &slot)
        .map_err(|e| format!("{e:#}"))?;

    let use_real = state
        .settings
        .lock()
        .expect("settings poisoned")
        .tts_irodori_use_real_model;

    // サイドカーへ生成依頼
    state
        .tts
        .irodori
        .generate_voice_ref(&asset_root, &caption_trimmed, &out_path, !use_real, Some(app))
        .await
        .map_err(|e| format!("{e}"))?;

    // DB upsert
    state
        .db
        .upsert_ghost_voice_ref(&ghost_id, &slot, &caption_trimmed, &out_path.to_string_lossy(), ts)
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
    app: AppHandle,
) -> Result<String, String> {
    if !matches!(slot.as_str(), "main" | "sub") {
        return Err(format!("未知の slot: {slot}"));
    }
    if text.trim().is_empty() {
        return Err("プレビュー文字列が空です".to_string());
    }
    let state_arc = state.inner().clone();
    let wav = synthesize_irodori(state_arc, &slot, &text, None, Some(app))
        .await
        .map_err(|e| format!("{e}"))?;
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

/// voicevox engine の遅延初期化。**lock を保持したまま init する** (二重 init 防止)。
///
/// 旧実装は「check (lock) → unlock → init → 再 lock で代入」の TOCTOU を抱えていた:
/// spawn_preinit と発話 (synthesize_voicevox / preprocess_for_irodori) が並行すると
/// `VoicevoxEngine::init` が二重に走り、後勝ち代入で負けた側の engine が drop される。
/// drop は libloading::Library の解放 = voicevox DLL のアンロードを伴うため、勝った側の
/// synthesizer が解放済みコードを指して onnxruntime.dll 内 0xc0000005 でプロセスごと落ちる
/// (実機で再現、起動直後に GPU 検出ログが 2 回出るのが二重 init の痕跡)。
///
/// init は数秒ブロックするが、待つ側もどうせ engine 完成までは合成できないので、
/// Mutex で直列化するのが正しい。本関数は常に blocking コンテキスト (spawn_blocking /
/// 同期 command) から呼ばれる前提。
fn ensure_engine(state: &Arc<AppState>) -> Result<(), String> {
    let mut guard = state.tts.voicevox.lock().expect("tts poisoned");
    if guard.is_some() {
        return Ok(());
    }
    let asset_dir = crate::state::voicevox_asset_dir().map_err(|e| format!("{e:#}"))?;
    if !voicevox::assets_ready(&asset_dir) {
        return Err("voicevox 資産が未ダウンロードです".to_string());
    }
    let engine = voicevox::VoicevoxEngine::init(&asset_dir)?;
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
/// `tauri::async_runtime::spawn_blocking` を使うこと: Tauri 2 の setup コールバックは
/// tokio ランタイムが thread-local に見えていないケースがあり、生の `tokio::task::spawn_blocking`
/// は "there is no reactor running" で panic することがある。
#[allow(dead_code)]
pub fn spawn_preinit(state: Arc<AppState>) {
    tauri::async_runtime::spawn_blocking(move || {
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

    /// **合成の失敗を、キャラが言う理由の種類へ**（v0.5.7 項目 7）。更新中・モデルが無い・VRAM 不足を言い分け、
    /// 分からないものは「その他」（以前は理由によらず「GPU 環境が整っていない」と言った）。どれも VOICEVOX へ落とす。
    #[test]
    fn a_failure_is_told_by_its_reason() {
        use crate::tts::irodori::SynthFailure;
        let synth = |kind| TtsError::Synth { kind, message: "x".into() };
        assert_eq!(unavailable_kind(&TtsError::Updating), UnavailableKind::Updating);
        assert_eq!(unavailable_kind(&synth(SynthFailure::ModelMissing)), UnavailableKind::ModelMissing);
        assert_eq!(unavailable_kind(&synth(SynthFailure::OutOfMemory)), UnavailableKind::Vram);
        assert_eq!(unavailable_kind(&synth(SynthFailure::Other)), UnavailableKind::Other);
        assert_eq!(unavailable_kind(&TtsError::SidecarStart("x".into())), UnavailableKind::Other);
        assert_eq!(unavailable_kind(&TtsError::Http("x".into())), UnavailableKind::Other);
        for err in [TtsError::Updating, synth(SynthFailure::ModelMissing), synth(SynthFailure::OutOfMemory)] {
            assert_eq!(decide_fallback(&err), FallbackAction::FallbackToVoicevox, "{err}");
        }
        // 告知には理由の種類を渡している（本文のテキスト）
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/tts.rs"),
        )
        .unwrap()
        .replace("\r\n", "\n");
        // コマンドの本文だけを見る（このテスト自身が同じ文字列を持つので、ファイル全体だと自分に当たる）
        let body = &src[src.find("pub async fn synthesize_voice(").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        assert!(body.contains("kind: unavailable_kind(&err),"), "告知に理由の種類を渡していない");
    }

    /// **導入済みの環境で「ランタイムをダウンロード」を押しても、全段は入れ直さない**（v0.5.7 項目 1）。
    /// 全段の入れ直しには版の控えも全戻しも合成のゲートも無いので、依存の major 移行が失敗しても戻らない。
    #[test]
    fn an_installed_runtime_is_sent_to_the_update() {
        assert_eq!(download_route(true), DownloadRoute::Update);
        assert_eq!(download_route(false), DownloadRoute::FirstInstall);
    }

    /// **環境を書き換える処理は、始まりを `ugg.log` に残す。更新は終わりも残す**（v0.5.7 リリース前監査）。進捗は画面に
    /// しか出ないので、E-11 で更新が pip の途中で止まったことが site-packages の日時からしか分からなかった
    /// （配線を本文のテキストで固定する。コマンドは AppHandle が要るので単体では呼べない）。
    #[test]
    fn rewrites_log_their_start_and_the_update_its_end() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/tts.rs"),
        )
        .unwrap()
        .replace("\r\n", "\n");
        let body_of = |name: &str| {
            let body = &src[src.find(name).unwrap_or_else(|| panic!("{name} が無い"))..];
            body[..body.find("\n}\n").unwrap()].to_string()
        };
        let upd = body_of("pub async fn update_irodori_runtime(");
        let start = upd.find("crate::ulog!(\"[irodori] 更新を始めます（対象: {}）\"").expect("更新の始まりを残していない");
        let call = upd
            .find("irodori_download::update_irodori_runtime(&root, &targets, &emit).await;")
            .expect("更新を呼んでいない");
        let ok = upd.find("crate::ulog!(\"[irodori] 更新が終わりました").expect("成功の終わりを残していない");
        let err = upd.find("crate::ulog!(\"[irodori] 更新が終わりませんでした").expect("失敗の終わりを残していない");
        assert!(start < call && call < ok && call < err, "始まりと終わりの位置");
        for (name, line) in [
            ("pub async fn download_irodori_assets", "crate::ulog!(\"[irodori] ランタイムの導入を始めます\")"),
            ("pub async fn download_voicevox_assets(", "crate::ulog!(\"[voicevox] 資産の取得を始めます\")"),
        ] {
            assert!(body_of(name).contains(line), "{name} が始まりを残していない");
        }
    }

    /// 振り分けは**最初の入れる段より前**にあり、導入済みなら更新のコマンドへ渡す（配線を本文のテキストで固定する。
    /// コマンドは AppHandle が要るので単体では呼べない）。
    #[test]
    fn the_download_command_routes_before_installing_anything() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/tts.rs"),
        )
        .unwrap()
        .replace("\r\n", "\n");
        let name = "pub async fn download_irodori_assets";
        let body = &src[src.find(name).unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let routed = body
            .find("download_route(irodori_download::assets_ready(")
            .expect("導入済みかを見ていない");
        let to_update = body
            .find("return update_irodori_runtime(")
            .expect("導入済みのとき更新のコマンドへ渡していない");
        for step in [
            "IrodoriBusyGuard::acquire_for(",
            "prepare_to_replace_runtime(",
            "ensure_python_embeddable(",
            "install_common_requirements(",
            "install_torch_cuda(",
            "install_irodori_runtime(",
        ] {
            let at = body.find(step).unwrap_or_else(|| panic!("{step} が無い"));
            assert!(routed < at && to_update < at, "{step} より後で振り分けている");
        }
    }

    /// 入れ替えを始められないときの説明は、**持ち主ごとに、することを分けて言う**（v0.5.6 項目 4）。
    /// ほかの ugg のものは「そちらを終了」、止められなかった孤児は「PC を再起動」。
    #[test]
    fn the_refusal_says_what_to_do_for_each_holder() {
        use crate::tts::sidecar::{Holder, LiveSidecar};
        let another = LiveSidecar { port: 50001, holder: Holder::AnotherUgg };
        let orphan = LiveSidecar { port: 50002, holder: Holder::Nobody };

        let msg = why_the_runtime_cannot_be_replaced(&[another]);
        assert!(msg.contains("ほかの ugg") && msg.contains("50001") && msg.contains("終了"), "{msg}");
        assert!(!msg.contains("再起動"), "ほかの ugg のものに PC の再起動は要らない: {msg}");

        let msg = why_the_runtime_cannot_be_replaced(&[orphan]);
        assert!(msg.contains("前回の実行") && msg.contains("50002") && msg.contains("PC を再起動"), "{msg}");
        assert!(!msg.contains("ほかの ugg"), "{msg}");

        let msg = why_the_runtime_cannot_be_replaced(&[another, orphan]);
        assert!(msg.contains("50001") && msg.contains("50002"), "両方を言う: {msg}");
    }

    /// **声はゴーストごと、引くのは読み込み中のゴースト**（v0.6.0 項目 2）。`settings.ghost_id` は切り替えて再起動する
    /// までの間に先に変わるので、それで引くと旧ゴーストが新ゴーストの声で喋る。VOICEVOX の話者も参照音声も同じ
    /// （配線を本文のテキストで固定する。コマンドは AppHandle / State が要るので単体では呼べない）。
    #[test]
    fn the_voice_is_looked_up_by_the_loaded_ghost() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/tts.rs"),
        )
        .unwrap()
        .replace("\r\n", "\n");
        let body_of = |name: &str| {
            let body = &src[src.find(name).unwrap_or_else(|| panic!("{name} が無い"))..];
            body[..body.find("\n}\n").unwrap()].to_string()
        };
        let voicevox = body_of("async fn synthesize_voicevox(");
        assert!(
            voicevox.contains("settings.speakers_for(state.loaded_ghost_id().as_deref())"),
            "話者を読み込み中のゴーストの行から引いていない"
        );
        assert!(
            !voicevox.contains("tts_speaker_main") && !voicevox.contains("tts_speaker_sub"),
            "移行の読み出し元（tts_speaker_main/sub）を合成で読んでいる"
        );
        for (name, lookup) in [
            ("async fn synthesize_irodori(", "state.loaded_ghost_id()"),
            ("fn voice_ref_ghost(", "state.loaded_ghost_id()"),
            ("pub fn voice_ref_list(", "state.loaded_ghost_id()"),
            ("pub fn voice_ref_delete(", "voice_ref_ghost(&state)"),
            ("pub async fn voice_ref_generate(", "voice_ref_ghost(&state)"),
        ] {
            let body = body_of(name);
            assert!(body.contains(lookup), "{name} が読み込み中のゴーストを引いていない");
        }
        for name in [
            "async fn synthesize_voicevox(",
            "async fn synthesize_irodori(",
            "pub fn voice_ref_list(",
            "pub fn voice_ref_delete(",
            "pub async fn voice_ref_generate(",
        ] {
            let body = body_of(name);
            assert!(!body.contains("settings.ghost_id"), "{name} が settings.ghost_id で引いている");
            // 旧テーブル `voice_refs` は新版が読み書きしない（export だけが読む）
            assert!(
                !body.contains("get_voice_ref(") && !body.contains("upsert_voice_ref(") && !body.contains("delete_voice_ref("),
                "{name} が旧 voice_refs を触っている"
            );
        }
    }

    /// **参照音声が無い slot は、通常の合成では VOICEVOX へ回り（無言にしない）、プレビューはエラーのまま**
    /// （v0.6.0 項目 3）。プレビューは作ったばかりの声を聴く道具で、代わりの声で鳴らすと誤解させる。
    #[test]
    fn a_missing_voice_ref_goes_to_voicevox_but_the_preview_stays_an_error() {
        let err = TtsError::VoiceRefMissing("main".into());
        assert_eq!(decide_fallback(&err), FallbackAction::FallbackForMissingRef);
        // 故障（サイドカー・HTTP ほか）の VOICEVOX 代用とは別の扱い（告知が違う）
        assert_ne!(
            decide_fallback(&err),
            decide_fallback(&TtsError::SidecarStart("x".into()))
        );

        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/tts.rs"),
        )
        .unwrap()
        .replace("\r\n", "\n");
        let body_of = |name: &str| {
            let body = &src[src.find(name).unwrap_or_else(|| panic!("{name} が無い"))..];
            body[..body.find("\n}\n").unwrap()].to_string()
        };
        // プレビューは Irodori の合成をそのまま呼んでエラーで返す（フォールバックの判定も VOICEVOX も通らない）
        let preview = body_of("pub async fn voice_ref_preview(");
        assert!(preview.contains("synthesize_irodori("), "プレビューが Irodori の合成を呼んでいない");
        assert!(
            preview.contains(".map_err(|e| format!(\"{e}\"))?"),
            "プレビューが合成のエラーをエラーのまま返していない"
        );
        for banned in ["decide_fallback", "synthesize_voicevox", "FallbackAction", "announce_"] {
            assert!(!preview.contains(banned), "プレビューが {banned} を通っている（代わりの声で鳴らさない）");
        }
        // 通常の合成は VoiceRefMissing の枝で VOICEVOX を呼んで告げる。irodori_unavailable の経路（5 分の待ち・
        // IrodoriUnavailable）は共有しない
        let synth = body_of("pub async fn synthesize_voice(");
        let from = synth
            .find("FallbackAction::FallbackForMissingRef =>")
            .expect("VoiceRefMissing の枝が無い");
        let to = synth
            .find("FallbackAction::FallbackToVoicevox =>")
            .expect("故障の枝が無い");
        assert!(from < to, "枝の並びが想定と違う");
        let missing = &synth[from..to];
        assert!(missing.contains("synthesize_voicevox("), "VOICEVOX で喋っていない");
        assert!(missing.contains("announce_voice_ref_missing_once("), "1 回だけ伝えていない");
        for banned in ["should_notify_unavailable", "IrodoriUnavailable", "unavailable_kind"] {
            assert!(!missing.contains(banned), "参照音声が無いだけなのに {banned} を通っている");
        }
        assert!(!synth.contains("ReturnError"), "エラーで返す枝が残っている");
        // 戻りの engine は実際に鳴らしたもの（クレジットの根拠）。VOICEVOX で鳴らした枝は VOICEVOX、Irodori は Irodori
        assert!(missing.contains("(wav, ENGINE_VOICEVOX)"), "代用で鳴らしたのに VOICEVOX と返していない");
        assert!(!missing.contains("ENGINE_IRODORI"));
        let failure = &synth[to..];
        assert!(failure.contains("(wav, ENGINE_VOICEVOX)"), "故障の代用が VOICEVOX と返していない");
        assert!(!failure.contains("ENGINE_IRODORI"));
        assert!(synth.contains("Ok(wav) => (wav, ENGINE_IRODORI)"), "Irodori で鳴らしたのに Irodori と返していない");
        assert!(synth.contains("            ENGINE_VOICEVOX,\n        ),"), "VOICEVOX 設定の合成が VOICEVOX と返していない");
    }

    /// **参照音声の告知は、ゴースト × slot の鍵で取り合い、届いたときだけ済みにする**（v0.6.0 項目 3）。
    /// 実物は `once_reached_claimed` が行う（振る舞いは notify.rs のテストで固定）。ここは配線。
    #[test]
    fn the_voice_ref_notice_is_claimed_per_ghost_and_slot() {
        assert_ne!(voice_ref_missing_key("a", "main"), voice_ref_missing_key("a", "sub"));
        assert_ne!(voice_ref_missing_key("a", "main"), voice_ref_missing_key("b", "main"));
        assert_eq!(voice_ref_missing_key("a", "main"), voice_ref_missing_key("a", "main"));

        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/tts.rs"),
        )
        .unwrap()
        .replace("\r\n", "\n");
        let body_of = |name: &str| {
            let body = &src[src.find(name).unwrap_or_else(|| panic!("{name} が無い"))..];
            body[..body.find("\n}\n").unwrap()].to_string()
        };
        let body = body_of("pub(crate) async fn announce_voice_ref_missing_once(");
        let claimed = body.find("once_reached_claimed(").expect("取り合いを通していない");
        let shown = body
            .find("NoticeKind::IrodoriVoiceRefMissing")
            .expect("参照音声の告知を出していない");
        assert!(claimed < shown, "取る前に告知を出している");
        assert!(body.contains("voice_ref_missing_key(&ghost_id, slot)"), "ゴースト × slot の鍵を使っていない");
        assert!(body.contains("state.loaded_ghost_id()"), "読み込み中のゴーストで引いていない");
        assert!(!body.contains("settings.ghost_id"), "設定の ghost_id で引いている");
        // 取る側の中で 5 分の待ちには触らない
        assert!(!body.contains("should_notify_unavailable"), "5 分の待ちを共有している");
    }

    /// **声を決めていないゴーストの案内**（v0.6.0 項目 2）。行が無いときだけ・ゴーストごとに 1 回・
    /// 届いたときだけ済み（`app_settings` に記録）。実物の DB で流す。
    #[tokio::test]
    async fn the_unset_voice_hint_is_once_per_ghost_and_only_when_reached() {
        use crate::db::Db;
        use crate::state::GhostSpeakers;
        use crate::system::notify::{NoticeOutcome, OnceGate};

        let db = Db::open(std::path::Path::new(":memory:")).unwrap();
        db.migrate().unwrap();
        let gate = OnceGate::default();
        let mut settings = Settings::default();
        settings.tts_speakers_by_ghost.clear();
        let shown = std::cell::Cell::new(0);
        let tell = |outcome: NoticeOutcome| {
            shown.set(shown.get() + 1);
            async move { outcome }
        };

        // 隠している間（保留）は済みにしない。記録も gate も残さず、次の機会にまた出る
        let held = ghost_voice_unset_once(&db, &gate, &settings, "g1", || tell(NoticeOutcome::Held)).await;
        assert_eq!(held, Some(NoticeOutcome::Held));
        assert_eq!(db.get_setting(&ghost_voice_hint_key("g1")).unwrap(), None, "保留で済みにした");
        // 失敗も同じ
        let failed = ghost_voice_unset_once(&db, &gate, &settings, "g1", || tell(NoticeOutcome::Failed)).await;
        assert_eq!(failed, Some(NoticeOutcome::Failed));
        assert_eq!(db.get_setting(&ghost_voice_hint_key("g1")).unwrap(), None, "失敗で済みにした");

        // 届いたら出て、app_settings に記録する
        let reached = ghost_voice_unset_once(&db, &gate, &settings, "g1", || tell(NoticeOutcome::Shown)).await;
        assert_eq!(reached, Some(NoticeOutcome::Shown));
        assert_eq!(
            db.get_setting(&ghost_voice_hint_key("g1")).unwrap().as_deref(),
            Some("1"),
            "届いたのに記録していない"
        );
        // 済みなら出さない
        let again = ghost_voice_unset_once(&db, &gate, &settings, "g1", || tell(NoticeOutcome::Shown)).await;
        assert_eq!(again, None);
        // 再起動（gate が空）しても、記録があれば出ない
        let after_restart =
            ghost_voice_unset_once(&db, &OnceGate::default(), &settings, "g1", || tell(NoticeOutcome::Shown)).await;
        assert_eq!(after_restart, None, "再起動すると出る");
        assert_eq!(shown.get(), 3, "出した回数（保留・失敗・届いたの 3 回だけ。済みと再起動後は出さない）");

        // ゴーストごと: 別のゴーストにはまた出る
        let other = ghost_voice_unset_once(&db, &gate, &settings, "g2", || tell(NoticeOutcome::Shown)).await;
        assert_eq!(other, Some(NoticeOutcome::Shown));

        // 声を決めたゴースト（行がある）には出ない。記録もしない
        settings
            .tts_speakers_by_ghost
            .insert("g3".into(), GhostSpeakers { main: 5, sub: 6 });
        let decided = ghost_voice_unset_once(&db, &gate, &settings, "g3", || tell(NoticeOutcome::Shown)).await;
        assert_eq!(decided, None);
        assert_eq!(db.get_setting(&ghost_voice_hint_key("g3")).unwrap(), None);
    }

    /// **案内の判定は、行・記録のどちらかがあれば出さない**（純粋部分）。移行で行が埋まる既存のゴースト
    /// （`from_persisted`）には出ない。
    #[test]
    fn the_hint_is_due_only_without_a_row_and_a_record() {
        use crate::state::GhostSpeakers;
        let mut s = Settings::default();
        s.tts_speakers_by_ghost.clear();
        assert!(ghost_voice_hint_due(&s, "a", false));
        assert!(!ghost_voice_hint_due(&s, "a", true), "記録があるのに出す");
        s.tts_speakers_by_ghost.insert("a".into(), GhostSpeakers { main: 2, sub: 3 });
        assert!(!ghost_voice_hint_due(&s, "a", false), "行があるのに出す");
        assert!(ghost_voice_hint_due(&s, "b", false), "別のゴーストの行で引いている");
        // 旧版の設定を読んだ（`from_persisted` が現在のゴーストの行を埋める）既存のゴーストには出ない
        let raw = serde_json::to_string(&Settings::default()).unwrap();
        let migrated = Settings::from_persisted(&raw).unwrap();
        assert!(!ghost_voice_hint_due(&migrated, &migrated.ghost_id, false), "移行で行が埋まるゴーストに出す");
    }

    /// **配線: VOICEVOX で鳴らしたら案内を試みる**（フォールバックを含む。全経路が `engine` の判定 1 か所を通る）。
    /// 案内は告知を出す関数に取り合いと記録を任せ、済みの記録は `once_reached_claimed` の後ろ（届いたとき）にある。
    #[test]
    fn the_unset_voice_hint_is_wired_after_voicevox_speaks_and_marked_after_reaching() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/tts.rs"),
        )
        .unwrap()
        .replace("\r\n", "\n");
        let body_of = |name: &str| {
            let body = &src[src.find(name).unwrap_or_else(|| panic!("{name} が無い"))..];
            body[..body.find("\n}\n").unwrap()].to_string()
        };
        let synth = body_of("pub async fn synthesize_voice(");
        let judged = synth.find("if engine == ENGINE_VOICEVOX {").expect("鳴らしたエンジンで案内を判定していない");
        let announced = synth.find("announce_ghost_voice_unset_once(").expect("案内を呼んでいない");
        let returned = synth.find("Ok(SynthesizedVoice {").expect("戻りが無い");
        assert!(judged < announced && announced < returned);
        assert_eq!(
            synth.matches("announce_ghost_voice_unset_once(").count(),
            1,
            "案内の呼び出しが経路ごとに散っている"
        );
        // 告知を出す側（AppHandle が要る）は NoticeKind::GhostVoiceUnset を出す
        let announce = body_of("pub(crate) async fn announce_ghost_voice_unset_once(");
        assert!(announce.contains("NoticeKind::GhostVoiceUnset"));
        assert!(announce.contains("state.loaded_ghost_id()"), "読み込み中のゴーストで引いていない");
        // 記録は届いたかを見る関数（once_reached_claimed）の mark の中だけ
        let core = body_of("async fn ghost_voice_unset_once<Fut>(");
        let claimed = core.find("once_reached_claimed(").expect("取り合いを通していない");
        let marked = core.find("set_setting(&seen_key").expect("記録していない");
        assert!(claimed < marked, "届いたかを見る前に済みにしている");
        assert_eq!(core.matches("set_setting(").count(), 1);
        assert!(!announce.contains("set_setting("), "出す側が記録を直接書いている");
    }

    #[test]
    fn decide_fallback_sidecar_start_falls_back() {
        let err = TtsError::SidecarStart("python.exe not found".into());
        assert_eq!(decide_fallback(&err), FallbackAction::FallbackToVoicevox);
    }

    #[test]
    fn decide_fallback_http_falls_back() {
        let err = TtsError::Http("503".into());
        assert_eq!(decide_fallback(&err), FallbackAction::FallbackToVoicevox);
    }

    #[test]
    fn decide_fallback_not_implemented_falls_back() {
        let err = TtsError::NotImplemented;
        assert_eq!(decide_fallback(&err), FallbackAction::FallbackToVoicevox);
    }

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

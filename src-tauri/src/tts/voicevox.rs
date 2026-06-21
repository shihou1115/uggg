//! voicevox_core 0.16 のプリビルド C API (`voicevox_core.dll`) を libloading で
//! 実行時ロードしてプロセス内で音声合成する。
//!
//! 資産配置: `%APPDATA%\ugg\voicevox\` に dll / onnxruntime / open_jtalk_dic / *.vvm。
//! 取得は M4-A4 の `download` モジュールが公式 download ツール経由で行う。
//!
//! 設計判断:
//! - CPU 強制 (`acceleration_mode = 1`)。GPU 経路は依存 DLL 未配布で AV 検出リスクあり、
//!   かつ「軽量常駐 + CPU で全機能」が spec の要件 (§1.3)。
//! - 関数ポインタは Library から取り出して所有値で持つ (Symbol の借用を残さない)。
//! - Mutex で直列化される前提なので `unsafe impl Send`。
//!
//! 参考: v0.0.3 (`C:\claude\ugga\src-tauri\src\voicevox_ffi.rs`) と signatures は同一。

use std::ffi::{c_char, c_void, CString};
use std::path::{Path, PathBuf};

use libloading::{Library, Symbol};

// ===== 公開ヘルパ =====

/// `%APPDATA%\ugg\voicevox\` 配下に必須資産が揃っているか。
pub fn assets_ready(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let core = find_file(dir, &|n| n.eq_ignore_ascii_case("voicevox_core.dll")).is_some();
    let ort = find_file(dir, &|n| {
        let l = n.to_ascii_lowercase();
        l.contains("onnxruntime") && l.ends_with(".dll")
    })
    .is_some();
    let dict = find_dir(dir, &|n| n.to_ascii_lowercase().starts_with("open_jtalk_dic")).is_some();
    let mut vvms = Vec::new();
    collect_files(dir, &|n| n.to_ascii_lowercase().ends_with(".vvm"), &mut vvms);
    core && ort && dict && !vvms.is_empty()
}

// ===== C API シグネチャ (voicevox_core 0.16.x に対応) =====

type VoicevoxResultCode = i32;
const VOICEVOX_RESULT_OK: VoicevoxResultCode = 0;

#[repr(C)]
struct VoicevoxLoadOnnxruntimeOptions {
    filename: *const c_char,
}

#[repr(C)]
struct VoicevoxInitializeOptions {
    /// 0=AUTO / 1=CPU / 2=GPU
    acceleration_mode: i32,
    cpu_num_threads: u16,
}

#[repr(C)]
struct VoicevoxTtsOptions {
    enable_interrogative_upspeak: bool,
}

type OnnxruntimePtr = *const c_void;
type OpenJtalkPtr = *mut c_void;
type SynthesizerPtr = *mut c_void;
type VoiceModelPtr = *mut c_void;

type FnMakeLoadOrtOpts = unsafe extern "C" fn() -> VoicevoxLoadOnnxruntimeOptions;
type FnOrtLoadOnce =
    unsafe extern "C" fn(VoicevoxLoadOnnxruntimeOptions, *mut OnnxruntimePtr) -> VoicevoxResultCode;
type FnOpenJtalkNew = unsafe extern "C" fn(*const c_char, *mut OpenJtalkPtr) -> VoicevoxResultCode;
type FnOpenJtalkDelete = unsafe extern "C" fn(OpenJtalkPtr);
type FnMakeInitOpts = unsafe extern "C" fn() -> VoicevoxInitializeOptions;
type FnSynthNew = unsafe extern "C" fn(
    OnnxruntimePtr,
    OpenJtalkPtr,
    VoicevoxInitializeOptions,
    *mut SynthesizerPtr,
) -> VoicevoxResultCode;
type FnSynthDelete = unsafe extern "C" fn(SynthesizerPtr);
type FnModelOpen = unsafe extern "C" fn(*const c_char, *mut VoiceModelPtr) -> VoicevoxResultCode;
type FnLoadModel = unsafe extern "C" fn(SynthesizerPtr, VoiceModelPtr) -> VoicevoxResultCode;
type FnModelDelete = unsafe extern "C" fn(VoiceModelPtr);
type FnMakeTtsOpts = unsafe extern "C" fn() -> VoicevoxTtsOptions;
type FnTts = unsafe extern "C" fn(
    SynthesizerPtr,
    *const c_char,
    u32, // VoicevoxStyleId
    VoicevoxTtsOptions,
    *mut usize,
    *mut *mut u8,
) -> VoicevoxResultCode;
type FnWavFree = unsafe extern "C" fn(*mut u8);
type FnMetasJson = unsafe extern "C" fn(SynthesizerPtr) -> *mut c_char;
type FnJsonFree = unsafe extern "C" fn(*mut c_char);
type FnOpenJtalkAnalyze =
    unsafe extern "C" fn(OpenJtalkPtr, *const c_char, *mut *mut c_char) -> VoicevoxResultCode;

// ===== Engine =====

pub struct VoicevoxEngine {
    lib: Library,
    synthesizer: SynthesizerPtr,
    open_jtalk: OpenJtalkPtr,
    loaded_models: usize,
}

// Mutex で直列化する前提で Send。
unsafe impl Send for VoicevoxEngine {}

impl VoicevoxEngine {
    /// 資産ディレクトリ配下から DLL / onnxruntime / 辞書 / *.vvm をロードして初期化する。
    pub fn init(asset_dir: &Path) -> Result<Self, String> {
        if !asset_dir.is_dir() {
            return Err(format!(
                "voicevox 資産ディレクトリがありません: {}",
                asset_dir.display()
            ));
        }
        let core_dll = find_file(asset_dir, &|n| n.eq_ignore_ascii_case("voicevox_core.dll"))
            .ok_or_else(|| {
                "voicevox_core.dll が見つかりません (資産ダウンロードを実行してください)"
                    .to_string()
            })?;
        let lib = unsafe { Library::new(&core_dll) }
            .map_err(|e| format!("voicevox_core.dll のロードに失敗: {e}"))?;

        unsafe {
            let make_ort_opts: FnMakeLoadOrtOpts =
                fptr(&lib, b"voicevox_make_default_load_onnxruntime_options\0")?;
            let ort_load: FnOrtLoadOnce = fptr(&lib, b"voicevox_onnxruntime_load_once\0")?;
            let oj_new: FnOpenJtalkNew = fptr(&lib, b"voicevox_open_jtalk_rc_new\0")?;
            let oj_delete: FnOpenJtalkDelete = fptr(&lib, b"voicevox_open_jtalk_rc_delete\0")?;
            let make_init: FnMakeInitOpts =
                fptr(&lib, b"voicevox_make_default_initialize_options\0")?;
            let synth_new: FnSynthNew = fptr(&lib, b"voicevox_synthesizer_new\0")?;
            let synth_delete: FnSynthDelete = fptr(&lib, b"voicevox_synthesizer_delete\0")?;
            let model_open: FnModelOpen = fptr(&lib, b"voicevox_voice_model_file_open\0")?;
            let model_load: FnLoadModel = fptr(&lib, b"voicevox_synthesizer_load_voice_model\0")?;
            let model_delete: FnModelDelete = fptr(&lib, b"voicevox_voice_model_file_delete\0")?;

            // ONNX Runtime ロード (パスを filename に指定可)
            let ort_dll = find_file(asset_dir, &|n| {
                let l = n.to_ascii_lowercase();
                l.contains("onnxruntime") && l.ends_with(".dll")
            });
            let ort_path_c = ort_dll
                .as_ref()
                .and_then(|p| p.to_str())
                .and_then(|s| CString::new(s).ok());
            let mut opts = make_ort_opts();
            if let Some(c) = &ort_path_c {
                opts.filename = c.as_ptr();
            }
            let mut ort: OnnxruntimePtr = std::ptr::null();
            check(ort_load(opts, &mut ort), "ONNX Runtime のロード")?;
            drop(ort_path_c);

            // Open JTalk 辞書
            let dict_dir = find_dir(asset_dir, &|n| {
                n.to_ascii_lowercase().starts_with("open_jtalk_dic")
            })
            .ok_or_else(|| "Open JTalk 辞書 (open_jtalk_dic_*) が見つかりません".to_string())?;
            let dict_c = CString::new(dict_dir.to_string_lossy().as_ref())
                .map_err(|e| format!("辞書パスが不正: {e}"))?;
            let mut open_jtalk: OpenJtalkPtr = std::ptr::null_mut();
            check(
                oj_new(dict_c.as_ptr(), &mut open_jtalk),
                "Open JTalk 辞書の読み込み",
            )?;

            // Synthesizer (CPU 強制)
            let mut init_opts = make_init();
            init_opts.acceleration_mode = 1;
            let mut synth: SynthesizerPtr = std::ptr::null_mut();
            let rc = synth_new(ort, open_jtalk, init_opts, &mut synth);
            if rc != VOICEVOX_RESULT_OK {
                oj_delete(open_jtalk);
                return Err(format!("Synthesizer の生成に失敗 (コード {rc})"));
            }

            // 全 *.vvm をロード
            let mut vvms = Vec::new();
            collect_files(
                asset_dir,
                &|n| n.to_ascii_lowercase().ends_with(".vvm"),
                &mut vvms,
            );
            let mut loaded = 0usize;
            for vvm in &vvms {
                let Ok(path_c) = CString::new(vvm.to_string_lossy().as_ref()) else {
                    continue;
                };
                let mut model: VoiceModelPtr = std::ptr::null_mut();
                if model_open(path_c.as_ptr(), &mut model) != VOICEVOX_RESULT_OK {
                    continue;
                }
                if model_load(synth, model) == VOICEVOX_RESULT_OK {
                    loaded += 1;
                }
                model_delete(model);
            }
            if loaded == 0 {
                synth_delete(synth);
                oj_delete(open_jtalk);
                return Err("音声モデル (.vvm) を 1 つも読み込めませんでした".to_string());
            }

            Ok(VoicevoxEngine {
                lib,
                synthesizer: synth,
                open_jtalk,
                loaded_models: loaded,
            })
        }
    }

    pub fn loaded_models(&self) -> usize {
        self.loaded_models
    }

    /// 話者/スタイル一覧の JSON (`/speakers` 同形式)。
    pub fn metas_json(&self) -> Result<String, String> {
        unsafe {
            let metas: FnMetasJson = fptr(&self.lib, b"voicevox_synthesizer_create_metas_json\0")?;
            let json_free: FnJsonFree = fptr(&self.lib, b"voicevox_json_free\0")?;
            let p = metas(self.synthesizer);
            if p.is_null() {
                return Err("メタ情報の取得に失敗".to_string());
            }
            let s = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
            json_free(p);
            Ok(s)
        }
    }

    /// 指定 style_id でテキストを合成し WAV バイト列を返す。
    /// speed/volume は voicevox_core の TTS API では直接扱えないため、
    /// 後工程の Web Audio 側 (playbackRate / GainNode) で適用する。
    pub fn synthesize(&self, text: &str, style_id: u32) -> Result<Vec<u8>, String> {
        let text_c = CString::new(text).map_err(|e| format!("テキストが不正: {e}"))?;
        unsafe {
            let make_tts: FnMakeTtsOpts = fptr(&self.lib, b"voicevox_make_default_tts_options\0")?;
            let tts: FnTts = fptr(&self.lib, b"voicevox_synthesizer_tts\0")?;
            let wav_free: FnWavFree = fptr(&self.lib, b"voicevox_wav_free\0")?;

            let opts = make_tts();
            let mut len: usize = 0;
            let mut wav: *mut u8 = std::ptr::null_mut();
            check(
                tts(
                    self.synthesizer,
                    text_c.as_ptr(),
                    style_id,
                    opts,
                    &mut len,
                    &mut wav,
                ),
                "音声合成",
            )?;
            if wav.is_null() || len == 0 {
                return Err("合成結果が空でした".to_string());
            }
            let bytes = std::slice::from_raw_parts(wav, len).to_vec();
            wav_free(wav);
            Ok(bytes)
        }
    }

    /// Open JTalk の analyze: テキストを AccentPhrase JSON に変換する。
    /// M4b の漢字→ひらがな前処理 (Irodori 用) で使う。
    pub fn openjtalk_analyze(&self, text: &str) -> Result<String, String> {
        let text_c = CString::new(text).map_err(|e| format!("テキストが不正: {e}"))?;
        unsafe {
            let analyze: FnOpenJtalkAnalyze = fptr(&self.lib, b"voicevox_open_jtalk_rc_analyze\0")?;
            let json_free: FnJsonFree = fptr(&self.lib, b"voicevox_json_free\0")?;
            let mut json: *mut c_char = std::ptr::null_mut();
            check(analyze(self.open_jtalk, text_c.as_ptr(), &mut json), "解析")?;
            if json.is_null() {
                return Err("解析結果が空".to_string());
            }
            let s = std::ffi::CStr::from_ptr(json).to_string_lossy().into_owned();
            json_free(json);
            Ok(s)
        }
    }
}

impl Drop for VoicevoxEngine {
    fn drop(&mut self) {
        unsafe {
            if let Ok(f) = fptr::<FnSynthDelete>(&self.lib, b"voicevox_synthesizer_delete\0") {
                f(self.synthesizer);
            }
            if let Ok(f) = fptr::<FnOpenJtalkDelete>(&self.lib, b"voicevox_open_jtalk_rc_delete\0") {
                f(self.open_jtalk);
            }
        }
    }
}

// ===== 内部ユーティリティ =====

unsafe fn fptr<T: Copy>(lib: &Library, name: &[u8]) -> Result<T, String> {
    let s: Symbol<T> = lib
        .get::<T>(name)
        .map_err(|e| format!("関数 {} が見つかりません: {e}", String::from_utf8_lossy(name)))?;
    Ok(*s)
}

fn check(code: VoicevoxResultCode, what: &str) -> Result<(), String> {
    if code == VOICEVOX_RESULT_OK {
        Ok(())
    } else {
        Err(format!("{what}に失敗しました (voicevox コード {code})"))
    }
}

fn find_file(dir: &Path, pred: &dyn Fn(&str) -> bool) -> Option<PathBuf> {
    let mut found = None;
    visit(dir, &mut |p| {
        if found.is_none() && p.is_file() {
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if pred(name) {
                    found = Some(p.to_path_buf());
                }
            }
        }
    });
    found
}

fn find_dir(dir: &Path, pred: &dyn Fn(&str) -> bool) -> Option<PathBuf> {
    let mut found = None;
    visit(dir, &mut |p| {
        if found.is_none() && p.is_dir() {
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if pred(name) {
                    found = Some(p.to_path_buf());
                }
            }
        }
    });
    found
}

fn collect_files(dir: &Path, pred: &dyn Fn(&str) -> bool, out: &mut Vec<PathBuf>) {
    visit(dir, &mut |p| {
        if p.is_file() {
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if pred(name) {
                    out.push(p.to_path_buf());
                }
            }
        }
    });
}

fn visit(dir: &Path, f: &mut dyn FnMut(&Path)) {
    visit_depth(dir, f, 0);
}

fn visit_depth(dir: &Path, f: &mut dyn FnMut(&Path), depth: usize) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        f(&path);
        if path.is_dir() {
            visit_depth(&path, f, depth + 1);
        }
    }
}

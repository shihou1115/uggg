//! Irodori-TTS の Python ランタイム + 共通依存の初回 DL (architecture §8.2-8.3, M4c Phase C/G)。
//!
//! 配置は `%APPDATA%\ugg\irodori\python\` 配下。
//! - `python.exe` / `python311.dll` 等: 公式 Embeddable Python (Windows x64) を ZIP で取得
//! - `Lib\site-packages\`: pip ブートストラップで作成、torch / fastapi / uvicorn / huggingface_hub
//!   + Phase G 追加で transformers / peft / accelerate / safetensors / librosa / numba / 等 +
//!   irodori-tts / dacvae / silentcipher を GitHub アーカイブから取得
//!
//! HF モデル本体 (Aratako/Irodori-TTS-*) の DL は `sidecar.py::download_models` が担う (Phase G)。
//! 本モジュールは「Python が起動して `from irodori_tts.inference_runtime import InferenceRuntime`
//! できる状態」までを担う。
//!
//! 設計判断:
//! - Python 3.11.x: torch CUDA wheel が最も安定して提供されている系列 (3.13 はまだ部分対応)
//! - torch >=2.10 / cu128: Irodori-TTS upstream pyproject の要求に合わせる (cu121 から更新)
//! - GitHub アーカイブ URL での pip install: embeddable Python に git CLI が無いため `git+` URL は
//!   使えない。代わりに `https://github.com/.../archive/<ref>.zip` でアーカイブを取得し pip に渡す
//! - `--no-deps`: irodori-tts pyproject の `dacvae` / `silentcipher` git+ 依存をスキップし
//!   別 step で明示的に install (順序: silentcipher → dacvae → irodori-tts)
//! - zip 展開は PowerShell の `Expand-Archive` 呼び出し: 追加 crate なし
//! - run_python は wait → 全 stdout/stderr 一括読み: シンプル優先。リアルタイム進捗が必要になれば
//!   spawn_blocking + thread + channel に拡張する (現状は各 step 開始時に on_line でステージを emit)

use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

/// 採用 Python バージョン (CUDA 12.x torch wheel が安定して提供されている系列)。
pub const PYTHON_VERSION: &str = "3.11.9";
/// 公式 Embeddable Python (Windows x64) の DL URL。
const PYTHON_URL: &str = "https://www.python.org/ftp/python/3.11.9/python-3.11.9-embed-amd64.zip";
/// pip ブートストラップ用スクリプト。
const GET_PIP_URL: &str = "https://bootstrap.pypa.io/get-pip.py";
/// torch CUDA 12.8 wheel の追加 index URL。
/// Irodori-TTS upstream pyproject が `torch>=2.10.0` を要求するため Phase G で cu121→cu128 に更新。
const TORCH_CUDA_INDEX_URL: &str = "https://download.pytorch.org/whl/cu128";

/// Phase C で確実にインストールする共通依存。バージョン固定で再現性を担保。
/// torch 系は CUDA index 経由で別途インストールする (`install_torch_cuda`)。
const COMMON_REQUIREMENTS: &[&str] = &[
    "fastapi==0.115.6",
    "uvicorn[standard]==0.32.1",
    "huggingface_hub==0.27.0",
    "numpy<2",
    "soundfile==0.12.1",
];

/// torch / torchaudio バージョン (CUDA 12.8)。
/// Irodori-TTS upstream pyproject の `torch>=2.10.0,<2.11.0` レンジに合わせる。
const TORCH_PACKAGES: &[&str] = &["torch>=2.10.0,<2.11.0", "torchaudio>=2.10.0,<2.11.0"];

/// Irodori-TTS が要求する追加 pip パッケージ (Phase G)。
/// upstream pyproject の dependencies と Phase G で実 InferenceRuntime に必要な周辺ライブラリを
/// 過不足なく揃える (`dacvae` / `silentcipher` / `irodori-tts` 本体は GitHub アーカイブで別途)。
const IRODORI_EXTRA_REQUIREMENTS: &[&str] = &[
    "torchcodec>=0.10.0,<0.11.0",
    "transformers<5",
    "accelerate>=1.0.0",
    "peft>=0.18.0",
    "safetensors>=0.7.0",
    "datasets>=3.0.0",
    "librosa",
    "numba>=0.57.0",
    "llvmlite>=0.40.0",
    "sentencepiece>=0.1.99,<0.2",
    "pyyaml>=6.0",
    "tqdm>=4.67.3",
    "einops",
    // dacvae の transitive 依存 (PyPI 名は descript-audiotools、import 名は audiotools)。
    // upstream Irodori-TTS pyproject では dacvae の git+ 依存だけ宣言されているが、それ自体は
    // descript-audiotools を要件として書いておらず、実合成 (DAC codec encode/decode) で
    // `No module named 'audiotools'` が発生する。明示的に追加。
    "descript-audiotools>=0.7.2",
];

/// silentcipher (Sesame AI Labs) の固定 commit zipball。upstream Irodori-TTS pyproject の
/// `silentcipher @ git+https://github.com/SesameAILabs/silentcipher.git@<hash>` と同じ hash。
const SILENTCIPHER_ZIPBALL: &str =
    "https://github.com/SesameAILabs/silentcipher/archive/d46d7d0893a583d8968ab3a6626e2289faec9152.zip";

/// dacvae (facebookresearch) の**固定 commit** zipball。upstream Irodori-TTS pyproject の
/// `dacvae = { git = "https://github.com/facebookresearch/dacvae" }` と同じリポジトリ。
/// pin: 2025-12-22 時点の main HEAD。
const DACVAE_ZIPBALL: &str =
    "https://github.com/facebookresearch/dacvae/archive/414c20785fc3a28373073ea8ef7a1316eeeaca6e.zip";

/// Irodori-TTS 本体 (Aratako) の**固定 commit** zipball。`infer.py` / `irodori_tts.inference_runtime`
/// を提供する。pin: 2026-08-11 時点の main HEAD。
///
/// **3 資産すべてを commit 固定にする** (v0.4 負債返済 D4)。以前は本体と dacvae が
/// `refs/heads/main` 追随で、上流の破壊的変更がそのまま配布版の初回 DL を壊しうる状態だった
/// (silentcipher だけが pin 済みという非一貫)。更新するときは
/// **ここを手で上げて実機で DL・合成まで通す**こと。
const IRODORI_TTS_ZIPBALL: &str =
    "https://github.com/Aratako/Irodori-TTS/archive/8224dafb46d0aba89209a8f905f1cb7e3299d9c1.zip";

/// Python 配置ディレクトリ (`%APPDATA%\ugg\irodori\python\`)。
/// Phase D 以降の `sidecar.py` 起動で使う。
#[allow(dead_code)]
pub fn python_dir() -> Result<PathBuf> {
    Ok(crate::tts::voice_ref::irodori_root()?.join("python"))
}

/// Python 実行ファイル (`python.exe`)。Phase D 以降のサイドカー起動で使う。
#[allow(dead_code)]
pub fn python_exe() -> Result<PathBuf> {
    Ok(python_dir()?.join("python.exe"))
}

/// Irodori 資産が「実モデル可」レベルまで揃っているか。
/// Phase C (python.exe + torch + fastapi + uvicorn + huggingface_hub) + Phase G (irodori_tts) を要求。
/// この判定が true のときのみ設定パネルの「実モデルを使う (β)」トグルが enable される。
/// 導入記録のファイル名。`%APPDATA%\ugg\irodori\installed.json`。
const STAMP_FILE: &str = "installed.json";
/// 導入記録のスキーマ版。形を変えたら上げる（読めない版は「記録なし」として扱う）。
const STAMP_SCHEMA: u32 = 1;
/// 解決済みバージョンを python から受け取るときの目印。
const VERSIONS_MARKER: &str = "UGG_RESOLVED_VERSIONS ";

/// 「何を入れたか」の記録 (v0.5.4 項目 1)。
///
/// **なぜ要るか**: `assets_ready` はパッケージの存在しか見ないため、pin を上げても
/// 既存環境には永久に届かない。届いていないことに気づく手段が、アプリ側に 1 つも無かった
/// （pip の `direct_url.json` を読まないと分からない）。
///
/// **`pins` と `resolved` を両方持つ理由**は役割が違うから:
/// - `pins` = このビルドが**要求した**もの。いまの定数と突き合わせて「入れ直しが要るか」を決める
/// - `resolved` = **実際に入った**もの。指定どおりに入るとは限らない
///   （`huggingface_hub==0.27.0` と書いてあるのに実機は 0.36.2 だった。transformers の
///   依存に押し上げられたため）。記録するのは指定値ではなく実測値でなければ意味がない
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InstalledStamp {
    pub schema: u32,
    /// 導入が完了した unix 秒。
    pub installed_at: i64,
    /// このビルドが要求した固定 URL（名前 → URL）。
    pub pins: std::collections::BTreeMap<String, String>,
    /// 実際に入ったバージョン（配布名 → 版。取得できなければ欠落）。
    pub resolved: std::collections::BTreeMap<String, String>,
}

/// いまのビルドが要求している固定 URL 一式。
///
/// **ここに挙げたものだけが「入れ直しが要るか」の判定材料になる。**
/// pin を増やしたらここにも足すこと（`pins_cover_every_pinned_url` が件数で見張る）。
pub fn current_pins() -> std::collections::BTreeMap<String, String> {
    [
        ("python", PYTHON_URL),
        ("silentcipher", SILENTCIPHER_ZIPBALL),
        ("dacvae", DACVAE_ZIPBALL),
        ("irodori_tts", IRODORI_TTS_ZIPBALL),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

fn stamp_path(asset_root: &Path) -> PathBuf {
    asset_root.join(STAMP_FILE)
}

/// 導入記録を読む。無い・壊れている・スキーマが違う場合は `None`
/// （= v0.5.4 より前に導入した環境。記録が無いこと自体が「古い」の証拠になる）。
pub fn read_stamp(asset_root: &Path) -> Option<InstalledStamp> {
    let text = std::fs::read_to_string(stamp_path(asset_root)).ok()?;
    let stamp: InstalledStamp = serde_json::from_str(&text).ok()?;
    (stamp.schema == STAMP_SCHEMA).then_some(stamp)
}

/// `pins` を明示して記録を書く。
///
/// **部分更新のあとは「入れ直した分だけ」を書き換える。** 全部を現在値にすると、
/// まだ古いままの依存（`ensure_python_embeddable` が skip する Python 本体など）まで
/// 「最新」と記録してしまい、記録そのものが嘘になる。
pub fn write_stamp_pins(
    asset_root: &Path,
    pins: std::collections::BTreeMap<String, String>,
    resolved: std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let stamp = InstalledStamp {
        schema: STAMP_SCHEMA,
        installed_at: chrono::Utc::now().timestamp(),
        pins,
        resolved,
    };
    let json = serde_json::to_string_pretty(&stamp).context("導入記録の JSON 化")?;
    std::fs::write(stamp_path(asset_root), json)
        .with_context(|| format!("導入記録の書き出し: {}", stamp_path(asset_root).display()))?;
    Ok(())
}

/// 要件文字列から配布名だけを取り出す。
/// `"uvicorn[standard]==0.32.1"` → `"uvicorn"` / `"torch>=2.10.0,<2.11.0"` → `"torch"`。
fn requirement_name(spec: &str) -> &str {
    let end = spec
        .find(|c: char| matches!(c, '[' | '=' | '<' | '>' | '!' | '~' | ';' | ' '))
        .unwrap_or(spec.len());
    spec[..end].trim()
}

/// 記録対象の配布名（重複を除いた順序保持）。
/// pip で名前を指定して入れたもの全部 + GitHub アーカイブで入れた 3 本。
fn recorded_distributions() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let named = COMMON_REQUIREMENTS
        .iter()
        .chain(TORCH_PACKAGES.iter())
        .chain(IRODORI_EXTRA_REQUIREMENTS.iter())
        .map(|s| requirement_name(s).to_string());
    for name in named.chain(
        ["silentcipher", "dacvae", "irodori-tts"]
            .into_iter()
            .map(str::to_string),
    ) {
        if !out.iter().any(|n| n == &name) {
            out.push(name);
        }
    }
    out
}

/// `VERSIONS_MARKER` 付きの行から解決済みバージョンを取り出す。
fn parse_resolved_line(line: &str) -> Option<std::collections::BTreeMap<String, String>> {
    let json = line.trim().strip_prefix(VERSIONS_MARKER)?;
    serde_json::from_str(json).ok()
}

/// 実際に入ったバージョンを python に聞く。
///
/// **失敗しても導入を失敗にしない。** 記録が取れないこと自体は動作に影響しないので、
/// 空の記録を返して続行する（`pins` だけでも「入れ直しが要るか」は判定できる）。
fn query_resolved_versions<F>(
    py_exe: &Path,
    mut on_line: F,
) -> std::collections::BTreeMap<String, String>
where
    F: FnMut(&str),
{
    let names = recorded_distributions();
    let script = format!(
        "import json,importlib.metadata as m
out={{}}
for n in {names:?}:
    try: out[n]=m.version(n)
    except Exception: pass
print({marker:?}+json.dumps(out,sort_keys=True))",
        names = names,
        marker = VERSIONS_MARKER,
    );
    let mut found = None;
    let res = run_python(py_exe, &["-c", &script], |line| {
        if let Some(map) = parse_resolved_line(line) {
            found = Some(map);
        } else {
            on_line(line);
        }
    });
    if let Err(err) = res {
        on_line(&format!("導入バージョンの記録に失敗しました (続行します): {err:#}"));
    }
    found.unwrap_or_default()
}

/// 導入記録を残す（`download_irodori_assets` の最後に呼ぶ、v0.5.4 項目 1）。
pub fn record_installed<F>(asset_root: &Path, mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    // 初回導入は「入れ直せる 3 本」を入れたことになる。**`python` をここに含めない** —
    // `ensure_python_embeddable` は `python.exe` があれば skip するので、
    // 「入れた」と「入っている」は一致しない。実物に聞いて一致したときだけ記録する。
    let installed: Vec<String> = current_pins()
        .keys()
        .filter(|k| updatable_pin(k).is_some())
        .cloned()
        .collect();
    record_after_install(asset_root, &installed, |l| on_line(l))?;
    on_line("導入内容を記録しました");
    Ok(())
}

/// 導入・更新のあとに記録を書く（両経路で共通）。
///
/// **`python` は実物が pin と一致したときだけ記録する。** 初回導入の経路もここを通す。
/// 通していなかったため、`ensure_python_embeddable` が skip した古い python を
/// 「最新」と記録し、以後 `should_ask_python` が実物に聞かなくなって
/// **永久に見えなくなる**穴が残っていた（更新経路だけ塞いで隣を残していた）。
fn record_after_install<F>(asset_root: &Path, installed: &[String], mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    let py_exe = asset_root.join("python").join("python.exe");
    let stamp = read_stamp(asset_root);
    let recorded = stamp.as_ref().map(|s| s.pins.clone()).unwrap_or_default();
    let mut resolved = stamp.map(|s| s.resolved).unwrap_or_default();
    // 記録の要点は「指定した版」ではなく「実際に入った版」。毎回取り直す。
    resolved.extend(query_resolved_versions(&py_exe, |l| on_line(l)));
    let py_version = installed_python_version(asset_root);
    let python_matches = py_version.as_deref() == pinned_python_version();
    if let Some(v) = py_version {
        resolved.insert("python".to_string(), v);
    }
    write_stamp_pins(
        asset_root,
        merged_pins(&recorded, installed, python_matches),
        resolved,
    )
}

/// 導入状態 (v0.5.4 項目 2)。
///
/// **「使える」と「最新」は別の質問。** `present` が真なら実モデルは使える。
/// `up_to_date` が偽でも使えることに変わりはない（古いコードで古いモデルを動かしている
/// だけ）。ここを混ぜて `assets_ready` を偽にすると、フロントの
/// `canUseReal = gpuOk && assetsOk` が倒れ、**ユーザーの
/// `tts_irodori_use_real_model` が黙って false に書き換わって永続化される**。
/// 動いている環境を壊さないために、2 つの信号は分けたままにする。
#[derive(Debug, Clone, serde::Serialize)]
pub struct IrodoriStatus {
    /// パッケージが一式そろっているか（従来の `assets_ready` と同じ意味）。
    pub present: bool,
    /// **導入記録があるか。** v0.5.4 より前に導入した環境では無い
    /// （記録が無いこと自体が「いつの版か分からない」の証拠）。
    pub has_record: bool,
    /// いまのビルドが要求する pin と、記録された pin が一致するか。
    /// 記録が無ければ `false`（分からないものを「最新」とは言わない）。
    pub up_to_date: bool,
    /// 一致しなかった pin の名前。`up_to_date` が偽の理由を示す。
    pub outdated: Vec<String>,
    /// 記録されている「実際に入った版」。
    pub resolved: std::collections::BTreeMap<String, String>,
}

/// pin されている Python の版（`PYTHON_URL` の `python-3.11.9-embed-amd64.zip` → `3.11.9`）。
pub fn pinned_python_version() -> Option<&'static str> {
    PYTHON_URL
        .rsplit('/')
        .next()?
        .strip_prefix("python-")?
        .split('-')
        .next()
}

/// 入っている Python の版を**実物に聞く**（`python.exe --version` → `Python 3.11.9`）。
///
/// 記録を当てにしないのは、`ensure_python_embeddable` が `python.exe` があれば skip
/// するため、**「記録が無い」ことと「版が違う」ことが別物**だから。混同すると、
/// 直せもしない入れ直しを要求して更新経路そのものが止まる。
pub fn installed_python_version(asset_root: &Path) -> Option<String> {
    let py_exe = asset_root.join("python").join("python.exe");
    let mut found = None;
    run_python(&py_exe, &["--version"], |line| {
        if let Some(rest) = line.trim().strip_prefix("Python ") {
            found = Some(rest.trim().to_string());
        }
    })
    .ok()?;
    found
}

/// 版が違うと**言い切れる**ときだけ真。聞けなかったときは偽。
///
/// 記録の欠落は「更新ボタンを出す」だけで済むが、python の判定を誤ると
/// 「全部消して入れ直せ」という数 GB の要求になる。証拠が無い側へ倒す。
fn python_is_stale(installed: Option<&str>, pinned: Option<&str>) -> bool {
    matches!((installed, pinned), (Some(i), Some(p)) if i != p)
}

/// 実物に版を聞く必要があるか。
///
/// 記録が現在の pin を主張しているなら、それは初回導入が**実際に入れた**もの。
/// 聞き直さない（設定パネルを開くたびに python.exe を起動しないため）。
fn should_ask_python(recorded: &std::collections::BTreeMap<String, String>) -> bool {
    recorded.get("python") != current_pins().get("python")
}

/// 入れ直しが要る pin の名前を並べる。**python だけは記録ではなく実物で判定する。**
fn outdated_list(
    asset_root: &Path,
    recorded: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    let current = current_pins();
    let mut out = outdated_pins(recorded, &current);
    out.retain(|n| n != "python");
    if should_ask_python(recorded)
        && python_is_stale(
            installed_python_version(asset_root).as_deref(),
            pinned_python_version(),
        )
    {
        out.push("python".to_string());
        out.sort();
    }
    out
}

/// 導入状態を調べる (v0.5.4 項目 2)。
pub fn status(asset_root: &Path) -> IrodoriStatus {
    let present = assets_ready(asset_root);
    let Some(stamp) = read_stamp(asset_root) else {
        return IrodoriStatus {
            present,
            has_record: false,
            up_to_date: false,
            // **記録が無くても「入れ直せば済むもの」は名指しできる。** ここを空にすると
            // 呼び出し側が対象を自前で組み立てることになり、実際それで入れ直せない
            // python が混ざって、この機能が対象にしている環境がちょうど 1 つも
            // 更新できなくなっていた。
            outdated: if present {
                outdated_list(asset_root, &Default::default())
            } else {
                Vec::new()
            },
            resolved: Default::default(),
        };
    };
    let outdated = outdated_list(asset_root, &stamp.pins);
    IrodoriStatus {
        present,
        has_record: true,
        up_to_date: outdated.is_empty(),
        outdated,
        resolved: stamp.resolved,
    }
}

/// 記録された pin と、いまのビルドが要求する pin を突き合わせる。
///
/// 返すのは**入れ直しが要る名前**。`current` にあって `recorded` と違うもの、および
/// `current` にあって `recorded` に無いもの（pin を増やした場合）。
/// 逆に `recorded` にしか無いものは無視する（pin を減らした場合、入れ直しは要らない）。
fn outdated_pins(
    recorded: &std::collections::BTreeMap<String, String>,
    current: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    current
        .iter()
        .filter(|(name, url)| recorded.get(*name) != Some(url))
        .map(|(name, _)| name.clone())
        .collect()
}

pub fn assets_ready(asset_root: &Path) -> bool {
    let py = asset_root.join("python").join("python.exe");
    if !py.is_file() {
        return false;
    }
    let site = asset_root.join("python").join("Lib").join("site-packages");
    if !site.is_dir() {
        return false;
    }
    has_package(&site, "torch")
        && has_package(&site, "fastapi")
        && has_package(&site, "uvicorn")
        && has_package(&site, "huggingface_hub")
        && has_package(&site, "irodori_tts")
}

fn has_package(site_packages: &Path, name: &str) -> bool {
    // <pkg>/__init__.py または <pkg>.py または <pkg>-*.dist-info で判定
    let pkg = site_packages.join(name);
    if pkg.is_dir() {
        return true;
    }
    if site_packages.join(format!("{name}.py")).is_file() {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(site_packages) else {
        return false;
    };
    let prefix = format!("{name}-");
    for e in entries.flatten() {
        if let Some(n) = e.file_name().to_str() {
            if n.starts_with(&prefix) && n.ends_with(".dist-info") {
                return true;
            }
        }
    }
    false
}

// ============ DL の各ステップ ============

/// 1) Embeddable Python の取得と展開。
/// 既に `python.exe` があれば skip する。
pub async fn ensure_python_embeddable<F>(asset_root: &Path, mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    let py_dir = asset_root.join("python");
    let py_exe = py_dir.join("python.exe");
    if py_exe.is_file() {
        on_line("Python ランタイムは既に配置済みです (skip)");
        return Ok(());
    }
    std::fs::create_dir_all(&py_dir)
        .with_context(|| format!("create python dir: {}", py_dir.display()))?;

    on_line(&format!(
        "Embeddable Python {PYTHON_VERSION} をダウンロードしています…"
    ));
    let zip_path = py_dir.join(format!("python-{PYTHON_VERSION}-embed-amd64.zip"));
    download_to(PYTHON_URL, &zip_path).await?;

    on_line("Python ZIP を展開しています…");
    expand_zip_windows(&zip_path, &py_dir)?;
    // 展開後の zip は不要
    let _ = std::fs::remove_file(&zip_path);

    on_line("python._pth を編集して site-packages を有効化…");
    enable_site_packages(&py_dir)?;

    Ok(())
}

/// 2) get-pip.py 経由で pip をブートストラップする。
pub async fn ensure_pip<F>(asset_root: &Path, mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    let py_dir = asset_root.join("python");
    let py_exe = py_dir.join("python.exe");
    if !py_exe.is_file() {
        return Err(anyhow!("python.exe が見つかりません: {}", py_exe.display()));
    }
    let pip_dir = py_dir.join("Lib").join("site-packages").join("pip");
    if pip_dir.is_dir() {
        on_line("pip は既にブートストラップ済みです (skip)");
        return Ok(());
    }

    let get_pip = py_dir.join("get-pip.py");
    on_line("get-pip.py を取得しています…");
    download_to(GET_PIP_URL, &get_pip).await?;

    on_line("pip をブートストラップしています…");
    run_python(&py_exe, &[get_pip.to_string_lossy().as_ref()], |l| on_line(l))?;
    Ok(())
}

/// 3) 共通依存 (fastapi / uvicorn / huggingface_hub / numpy / soundfile) を pip install。
pub async fn install_common_requirements<F>(asset_root: &Path, mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    let py_exe = asset_root.join("python").join("python.exe");
    on_line(&format!(
        "共通 Python 依存をインストールしています ({} パッケージ)…",
        COMMON_REQUIREMENTS.len()
    ));
    let mut args: Vec<&str> = vec!["-m", "pip", "install", "--no-warn-script-location"];
    args.extend(COMMON_REQUIREMENTS);
    run_python(&py_exe, &args, |l| on_line(l))?;
    Ok(())
}

/// 4) torch + torchaudio (CUDA 12.8) を pip install。サイズが大きい (1〜2GB)。
pub async fn install_torch_cuda<F>(asset_root: &Path, mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    let py_exe = asset_root.join("python").join("python.exe");
    on_line("PyTorch (CUDA 12.8) をインストールしています… (1〜2GB ダウンロードします)");
    let mut args: Vec<&str> = vec![
        "-m",
        "pip",
        "install",
        "--no-warn-script-location",
        "--upgrade",
        "--index-url",
        TORCH_CUDA_INDEX_URL,
    ];
    args.extend(TORCH_PACKAGES);
    run_python(&py_exe, &args, |l| on_line(l))?;
    Ok(())
}

/// 5) Irodori-TTS ランタイム本体 + 追加 pip 依存 (M4c Phase G)。
///
/// 順序: 追加 pip 依存 (transformers / peft / accelerate …) → silentcipher → dacvae → irodori-tts。
/// silentcipher/dacvae は irodori-tts の git+ 依存なので、irodori-tts を `--no-deps` で入れる前に
/// 別個に install しておく。dacvae リポジトリの公開状況や upstream API は実機 GPU で初回検証して
/// 必要に応じて URL/コミットを pin する想定 (本セッションは upstream pyproject 通りに繋ぐ)。
///
/// 全ステップに `--upgrade` を付け、過去に旧版が入っていた場合でも要件レンジに揃え直す。
pub async fn install_irodori_runtime<F>(asset_root: &Path, mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    let py_exe = asset_root.join("python").join("python.exe");
    if !py_exe.is_file() {
        return Err(anyhow!(
            "python.exe が見つかりません: {}",
            py_exe.display()
        ));
    }

    on_line(&format!(
        "Irodori-TTS の追加 pip 依存をインストールしています ({} パッケージ、~数百MB)…",
        IRODORI_EXTRA_REQUIREMENTS.len()
    ));
    let mut args: Vec<&str> = vec![
        "-m",
        "pip",
        "install",
        "--no-warn-script-location",
        "--upgrade",
    ];
    args.extend(IRODORI_EXTRA_REQUIREMENTS);
    run_python(&py_exe, &args, |l| on_line(l))?;

    on_line("silentcipher を GitHub アーカイブから取得しています…");
    run_python(
        &py_exe,
        &[
            "-m",
            "pip",
            "install",
            "--no-warn-script-location",
            "--no-deps",
            SILENTCIPHER_ZIPBALL,
        ],
        |l| on_line(l),
    )?;

    on_line("dacvae を GitHub アーカイブから取得しています…");
    run_python(
        &py_exe,
        &[
            "-m",
            "pip",
            "install",
            "--no-warn-script-location",
            "--no-deps",
            DACVAE_ZIPBALL,
        ],
        |l| on_line(l),
    )?;

    on_line("Irodori-TTS 本体を GitHub アーカイブから取得しています…");
    run_python(
        &py_exe,
        &[
            "-m",
            "pip",
            "install",
            "--no-warn-script-location",
            "--no-deps",
            IRODORI_TTS_ZIPBALL,
        ],
        |l| on_line(l),
    )?;

    on_line("Irodori-TTS ランタイムのインストールが完了しました");
    Ok(())
}

/// 更新の作業用ディレクトリ（site-packages の**外**に置く）。
///
/// site-packages の中に退避すると、名前次第で import されうるうえ、pip が
/// dist-info を拾って混乱する。`asset_root` 直下に置いて完全に切り離す。
/// 導入と更新を同時に走らせないための印。
///
/// **同じ `site-packages` を 2 つの経路が同時に触ると、退避 → 入れ直し → 復元の
/// どの段も守れない中間状態になる。** 記録が無い環境では「ランタイムをダウンロード」と
/// 「更新する」が両方押せるので、10〜20 分かかる初回 DL の途中で更新を押せてしまう。
/// 項目 3 の約束（失敗しても動いていた環境を壊さない）はこの排他が前提。
static IRODORI_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// いま導入または更新が走っているか (v0.5.5 項目 4)。
///
/// **合成の側もこれを見る。** 見ていなかったため、更新の最中に発話が来ると
/// **半分入れ替わった `site-packages` で新しいサイドカーが起動しうる**。
/// 数十秒だった v0.5.4 では踏みにくいが、数 GB・十数分になるモデル更新では現実的に踏む。
pub fn is_busy() -> bool {
    IRODORI_BUSY.load(std::sync::atomic::Ordering::SeqCst)
}

/// 取れたら作業してよい。drop で自動的に手放す（途中で return しても取り残さない）。
pub struct IrodoriBusyGuard(());

impl IrodoriBusyGuard {
    pub fn acquire() -> Result<Self> {
        if IRODORI_BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err(anyhow!(
                "Irodori ランタイムの導入または更新がすでに進行中です。終わってからもう一度お試しください"
            ));
        }
        Ok(Self(()))
    }
}

impl Drop for IrodoriBusyGuard {
    fn drop(&mut self) {
        IRODORI_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

const UPDATE_BACKUP_DIR: &str = ".update-backup";

/// pin ごとの「入れ直し方」。
///
/// **`python` はここに無い。** `ensure_python_embeddable` は `python.exe` があれば
/// skip するため、Python 本体の pin を上げても既存環境には反映されない。
/// 稼働中のインタプリタをその場で差し替える安全な方法は無いので、
/// **入れ直しの対象にせず、全体の入れ直しが要る旨を伝える**（黙って失敗させない）。
fn updatable_pin(name: &str) -> Option<(&'static str, &'static str)> {
    match name {
        // (site-packages 上のディレクトリ名, 入れ直しに使う URL)
        "silentcipher" => Some(("silentcipher", SILENTCIPHER_ZIPBALL)),
        "dacvae" => Some(("dacvae", DACVAE_ZIPBALL)),
        "irodori_tts" => Some(("irodori_tts", IRODORI_TTS_ZIPBALL)),
        _ => None,
    }
}

/// 資産ルートから site-packages の位置を出す。
fn site_of(asset_root: &Path) -> PathBuf {
    asset_root.join("python").join("Lib").join("site-packages")
}

/// site-packages にいま何があるかを 1 行で述べる（実機検証の観測点）。
///
/// pip が `Successfully installed` と言っているのに**ファイルが変わっていない**
/// ことがあったため、入れ直しの前後で実際の姿を証跡に残す。
/// `direct_url.json` の `url` は「どの版か」を示す唯一の手がかりになる。
fn describe_package(site: &Path, pkg: &str) -> String {
    let mut infos: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(site) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(&format!("{pkg}-")) && name.ends_with(".dist-info") {
                let url = std::fs::read_to_string(e.path().join("direct_url.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .and_then(|v| v.get("url").and_then(|u| u.as_str()).map(str::to_string))
                    .unwrap_or_else(|| "(url なし)".to_string());
                infos.push(format!("{name} <- {url}"));
            }
        }
    }
    format!("dir={} info=[{}]", site.join(pkg).is_dir(), infos.join(", "))
}

/// `site-packages/<pkg>` と `<pkg>-*.dist-info` を退避先へ移す。
fn move_package_aside(site: &Path, pkg: &str, backup: &Path) -> Result<()> {
    std::fs::create_dir_all(backup).with_context(|| format!("mkdir {}", backup.display()))?;
    let mut moved = false;
    let dir = site.join(pkg);
    if dir.is_dir() {
        std::fs::rename(&dir, backup.join(pkg))
            .with_context(|| format!("退避: {}", dir.display()))?;
        moved = true;
    }
    // dist-info はバージョン番号を含むので走査して拾う。
    if let Ok(entries) = std::fs::read_dir(site) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(&format!("{pkg}-")) && name.ends_with(".dist-info") {
                std::fs::rename(e.path(), backup.join(&name))
                    .with_context(|| format!("退避: {name}"))?;
                moved = true;
            }
        }
    }
    if !moved {
        // 入っていなかった場合も更新自体は続行してよい（新規に入る）。
        crate::ulog!("[irodori] 退避対象が見つかりません (新規導入として続行): {pkg}");
    }
    Ok(())
}

/// 退避したものを元の場所へ戻す。
fn restore_package(site: &Path, backup: &Path) -> Result<()> {
    let entries = std::fs::read_dir(backup)
        .with_context(|| format!("退避先の読み取り: {}", backup.display()))?;
    for e in entries.flatten() {
        let dest = site.join(e.file_name());
        // 失敗した入れ直しが中途半端に残していたら先に退ける。
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::rename(e.path(), &dest)
            .with_context(|| format!("復元: {}", dest.display()))?;
    }
    Ok(())
}

/// 入れ直しで壊れていないかを見るモジュール（import 名）。
const RUNTIME_MODULES: &[&str] = &["irodori_tts", "dacvae", "silentcipher"];

/// いま import できるか。できなければ**理由**（例外の最終行）を添える。
///
/// 理由を捨ててはいけない。最初の実装は出力を握り潰しており、実機で落ちたときに
/// 分かったのは「python 異常終了 (code Some(1))」だけだった。原因
/// （`No module named 'pydub'`）に辿り着くのに余計な一往復を要した。
fn import_report(py_exe: &Path) -> std::collections::BTreeMap<String, Option<String>> {
    RUNTIME_MODULES
        .iter()
        .map(|m| {
            let code = format!("import {m}");
            let mut last = String::new();
            let failed = run_python(py_exe, &["-c", code.as_str()], |l| last = l.to_string()).err();
            (m.to_string(), failed.map(|_| last))
        })
        .collect()
}

/// **入れ直す前より悪くなったものだけ**を返す。
///
/// **「全部 import できること」を条件にしてはいけない。** 実機の `silentcipher` は
/// `pydub` が入っていないため**一度も import できたことが無い**（ugg は `pydub` を
/// 入れず、upstream の `watermark.py` も `ImportError` を握って「透かし無しで続行」する
/// 設計）。それでも合成は成立している。ここで「全部使えること」を絶対条件にすると、
/// **既存環境では必ずロールバックし、更新が誰にも一度も成功しない**
/// （2026-09-11 の実機検証で実際にそうなった）。
///
/// 項目 3 が約束しているのは「失敗しても、それまで動いていた環境を壊さない」であって
/// 「壊れていた環境を直す」ではない。したがって判定は**絶対値ではなく差分**で行う。
fn import_regressions(
    before: &std::collections::BTreeMap<String, Option<String>>,
    after: &std::collections::BTreeMap<String, Option<String>>,
) -> Vec<String> {
    before
        .iter()
        .filter(|(_, err)| err.is_none())
        .filter_map(|(m, _)| match after.get(m) {
            Some(None) => None,
            Some(Some(why)) => Some(format!("{m}: {why}")),
            None => Some(m.clone()),
        })
        .collect()
}

/// 入れ直せた分だけを現在値へ反映した記録用の `pins` を作る。
///
/// **全部を現在値にしてはいけない。** 入れ直していない依存まで「最新」と記録すると、
/// 記録そのものが嘘になる。`python` は入れ直さないが、**実物が pin と一致していることを
/// 確認できたときだけ**記録する（確認せずに書けば嘘になり、確認したのに書かなければ
/// 設定パネルを開くたびに `python.exe` に聞き直すことになる）。
fn merged_pins(
    recorded: &std::collections::BTreeMap<String, String>,
    updated: &[String],
    python_matches: bool,
) -> std::collections::BTreeMap<String, String> {
    let current = current_pins();
    let mut pins = recorded.clone();
    for name in updated {
        if let Some(url) = current.get(name) {
            pins.insert(name.clone(), url.clone());
        }
    }
    if python_matches {
        if let Some(url) = current.get("python") {
            pins.insert("python".to_string(), url.clone());
        }
    }
    pins
}

/// 古くなった分だけを入れ直す (v0.5.4 項目 3、spec §6.0)。
///
/// **失敗しても、それまで動いていた環境を壊さない。** pip は「古いものを消してから
/// 新しいものを入れる」ので、途中で失敗すると消えたままになる。そこで
/// **退避 → 入れ直し → import 確認 → 成功したら退避を捨てる**の順にし、
/// どこかで失敗したら退避から戻す（v0.5.3 項目 2 と同じ規律）。
/// **戻すことにも失敗したら退避先を消さず、場所をログに残す。**
///
/// 戻り値は「入れ直せた pin の名前」。呼び出し側はこれで記録を部分的に更新する。
pub async fn update_irodori_runtime<F>(
    asset_root: &Path,
    outdated: &[String],
    mut on_line: F,
) -> Result<Vec<String>>
where
    F: FnMut(&str),
{
    let py_exe = asset_root.join("python").join("python.exe");
    if !py_exe.is_file() {
        return Err(anyhow!(
            "Python ランタイムがありません。先に初回導入を行ってください: {}",
            py_exe.display()
        ));
    }
    let site = site_of(asset_root);

    if outdated.iter().any(|n| n == "python") {
        // ここだけは安全に入れ直せない。黙って部分更新して「最新」と記録するより、
        // 何が要るかを伝えて止まるほうがよい。
        return Err(anyhow!(
            "Python 本体の版が変わっています。この経路では入れ直せません（稼働中のインタプリタを差し替えられないため）。`%APPDATA%\\ugg\\irodori\\python` を削除してから、もう一度導入してください"
        ));
    }

    let backup_root = asset_root.join(UPDATE_BACKUP_DIR);
    // **残っている退避を無条件に消さない。** 退避が残っているのは「復元に失敗したので
    // 消さずに置いた」ときだけで、そこには**唯一残った旧版**が入っている。
    // まず戻しを試み、戻せたら捨てる。戻せなければ場所を伝えて止まる
    // （消してから入れ直しに失敗すると、守ろうとしたものを失う）。
    if backup_root.is_dir() {
        on_line("前回の入れ直しが中断しています。退避したものを先に戻します…");
        let mut recovered = true;
        if let Ok(entries) = std::fs::read_dir(&backup_root) {
            for e in entries.flatten() {
                if let Err(err) = restore_package(&site, &e.path()) {
                    crate::ulog!("[irodori] 退避の復元に失敗: {} ({err:#})", e.path().display());
                    recovered = false;
                }
            }
        }
        if !recovered {
            return Err(anyhow!(
                "前回の入れ直しで退避したものを戻せません。手動で戻してから再実行してください。退避先: {}",
                backup_root.display()
            ));
        }
        let _ = std::fs::remove_dir_all(&backup_root);
        on_line("戻しました。入れ直しを続けます");
    }

    // 入れ直す前に「いま何が使えるか」を控える。ここを控えずに絶対値で判定すると、
    // 元から import できないものを理由に、正常な入れ直しまで巻き戻してしまう。
    let before_imports = import_report(&py_exe);
    for (m, err) in &before_imports {
        if let Some(why) = err {
            on_line(&format!("注意: {m} は入れ直す前から import できません ({why})"));
        }
    }

    let mut updated: Vec<String> = Vec::new();
    for name in outdated {
        let Some((pkg, url)) = updatable_pin(name) else {
            on_line(&format!("{name} は入れ直しの対象外です (skip)"));
            continue;
        };
        on_line(&format!("{pkg} を入れ直しています…"));
        on_line(&format!("  site={}", site.display()));
        on_line(&format!("  退避前: {}", describe_package(&site, pkg)));
        let backup = backup_root.join(pkg);
        // 退避は「ディレクトリ」と「dist-info」の 2 段で、前者だけ動いて後者で失敗しうる
        // （ファイルがロックされている等）。**そのまま返すと site-packages から消えたまま**
        // になるので、ここでも戻す。
        if let Err(err) = move_package_aside(&site, pkg, &backup) {
            if let Err(restore_err) = restore_package(&site, &backup) {
                crate::ulog!(
                    "[irodori] 退避中の失敗を戻せません。退避を残します: {} ({restore_err:#})",
                    backup.display()
                );
                return Err(err).with_context(|| {
                    format!("復元にも失敗しました。退避先: {}", backup.display())
                });
            }
            let _ = std::fs::remove_dir_all(&backup_root);
            return Err(err);
        }
        on_line(&format!("  退避後: {}", describe_package(&site, pkg)));

        let installed = run_python(
            &py_exe,
            &[
                "-m",
                "pip",
                "install",
                "--no-warn-script-location",
                "--no-deps",
                // 直 URL でも確実に入れ替えるため、キャッシュと既存判定を跨がせない。
                "--force-reinstall",
                url,
            ],
            |l| on_line(l),
        )
        .and_then(|()| {
            on_line(&format!("  入れ直し後: {}", describe_package(&site, pkg)));
            let regressed = import_regressions(&before_imports, &import_report(&py_exe));
            if regressed.is_empty() {
                Ok(())
            } else {
                Err(anyhow!(
                    "入れ直したことで import できなくなりました: {}",
                    regressed.join(" / ")
                ))
            }
        });

        if let Err(err) = installed {
            on_line(&format!("{pkg} の入れ直しに失敗しました。元に戻します: {err:#}"));
            if let Err(restore_err) = restore_package(&site, &backup) {
                // **戻せなかったら退避を消さない。** 消すと、まさに守ろうとしたものを失う。
                crate::ulog!(
                    "[irodori] 復元に失敗しました。退避を残します: {} ({restore_err:#})",
                    backup.display()
                );
                return Err(err).with_context(|| {
                    format!("復元にも失敗しました。退避先: {}", backup.display())
                });
            }
            let _ = std::fs::remove_dir_all(&backup_root);
            return Err(err);
        }
        updated.push(name.clone());
    }

    // ここまで来たら全部成功している。退避を捨てる。
    let _ = std::fs::remove_dir_all(&backup_root);

    // **記録は更新の一部。** これをコマンド層に置いていたためテストから到達できず、
    // 「入れ直したのに `up_to_date` が false のまま」を自動で検出できなかった。
    record_after_install(asset_root, &updated, |l| on_line(l))
        .context("入れ直しは成功しましたが、導入記録を書けませんでした")?;

    Ok(updated)
}

/// 6) HF モデル本体を sidecar.py の `--download-only` モードで取得する (M4c Phase G)。
///
/// 通常のサイドカー起動経路 (`--no-download`) では DL を skip するように切り替えたため、
/// マルチGBのモデル取得は本ステップで完了させておく。`--download-only` モードは uvicorn を
/// 起動せず、download_models 完了で即終了する。stderr の `[hf-download] ...` 行は run_python
/// が on_line に流すので、`download_irodori_assets` の `irodori-download` event に伝わる。
pub async fn install_irodori_models<F>(
    asset_root: &Path,
    sidecar_py: &Path,
    mut on_line: F,
) -> Result<()>
where
    F: FnMut(&str),
{
    let py_exe = asset_root.join("python").join("python.exe");
    if !py_exe.is_file() {
        return Err(anyhow!(
            "python.exe が見つかりません: {}",
            py_exe.display()
        ));
    }
    if !sidecar_py.is_file() {
        return Err(anyhow!(
            "sidecar.py が配置されていません: {}",
            sidecar_py.display()
        ));
    }
    on_line("Aratako/Irodori-TTS の HF モデル (約 2〜4GB) を取得しています…");
    let asset_root_str = asset_root.to_string_lossy().into_owned();
    let sidecar_py_str = sidecar_py.to_string_lossy().into_owned();
    let args: Vec<&str> = vec![
        sidecar_py_str.as_str(),
        "--asset-dir",
        asset_root_str.as_str(),
        "--download-only",
    ];
    run_python(&py_exe, &args, |l| on_line(l))?;
    Ok(())
}

// ============ 内部ユーティリティ ============

/// `python311._pth` の `#import site` を `import site` に書き換える。
/// Embeddable Python は既定で site-packages を無効化しているのでこの編集が必須。
fn enable_site_packages(py_dir: &Path) -> Result<()> {
    let entries = std::fs::read_dir(py_dir)
        .with_context(|| format!("read python dir: {}", py_dir.display()))?;
    for e in entries.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with("._pth") {
            continue;
        }
        let content = std::fs::read_to_string(&p)
            .with_context(|| format!("read {}", p.display()))?;
        let patched = patch_pth(&content);
        std::fs::write(&p, patched)
            .with_context(|| format!("write {}", p.display()))?;
        return Ok(());
    }
    Err(anyhow!("python._pth が {} に見つかりません", py_dir.display()))
}

/// `_pth` の中身を「import site が有効」になるよう書き換える純粋関数 (テスト対象)。
fn patch_pth(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 16);
    let mut site_seen = false;
    for line in input.lines() {
        let trimmed = line.trim_start();
        if trimmed == "#import site" || trimmed == "# import site" {
            out.push_str("import site");
            site_seen = true;
        } else if trimmed == "import site" {
            out.push_str(line);
            site_seen = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !site_seen {
        out.push_str("import site\n");
    }
    out
}

/// HTTP GET でファイルに保存。リトライなしの単純実装 (Phase G で必要なら強化)。
async fn download_to(url: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent: {}", parent.display()))?;
    }
    let resp = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("status {url}"))?;
    let bytes = resp.bytes().await.with_context(|| format!("read body {url}"))?;
    let mut f = std::fs::File::create(dest)
        .with_context(|| format!("create {}", dest.display()))?;
    f.write_all(&bytes)
        .with_context(|| format!("write {}", dest.display()))?;
    Ok(())
}

/// Windows PowerShell の `Expand-Archive` で zip を展開。追加 crate なし。
/// PowerShell の単引用符文字列へ埋め込める形にする (v0.5.3)。
///
/// 単引用符の中では `'` を `''` と二重にするのが唯一のエスケープ。素通しすると
/// **`O'Neil` のようにアポストロフィを含むユーザー名のパスで引用が壊れ、導入が失敗する**
/// (Codex レビュー 2026-09-06)。パスはアプリ側が決めるため実害は限定的だが、
/// 子プロセスにコンソール窓を出させない。
///
/// **これが無いと、pip や Expand-Archive を呼ぶたびに黒いコンソール窓が前面に出る。**
/// v0.5.4 で「更新する」を押したときに実機で確認した。リリース版は
/// `windows_subsystem = "windows"` でコンソールを持たないため、子プロセスが
/// 自前で窓を割り当ててしまう。stdout/stderr のパイプはこのフラグでは変わらない。
/// （`notepad.exe` で取説を開く経路は、窓が出るのが目的なので対象外）
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 文字列へ埋め込む以上は正しく引用する。
fn ps_single_quoted(p: &Path) -> String {
    p.display().to_string().replace("'", "''")
}

fn expand_zip_windows(zip: &Path, dest: &Path) -> Result<()> {
    let cmd = format!(
        "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
        ps_single_quoted(zip),
        ps_single_quoted(dest)
    );
    let status = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &cmd])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| "Expand-Archive 起動失敗")?;
    if !status.success() {
        return Err(anyhow!(
            "Expand-Archive 異常終了 (code {:?})",
            status.code()
        ));
    }
    Ok(())
}

/// Python を 1 回起動して stdout/stderr を行単位で on_line に流す。
/// 終了コード != 0 で Err。標準出力は完了後に一括処理 (リアルタイムには出さない)。
fn run_python<F>(python_exe: &Path, args: &[&str], mut on_line: F) -> Result<()>
where
    F: FnMut(&str),
{
    let mut child = Command::new(python_exe)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("python 起動失敗: {}", python_exe.display()))?;

    // stdout/stderr を別スレッドで一括取得 (wait をブロックしないため)。
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let h_out = stdout.map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut s, &mut buf);
            buf
        })
    });
    let h_err = stderr.map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut s, &mut buf);
            buf
        })
    });

    let status = child.wait().with_context(|| "python 待機失敗")?;

    for h in [h_out, h_err].into_iter().flatten() {
        if let Ok(buf) = h.join() {
            for raw in buf.split(|b| *b == b'\n' || *b == b'\r') {
                if raw.is_empty() {
                    continue;
                }
                let s = String::from_utf8_lossy(raw);
                let t = s.trim();
                if !t.is_empty() {
                    on_line(t);
                }
            }
        }
    }

    if !status.success() {
        return Err(anyhow!("python 異常終了 (code {:?})", status.code()));
    }
    Ok(())
}

#[cfg(test)]
mod stamp_tests {
    use super::*;

    fn pins_of(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// **記録が無い環境を「最新」と言わない** (v0.5.4 項目 2)。
    ///
    /// v0.5.4 より前に導入した環境には記録が無い。そこを「一致」と読むと、
    /// **この機能が対象にしている当のユーザー**（pin が届いていない人）を取りこぼす。
    #[test]
    fn no_record_is_not_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        let st = status(dir.path());
        assert!(!st.has_record, "記録が無いこと");
        assert!(!st.up_to_date, "分からないものを最新とは言わない");
    }

    /// **「使える」と「最新」は別の信号** (v0.5.4 項目 2 の訂正)。
    ///
    /// 古いランタイムでも動いている以上 `present` は真のまま。ここを偽にすると、
    /// フロントの `canUseReal = gpuOk && assetsOk` が倒れ、ユーザーの
    /// `tts_irodori_use_real_model` が黙って false に書き換えられて永続化される。
    #[test]
    fn present_does_not_depend_on_being_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("python").join("Lib").join("site-packages");
        std::fs::create_dir_all(&site).unwrap();
        std::fs::write(dir.path().join("python").join("python.exe"), b"x").unwrap();
        for pkg in ["torch", "fastapi", "uvicorn", "huggingface_hub", "irodori_tts"] {
            std::fs::create_dir_all(site.join(pkg)).unwrap();
        }
        // 記録は無い（= 古い導入）が、パッケージは揃っている。
        let st = status(dir.path());
        assert!(st.present, "揃っているなら使える");
        assert!(!st.up_to_date, "記録が無いので最新とは言えない");
    }

    /// 記録した pin が現在の pin と一致すれば最新、違えばその名前を返す。
    #[test]
    fn outdated_pins_names_what_changed() {
        let recorded = pins_of(&[("python", "PY-1"), ("irodori_tts", "IR-1")]);
        let same = pins_of(&[("python", "PY-1"), ("irodori_tts", "IR-1")]);
        assert!(outdated_pins(&recorded, &same).is_empty());

        let bumped = pins_of(&[("python", "PY-1"), ("irodori_tts", "IR-2")]);
        assert_eq!(outdated_pins(&recorded, &bumped), ["irodori_tts"]);

        // pin を増やした場合も「入れ直しが要る」
        let added = pins_of(&[("python", "PY-1"), ("irodori_tts", "IR-1"), ("newdep", "N-1")]);
        assert_eq!(outdated_pins(&recorded, &added), ["newdep"]);

        // pin を減らした場合は入れ直し不要（記録側にしか無いものは無視する）
        let removed = pins_of(&[("python", "PY-1")]);
        assert!(outdated_pins(&recorded, &removed).is_empty());
    }

    /// 書いた記録を読み戻せること。**記録できても読めなければ記録ではない。**
    #[test]
    fn stamp_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_stamp(dir.path()).is_none(), "書く前は記録なし");

        let resolved = [("transformers", "4.57.6"), ("huggingface_hub", "0.36.2")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        write_stamp_pins(dir.path(), current_pins(), resolved).unwrap();

        let got = read_stamp(dir.path()).expect("読み戻せること");
        assert_eq!(got.pins, current_pins(), "要求した pin をそのまま記録する");
        assert_eq!(got.resolved.get("huggingface_hub").map(String::as_str), Some("0.36.2"));
        assert!(status(dir.path()).up_to_date, "書いた直後は最新");
    }

    /// **記録するのは「指定した版」ではなく「実際に入った版」** (v0.5.4)。
    ///
    /// `huggingface_hub==0.27.0` と指定しているのに実機は 0.36.2 だった
    /// （transformers の依存に押し上げられた）。この食い違いを記録できなければ、
    /// 「版を固定して再現性を担保」という宣言が成立していないことに気づけない。
    #[test]
    fn resolved_can_differ_from_the_requested_pin() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = [("huggingface_hub", "0.36.2")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        write_stamp_pins(dir.path(), current_pins(), resolved).unwrap();

        let got = read_stamp(dir.path()).unwrap();
        assert!(
            COMMON_REQUIREMENTS
                .iter()
                .any(|r| *r == "huggingface_hub==0.27.0"),
            "前提: 指定は 0.27.0"
        );
        assert_eq!(
            got.resolved.get("huggingface_hub").map(String::as_str),
            Some("0.36.2"),
            "実測値をそのまま残すこと"
        );
    }

    /// スキーマが違う記録は「記録なし」として扱う（読めない形を最新と誤認しない）。
    #[test]
    fn unknown_schema_is_treated_as_no_record() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(STAMP_FILE),
            r#"{"schema":999,"installed_at":0,"pins":{},"resolved":{}}"#,
        )
        .unwrap();
        assert!(read_stamp(dir.path()).is_none());
        assert!(!status(dir.path()).up_to_date);
    }

    /// 要件文字列から配布名を取り出す。
    #[test]
    fn requirement_name_strips_specifiers() {
        assert_eq!(requirement_name("torch>=2.10.0,<2.11.0"), "torch");
        assert_eq!(requirement_name("uvicorn[standard]==0.32.1"), "uvicorn");
        assert_eq!(requirement_name("numpy<2"), "numpy");
        assert_eq!(requirement_name("einops"), "einops");
        assert_eq!(requirement_name("descript-audiotools>=0.7.2"), "descript-audiotools");
    }

    /// 記録対象に、**pip で名前指定して入れたもの全部と GitHub 由来の 3 本**が入る。
    /// 取りこぼすと「何が入っているか」の記録として欠ける。
    #[test]
    fn recorded_distributions_cover_everything_we_install() {
        let names = recorded_distributions();
        for expected in ["torch", "transformers", "huggingface_hub", "fastapi", "numpy"] {
            assert!(names.iter().any(|n| n == expected), "{expected} が記録対象に無い");
        }
        for git in ["silentcipher", "dacvae", "irodori-tts"] {
            assert!(names.iter().any(|n| n == git), "{git} が記録対象に無い");
        }
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "重複がある: {names:?}");
    }

    /// python から返る解決済みバージョン行を読める。
    #[test]
    fn resolved_line_is_parsed() {
        let line = format!("{VERSIONS_MARKER}{{\"torch\":\"2.10.0+cu128\"}}");
        let got = parse_resolved_line(&line).expect("読めること");
        assert_eq!(got.get("torch").map(String::as_str), Some("2.10.0+cu128"));
        assert!(parse_resolved_line("pip install ...").is_none(), "無関係な行は拾わない");
    }

    /// **記録が無いことは「Python の版が違う」ことではない** (v0.5.4、実機検証の直前に発見)。
    ///
    /// 記録の欠落を「全 pin が古い」と読み替えて対象に `python` を混ぜていたため、
    /// 更新経路が冒頭の Err で止まり、**この機能が対象にしている環境
    /// （= v0.5.4 より前に導入した環境）がちょうど 1 つも更新できなかった**。
    /// しかもユーザーには「Python 本体の版が変わっています。全部消して入れ直せ」という
    /// 事実でない案内が出ていた。
    #[test]
    fn a_missing_record_does_not_accuse_python() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("python").join("Lib").join("site-packages");
        std::fs::create_dir_all(&site).unwrap();
        // 起動できない python.exe = 版を聞けない状況。聞けないなら古いとは言わない。
        std::fs::write(dir.path().join("python").join("python.exe"), b"x").unwrap();
        for pkg in ["torch", "fastapi", "uvicorn", "huggingface_hub", "irodori_tts"] {
            std::fs::create_dir_all(site.join(pkg)).unwrap();
        }

        let st = status(dir.path());
        assert!(st.present && !st.has_record && !st.up_to_date, "前提: 古い導入");
        assert!(
            !st.outdated.iter().any(|n| n == "python"),
            "聞けなかった python を対象に混ぜてはいけない: {:?}",
            st.outdated
        );
        for pkg in ["silentcipher", "dacvae", "irodori_tts"] {
            assert!(
                st.outdated.iter().any(|n| n == pkg),
                "{pkg} が入れ直しの対象から漏れている: {:?}",
                st.outdated
            );
        }
    }

    /// pin した URL から版を取り出せる（ここが壊れると python の判定が常に「聞けない」に倒れる）。
    #[test]
    fn pinned_python_version_comes_from_the_url() {
        assert_eq!(pinned_python_version(), Some("3.11.9"));
        assert!(PYTHON_URL.contains("python-3.11.9-embed-amd64.zip"), "前提が変わったら上も直す");
    }

    /// **証拠が無いほうへ倒す。** 聞けなかった版を「違う」と扱うと、直せもしない
    /// 全削除・再導入（数 GB）をユーザーに要求することになる。
    #[test]
    fn python_staleness_needs_evidence() {
        assert!(!python_is_stale(Some("3.11.9"), Some("3.11.9")), "一致なら古くない");
        assert!(python_is_stale(Some("3.11.8"), Some("3.11.9")), "違えば古い");
        assert!(!python_is_stale(None, Some("3.11.9")), "聞けないなら古いと言わない");
        assert!(!python_is_stale(Some("3.11.9"), None), "pin を読めないなら古いと言わない");
    }

    /// **初回導入の経路も、確認できない `python` を「最新」と記録しない**（監査 ④）。
    ///
    /// `ensure_python_embeddable` は `python.exe` があれば skip するので、
    /// 「入れた」と「入っている」は一致しない。ここで無条件に記録すると、以後
    /// `should_ask_python` が「記録が一致」と判断して実物に聞かなくなり、
    /// **古い python が永久に見えなくなる**。更新経路だけ直して隣に残していた穴。
    #[test]
    fn the_install_path_does_not_claim_an_unverified_python() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("python")).unwrap();
        // 起動できない python.exe = 版を確かめられない状況
        std::fs::write(dir.path().join("python").join("python.exe"), b"x").unwrap();

        record_installed(dir.path(), |_| {}).expect("記録は書けること");

        let pins = read_stamp(dir.path()).expect("記録があること").pins;
        assert!(
            !pins.contains_key("python"),
            "確かめられていない python を記録してはいけない: {pins:?}"
        );
        for pkg in ["dacvae", "irodori_tts", "silentcipher"] {
            assert_eq!(pins.get(pkg), current_pins().get(pkg), "{pkg} は記録すること");
        }
        assert!(
            should_ask_python(&pins),
            "記録していないのだから、次回は実物に聞きにいくこと"
        );
    }

    /// **入れ直せた分だけ**を現在値へ反映する。全部を現在値にすると、入れ直していない
    /// 依存まで「最新」と記録して記録そのものが嘘になる。
    #[test]
    fn only_updated_pins_are_recorded() {
        let updated = vec!["dacvae".to_string()];
        let pins = merged_pins(&Default::default(), &updated, false);
        assert_eq!(pins.len(), 1, "入れ直した 1 本だけ: {pins:?}");
        assert_eq!(pins.get("dacvae"), current_pins().get("dacvae"));
        assert!(!pins.contains_key("python"), "確認していない python を書かない");
        assert!(!pins.contains_key("irodori_tts"), "入れ直していないものを書かない");

        // 実物が pin と一致していると確認できたときだけ python も記録する
        let pins = merged_pins(&Default::default(), &updated, true);
        assert_eq!(pins.get("python"), current_pins().get("python"));

        // 3 本入れ直し + python 確認済み = 最新になる
        let all: Vec<String> = ["dacvae", "irodori_tts", "silentcipher"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let pins = merged_pins(&Default::default(), &all, true);
        assert!(
            outdated_pins(&pins, &current_pins()).is_empty(),
            "全部入れ直したら最新になること: {pins:?}"
        );
    }

    /// 一致している記録は信じ、**実物に聞かない**。ここが常に真になると、設定パネルを
    /// 開くたびに python.exe を起動することになる。
    #[test]
    fn a_matching_record_means_python_need_not_be_asked() {
        assert!(!should_ask_python(&current_pins()), "一致している記録は信じる");
        assert!(should_ask_python(&Default::default()), "記録が無ければ実物に聞く");
        let mut other = current_pins();
        other.insert("python".to_string(), "OLD".to_string());
        assert!(should_ask_python(&other), "記録が違えば実物に聞く");
    }

    /// 記録どおりなら空、違う pin だけを名指しする。
    #[test]
    fn outdated_list_names_only_what_changed() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorded = current_pins();
        assert!(outdated_list(dir.path(), &recorded).is_empty(), "全部一致なら空");
        recorded.insert("irodori_tts".to_string(), "OLD".to_string());
        assert_eq!(outdated_list(dir.path(), &recorded), ["irodori_tts"]);
    }

    /// **pin を増やしたら `current_pins` にも足す。** 足し忘れると、その依存だけ
    /// 更新判定から外れて「最新」と誤認する。
    #[test]
    fn pins_cover_every_pinned_url() {
        let pins = current_pins();
        assert_eq!(pins.len(), 4, "pin を増減したらここも更新する: {pins:?}");
        for url in pins.values() {
            assert!(url.starts_with("https://"), "URL でない: {url}");
        }
        assert!(pins.values().any(|u| u == IRODORI_TTS_ZIPBALL));
    }
}

#[cfg(test)]
mod update_tests {
    use super::*;

    fn make_site(dir: &Path) -> PathBuf {
        let site = dir.join("python").join("Lib").join("site-packages");
        std::fs::create_dir_all(&site).unwrap();
        site
    }

    fn put_pkg(site: &Path, pkg: &str, version: &str, body: &str) {
        std::fs::create_dir_all(site.join(pkg)).unwrap();
        std::fs::write(site.join(pkg).join("__init__.py"), body).unwrap();
        let di = site.join(format!("{pkg}-{version}.dist-info"));
        std::fs::create_dir_all(&di).unwrap();
        std::fs::write(di.join("METADATA"), format!("Version: {version}")).unwrap();
    }

    /// **Python 本体は入れ直しの対象にしない。**
    ///
    /// `ensure_python_embeddable` は `python.exe` があれば skip するので、
    /// pin を上げても反映されない。稼働中のインタプリタを安全に差し替える方法は
    /// 無いため、黙って部分更新して「最新」と記録するのではなく対象外にする。
    /// **実機検証用** (v0.5.4 項目 3、test-plan §5 E-7)。**実環境を書き換える**ので
    /// 既定では走らない。対象を環境変数で明示させ、うっかり実行できないようにしている。
    ///
    /// ```powershell
    /// $env:UGG_IRODORI_REAL_ROOT = "$env:APPDATA\\ugg\\irodori"
    /// cargo test -- --ignored --nocapture irodori_update_on_a_real_runtime
    /// ```
    ///
    /// 確かめるのは「入った」ではなく「**使える**」まで: `update_irodori_runtime` は
    /// import 確認を通ってから退避を捨てるので、成功して戻ればその時点で import できている。
    #[tokio::test]
    #[ignore = "実環境を書き換える。UGG_IRODORI_REAL_ROOT を指定して明示的に実行する"]
    async fn irodori_update_on_a_real_runtime() {
        let Ok(root) = std::env::var("UGG_IRODORI_REAL_ROOT") else {
            panic!("UGG_IRODORI_REAL_ROOT が未設定です（対象を明示すること）");
        };
        let root = PathBuf::from(root);
        assert!(
            root.join("python").join("python.exe").is_file(),
            "python.exe が無い: {}",
            root.display()
        );

        // **実行の証跡をファイルへ残す。** 実機検証の出力は端末の履歴と見分けがつかず、
        // 2026-09-11 に古い出力を新しい実行と取り違えて 2 往復を空費した。
        let log_path = std::env::temp_dir().join("ugg-e7-verify.log");
        let _ = std::fs::write(&log_path, "");
        let log = log_path.clone();
        let say = move |line: String| {
            use std::io::Write;
            println!("{line}");
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
                let _ = writeln!(f, "{line}");
            }
        };
        say(format!(
            "[harness] E-7 rev2 差分ゲート / {} / log={}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            log_path.display()
        ));

        let before_imports = import_report(&root.join("python").join("python.exe"));
        say(format!("[before] imports={before_imports:?}"));
        let before = status(&root);
        say(format!(
            "[before] present={} has_record={} up_to_date={}",
            before.present, before.has_record, before.up_to_date
        ));
        say(format!("[before] outdated={:?}", before.outdated));
        say(format!("[before] python={:?}", installed_python_version(&root)));
        assert!(before.present, "前提: 使える状態であること");
        assert!(!before.up_to_date, "前提: 更新対象があること（既に最新なら検証にならない）");
        assert!(
            !before.outdated.iter().any(|n| n == "python"),
            "python が対象に混ざっている: {:?}",
            before.outdated
        );

        let outcome = update_irodori_runtime(&root, &before.outdated, |l| say(format!("  | {l}"))).await;
        let updated = match outcome {
            Ok(v) => {
                say(format!("[updated] {v:?}"));
                v
            }
            Err(e) => {
                // 失敗そのものは想定内。**失敗したときに何が残ったか**まで証跡に出す。
                say(format!("[failed] {e:#}"));
                say(format!(
                    "[failed] backup_left={} stamp_written={}",
                    root.join(UPDATE_BACKUP_DIR).exists(),
                    root.join(STAMP_FILE).exists()
                ));
                say(format!(
                    "[failed] imports={:?}",
                    import_report(&root.join("python").join("python.exe"))
                ));
                panic!("入れ直しに失敗: {e:#}");
            }
        };
        assert_eq!(updated, before.outdated, "対象が全部入れ直されること");
        assert!(
            !root.join(UPDATE_BACKUP_DIR).exists(),
            "成功したら退避を残さない: {}",
            root.join(UPDATE_BACKUP_DIR).display()
        );
        let after = import_report(&root.join("python").join("python.exe"));
        say(format!("[after] imports={after:?}"));
        let after_status = status(&root);
        say(format!("[after] status={after_status:?}"));
        say(format!(
            "[after] installed.json={}",
            std::fs::read_to_string(root.join(STAMP_FILE))
                .unwrap_or_else(|e| format!("(読めません: {e})"))
        ));
        for pkg in ["dacvae", "irodori_tts", "silentcipher"] {
            say(format!("[after] {pkg}: {}", describe_package(&site_of(&root), pkg)));
        }
        assert!(after_status.has_record, "記録が書かれていること");
        assert!(
            after_status.resolved.len() > 1,
            "実際に入った版を記録しきること（python だけでは記録の意味が無い）: {:?}",
            after_status.resolved
        );
        assert!(
            after_status.up_to_date,
            "入れ直しきったら最新になること: outdated={:?}",
            after_status.outdated
        );
        assert!(
            import_regressions(&before_imports, &after).is_empty(),
            "入れ直す前に使えていたものが使えなくなっている"
        );
    }

    #[test]
    fn python_is_not_updatable_in_place() {
        assert!(updatable_pin("python").is_none());
        for name in ["silentcipher", "dacvae", "irodori_tts"] {
            assert!(updatable_pin(name).is_some(), "{name} は入れ直せるはず");
        }
        // current_pins の 4 件のうち、入れ直せるのは 3 件。
        let updatable = current_pins()
            .keys()
            .filter(|n| updatable_pin(n).is_some())
            .count();
        assert_eq!(updatable, 3, "pin を増やしたら updatable_pin にも足すこと");
    }

    /// 退避 → 復元で、**中身も dist-info も元どおりになる**。
    /// pip は「消してから入れる」ので、ここが戻らないと失敗時に環境が壊れる。
    fn report(pairs: &[(&str, Option<&str>)]) -> std::collections::BTreeMap<String, Option<String>> {
        pairs
            .iter()
            .map(|(m, e)| (m.to_string(), e.map(|x| x.to_string())))
            .collect()
    }

    /// **元から壊れていたものを理由に巻き戻さない** (v0.5.4、実機検証で発覚)。
    ///
    /// 実機の `silentcipher` は `pydub` が無く一度も import できていない（upstream が
    /// 意図的に任意依存にしており、合成は透かし無しで成立する）。最初の実装は
    /// 「3 つとも import できること」を絶対条件にしていたため、**既存環境では必ず
    /// ロールバックし、更新が誰にも一度も成功しない**状態だった。
    #[test]
    fn only_a_regression_blocks_the_update() {
        let before = report(&[
            ("irodori_tts", None),
            ("dacvae", None),
            ("silentcipher", Some("ModuleNotFoundError: No module named 'pydub'")),
        ]);

        // 元から壊れていたものが壊れたままでも、それは入れ直しの失敗ではない
        assert!(
            import_regressions(&before, &before).is_empty(),
            "前から import できないものを理由に巻き戻してはいけない"
        );

        // 直っていたらなおよい（改善を失敗として数えない）
        let fixed = report(&[("irodori_tts", None), ("dacvae", None), ("silentcipher", None)]);
        assert!(import_regressions(&before, &fixed).is_empty());

        // 使えていたものが使えなくなったら、それは失敗。理由も添える
        let broken = report(&[
            ("irodori_tts", Some("ImportError: bad")),
            ("dacvae", None),
            ("silentcipher", Some("ModuleNotFoundError: No module named 'pydub'")),
        ]);
        let got = import_regressions(&before, &broken);
        assert_eq!(got.len(), 1, "壊れたのは 1 つ: {got:?}");
        assert!(got[0].starts_with("irodori_tts: "), "どれが壊れたか: {got:?}");
        assert!(got[0].contains("ImportError: bad"), "理由を落とさない: {got:?}");

        // 調べられなくなった（消えた）場合も悪化として扱う
        let gone = report(&[("dacvae", None), ("silentcipher", None)]);
        assert_eq!(import_regressions(&before, &gone), ["irodori_tts"]);
    }

    /// 見張る対象に、入れ直しうる 3 本が揃っていること。
    #[test]
    fn runtime_modules_cover_every_updatable_pin() {
        for name in current_pins().keys() {
            let Some((pkg, _)) = updatable_pin(name) else { continue };
            assert!(
                RUNTIME_MODULES.contains(&pkg),
                "{pkg} を入れ直すのに import の見張りが無い"
            );
        }
    }

    /// 退避が**途中まで**進んだ状態から戻せること（監査 ③）。
    ///
    /// 退避は「ディレクトリ」と「dist-info」の 2 段で、前者だけ動いて後者で失敗しうる。
    /// そのまま返すと site-packages からパッケージが消えたまま戻らない。
    #[test]
    fn a_half_done_aside_can_be_restored() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        put_pkg(&site, "dacvae", "1.0.0", "keep");
        let backup = dir.path().join(UPDATE_BACKUP_DIR).join("dacvae");
        // dist-info の移動先を中身つきで塞ぐ（Windows の rename はここで失敗する）
        std::fs::create_dir_all(backup.join("dacvae-1.0.0.dist-info").join("blocker")).unwrap();

        assert!(
            move_package_aside(&site, "dacvae", &backup).is_err(),
            "前提: 途中で失敗すること"
        );
        assert!(!site.join("dacvae").exists(), "前提: ディレクトリだけ先に動いている");

        restore_package(&site, &backup).expect("戻せること");
        assert_eq!(
            std::fs::read_to_string(site.join("dacvae").join("__init__.py")).unwrap(),
            "keep",
            "消えたままにしない"
        );
    }

    /// **前回「守るために残した」退避を、次の実行が消してはいけない**（監査 ②）。
    ///
    /// 復元に失敗したときは退避を残してユーザーに場所を伝える。そこにあるのは
    /// **唯一残った旧版**なので、次の実行が冒頭で消すと、入れ直しに失敗した瞬間に
    /// 永久に失われる。残っていたらまず戻す。
    #[tokio::test]
    async fn a_leftover_backup_is_restored_before_starting() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        std::fs::write(dir.path().join("python").join("python.exe"), b"x").unwrap();

        // 前回の中断で退避だけが残っている状態を作る
        let backup = dir.path().join(UPDATE_BACKUP_DIR).join("dacvae");
        std::fs::create_dir_all(backup.join("dacvae")).unwrap();
        std::fs::write(backup.join("dacvae").join("__init__.py"), b"old").unwrap();
        std::fs::create_dir_all(backup.join("dacvae-1.0.0.dist-info")).unwrap();
        assert!(!site.join("dacvae").exists(), "前提: site からは消えている");

        update_irodori_runtime(dir.path(), &[], |_| {})
            .await
            .expect("入れ直す対象が無くても、戻しは行われること");

        assert_eq!(
            std::fs::read_to_string(site.join("dacvae").join("__init__.py")).unwrap(),
            "old",
            "退避していた旧版が site へ戻ること"
        );
        assert!(site.join("dacvae-1.0.0.dist-info").is_dir(), "dist-info も戻ること");
        assert!(
            !dir.path().join(UPDATE_BACKUP_DIR).exists(),
            "戻せたら退避は捨てる"
        );
    }

    /// 導入と更新を同時に走らせない（監査 ①）。
    ///
    /// 同じ `site-packages` を 2 経路が触ると、退避 → 入れ直し → 復元のどの段も守れない。
    #[test]
    fn install_and_update_do_not_overlap() {
        let first = IrodoriBusyGuard::acquire().expect("1 本目は取れる");
        assert!(
            IrodoriBusyGuard::acquire().is_err(),
            "進行中はもう 1 本走らせない"
        );
        drop(first);
        assert!(
            IrodoriBusyGuard::acquire().is_ok(),
            "終わったら次が取れる（途中で return しても取り残さない）"
        );
    }

    #[test]
    fn move_aside_then_restore_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        put_pkg(&site, "irodori_tts", "0.1.0", "OLD");
        let backup = dir.path().join(UPDATE_BACKUP_DIR).join("irodori_tts");

        move_package_aside(&site, "irodori_tts", &backup).unwrap();
        assert!(!site.join("irodori_tts").exists(), "退避後は元の場所に無い");
        assert!(!site.join("irodori_tts-0.1.0.dist-info").exists());

        restore_package(&site, &backup).unwrap();
        assert_eq!(
            std::fs::read_to_string(site.join("irodori_tts").join("__init__.py")).unwrap(),
            "OLD",
            "中身が戻っていない"
        );
        assert!(
            site.join("irodori_tts-0.1.0.dist-info").join("METADATA").is_file(),
            "dist-info が戻っていない"
        );
    }

    /// **失敗した入れ直しが中途半端に残していても復元できる。**
    /// pip が新しい版を途中まで書いた状態から、旧版へ戻せること。
    #[test]
    fn restore_overwrites_a_half_written_install() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        put_pkg(&site, "dacvae", "1.0.0", "OLD");
        let backup = dir.path().join(UPDATE_BACKUP_DIR).join("dacvae");
        move_package_aside(&site, "dacvae", &backup).unwrap();

        // 入れ直しが途中で落ちて、新しい版の残骸が居座っている状態を作る。
        std::fs::create_dir_all(site.join("dacvae")).unwrap();
        std::fs::write(site.join("dacvae").join("__init__.py"), "HALF").unwrap();

        restore_package(&site, &backup).unwrap();
        assert_eq!(
            std::fs::read_to_string(site.join("dacvae").join("__init__.py")).unwrap(),
            "OLD",
            "残骸を退けて旧版に戻すこと"
        );
    }

    /// 入っていないパッケージの退避は失敗にしない（新規に入る場合）。
    #[test]
    fn moving_a_missing_package_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        let backup = dir.path().join(UPDATE_BACKUP_DIR).join("silentcipher");
        move_package_aside(&site, "silentcipher", &backup).expect("失敗にしない");
    }

    /// 似た名前のパッケージを巻き込まない（`dacvae` の退避が `dacvae_extra` を持っていかない）。
    #[test]
    fn move_aside_does_not_touch_similarly_named_packages() {
        let dir = tempfile::tempdir().unwrap();
        let site = make_site(dir.path());
        put_pkg(&site, "dacvae", "1.0.0", "TARGET");
        put_pkg(&site, "dacvae_extra", "2.0.0", "BYSTANDER");
        let backup = dir.path().join(UPDATE_BACKUP_DIR).join("dacvae");

        move_package_aside(&site, "dacvae", &backup).unwrap();
        assert!(
            site.join("dacvae_extra").join("__init__.py").is_file(),
            "無関係なパッケージを巻き込んでいる"
        );
        assert!(site.join("dacvae_extra-2.0.0.dist-info").is_dir());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_pth_uncomments_import_site() {
        let input = "python311.zip\n.\n\n# Uncomment to run site.main() automatically\n#import site\n";
        let out = patch_pth(input);
        assert!(out.contains("\nimport site\n"));
        assert!(!out.contains("#import site"));
    }

    #[test]
    fn patch_pth_handles_space_before_site() {
        let input = "python311.zip\n.\n# import site\n";
        let out = patch_pth(input);
        // 行頭の "# import site" も import site に置換される
        assert!(out.contains("\nimport site\n"));
        assert!(!out.contains("# import site"));
    }

    #[test]
    fn patch_pth_idempotent_when_already_enabled() {
        let input = "python311.zip\n.\nimport site\n";
        let out = patch_pth(input);
        // 既に有効化されている場合は重複追加しない
        let count = out.matches("\nimport site").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn patch_pth_appends_when_missing() {
        let input = "python311.zip\n.\n";
        let out = patch_pth(input);
        assert!(out.contains("\nimport site\n"));
    }

    #[test]
    fn assets_ready_false_when_python_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(!assets_ready(tmp.path()));
    }
}

#[cfg(test)]
mod quoting_tests {
    use super::*;

    /// **PowerShell の単引用符は 2 つ重ねて escape する (v0.5.3)。**
    /// 素通しすると `O'` のようなユーザー名のパスで引用が壊れ、導入が失敗する
    /// (Codex レビュー 2026-09-06)。
    #[test]
    fn apostrophe_in_path_is_doubled() {
        let p = Path::new(r"C:\Users\O'Neil\AppData\Local\ugg");
        let q = ps_single_quoted(p);
        assert!(q.contains("O''Neil"), "escape されていない: {q}");
        // 単引用符が偶数個 = PowerShell の文字列が途中で閉じない。
        assert_eq!(q.matches('\'').count() % 2, 0, "引用が閉じない: {q}");
    }

    #[test]
    fn ordinary_path_is_unchanged() {
        let p = Path::new(r"C:\Users\shiho\AppData\Local\ugg");
        assert_eq!(ps_single_quoted(p), r"C:\Users\shiho\AppData\Local\ugg");
    }
}

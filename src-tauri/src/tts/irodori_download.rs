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

/// 導入記録を書く。**導入がすべて成功した後にだけ呼ぶ。**
/// 途中で失敗した状態に記録を残すと、次回「入っている」と誤認する。
pub fn write_stamp(
    asset_root: &Path,
    resolved: std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let stamp = InstalledStamp {
        schema: STAMP_SCHEMA,
        installed_at: chrono::Utc::now().timestamp(),
        pins: current_pins(),
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
    let py_exe = asset_root.join("python").join("python.exe");
    let resolved = query_resolved_versions(&py_exe, |l| on_line(l));
    write_stamp(asset_root, resolved)?;
    on_line("導入内容を記録しました");
    Ok(())
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

/// 導入状態を調べる (v0.5.4 項目 2)。
pub fn status(asset_root: &Path) -> IrodoriStatus {
    let present = assets_ready(asset_root);
    let Some(stamp) = read_stamp(asset_root) else {
        return IrodoriStatus {
            present,
            has_record: false,
            up_to_date: false,
            // 記録が無い環境では、どの pin が古いかまでは言えない。
            outdated: Vec::new(),
            resolved: Default::default(),
        };
    };
    let outdated = outdated_pins(&stamp.pins, &current_pins());
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
        write_stamp(dir.path(), resolved).unwrap();

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
        write_stamp(dir.path(), resolved).unwrap();

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

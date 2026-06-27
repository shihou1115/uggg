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
];

/// silentcipher (Sesame AI Labs) の固定 commit zipball。upstream Irodori-TTS pyproject の
/// `silentcipher @ git+https://github.com/SesameAILabs/silentcipher.git@<hash>` と同じ hash。
const SILENTCIPHER_ZIPBALL: &str =
    "https://github.com/SesameAILabs/silentcipher/archive/d46d7d0893a583d8968ab3a6626e2289faec9152.zip";

/// dacvae (facebookresearch) の main ブランチ zipball。upstream Irodori-TTS pyproject の
/// `dacvae = { git = "https://github.com/facebookresearch/dacvae" }` と同じリポジトリ。
const DACVAE_ZIPBALL: &str =
    "https://github.com/facebookresearch/dacvae/archive/refs/heads/main.zip";

/// Irodori-TTS 本体 (Aratako) の main ブランチ zipball。`infer.py` / `irodori_tts.inference_runtime`
/// を提供する。最新コードを追うため main ブランチを採用 (将来 release タグが切られたら pin する)。
const IRODORI_TTS_ZIPBALL: &str =
    "https://github.com/Aratako/Irodori-TTS/archive/refs/heads/main.zip";

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
fn expand_zip_windows(zip: &Path, dest: &Path) -> Result<()> {
    let cmd = format!(
        "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
        zip.display(),
        dest.display()
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

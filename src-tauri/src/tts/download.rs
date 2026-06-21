//! voicevox_core 資産の初回ダウンロード。
//!
//! 公式ダウンローダ (`download-windows-x64.exe`, 0.16.4) を GitHub Releases から取得して
//! 実行し、`%APPDATA%\ugg\voicevox\` 配下に `voicevox_core.dll` / onnxruntime / 辞書 / *.vvm
//! を展開する (CPU 版)。これでユーザーは別アプリ導入や手動配置が不要。
//!
//! - c-api バージョンは FFI と一致させる (0.16.4)。
//! - 利用規約への対話的同意は事前に UI で確認済みなので stdin に `y\n` を流す。
//! - GitHub API レート制限緩和のため、ユーザーが PAT を設定していれば GH_TOKEN として渡す。
//! - 既存 dll は使用中で削除不可なケースがあるため `.dll.old-N` に退避してから上書き。
//! - stderr を行単位で `voicevox-download` イベントに emit。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// FFI バインディングと一致させる C API バージョン。
pub const CAPI_VERSION: &str = "0.16.4";

/// 公式ダウンローダ (Windows x64, 0.16.4)。
const DOWNLOADER_URL: &str =
    "https://github.com/VOICEVOX/voicevox_core/releases/download/0.16.4/download-windows-x64.exe";

/// ダウンローダ実行ファイルの保存先。
pub fn downloader_path(asset_dir: &Path) -> PathBuf {
    asset_dir.join("voicevox-downloader.exe")
}

/// 公式ダウンローダをキャッシュ取得 (既にあれば再 DL しない)。
pub async fn ensure_downloader(asset_dir: &Path) -> Result<PathBuf, String> {
    let path = downloader_path(asset_dir);
    if path.is_file() {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("資産ディレクトリの作成に失敗: {e}"))?;
    }
    let bytes = reqwest::Client::new()
        .get(DOWNLOADER_URL)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("ダウンローダ取得に失敗: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("ダウンローダ受信に失敗: {e}"))?;
    std::fs::write(&path, &bytes).map_err(|e| format!("ダウンローダ保存に失敗: {e}"))?;
    Ok(path)
}

/// ダウンローダを実行して資産を `asset_dir` 配下に展開する (ブロッキング)。
/// `on_line` には stderr の進捗行が渡る (ANSI エスケープ除去・改行で分割済み)。
pub fn run_downloader(
    downloader: &Path,
    asset_dir: &Path,
    gh_token: Option<&str>,
    mut on_line: impl FnMut(&str),
) -> Result<(), String> {
    std::fs::create_dir_all(asset_dir).map_err(|e| format!("出力先の作成に失敗: {e}"))?;
    // 稼働中の DLL が残っていると上書きに失敗する。rename は通るので退避する。
    stash_locked_dlls(asset_dir);

    let mut cmd = Command::new(downloader);
    cmd.arg("-o")
        .arg(asset_dir)
        .args(["--c-api-version", CAPI_VERSION])
        .args(["--devices", "cpu"])
        .args(["--exclude", "additional-libraries"])
        // トーク用 VVM のみ (ソング用 s*.vvm 除外で軽量化)。
        .args(["--models-pattern", "[0-9]*.vvm"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(t) = gh_token {
        let t = t.trim();
        if !t.is_empty() {
            cmd.env("GH_TOKEN", t);
        }
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("ダウンローダ起動に失敗: {e}"))?;

    // 利用規約への対話的同意。ユーザーは UI で事前同意済み (download_voicevox_assets の agreed ガード)。
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"y\ny\ny\ny\ny\n");
        let _ = stdin.flush();
    }

    let mut detected: Option<String> = None;
    let mut rate_limited = false;
    if let Some(mut err) = child.stderr.take() {
        let mut buf = Vec::new();
        err.read_to_end(&mut buf)
            .map_err(|e| format!("ダウンローダ出力の読み取りに失敗: {e}"))?;
        for raw in buf.split(|b| *b == b'\n' || *b == b'\r') {
            if raw.is_empty() {
                continue;
            }
            let s = String::from_utf8_lossy(raw);
            let clean = strip_ansi(&s);
            let t = clean.trim();
            if t.is_empty() {
                continue;
            }
            if t.contains("API rate limit exceeded") {
                rate_limited = true;
            } else if detected.is_none() && t.starts_with("Error:") {
                detected = Some(t.to_string());
            }
            on_line(t);
        }
    }
    if rate_limited {
        detected = Some(
            "GitHub API のレート制限に達しました。1 時間ほど待つか、設定の \
             GitHub PAT を入れて再試行してください"
                .to_string(),
        );
    }

    let status = child
        .wait()
        .map_err(|e| format!("ダウンローダの終了待ちに失敗: {e}"))?;
    if !status.success() {
        return Err(detected.unwrap_or_else(|| {
            format!(
                "ダウンローダが異常終了しました (コード {:?})",
                status.code()
            )
        }));
    }
    Ok(())
}

fn stash_locked_dlls(asset_dir: &Path) {
    let candidates = ["voicevox_core.dll", "voicevox_onnxruntime.dll"];
    let mut stack = vec![asset_dir.to_path_buf()];
    let mut found = Vec::new();
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if candidates.iter().any(|c| c.eq_ignore_ascii_case(name)) {
                    found.push(p);
                }
            }
        }
    }
    for dll in found {
        for i in 0u32..100 {
            let stashed = dll.with_extension(format!("dll.old-{i}"));
            if !stashed.exists() {
                let _ = std::fs::rename(&dll, &stashed);
                break;
            }
        }
    }
}

fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            while i < bytes.len() {
                let b = bytes[i];
                i += 1;
                if (0x40..=0x7E).contains(&b) {
                    break;
                }
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

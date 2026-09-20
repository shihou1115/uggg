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
//! - stderr を行単位で `voicevox-download` イベントに emit。**届いたときに流す**（v0.5.6 項目 2。
//!   以前は終わってからまとめて読んでいた）。無進捗が続けば止める（`tts::child_process`）。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::tts::child_process::{self, Ended, Stream};

/// 無進捗とみなすまでの時間（v0.5.6 項目 2）。出力・読み書き・CPU のどれも無い時間で数える
/// （`child_process`）。`irodori_download` の `PYTHON_STALL_AFTER` と同じ考え方。
const DOWNLOADER_STALL_AFTER: Duration = Duration::from_secs(5 * 60);

/// GitHub API のレート制限に当たったときの案内（ダウンローダの出力から判定する）。
const RATE_LIMITED: &str = "GitHub API のレート制限に達しました。1 時間ほど待つか、設定の \
     GitHub PAT を入れて再試行してください";

/// FFI バインディングと一致させる C API バージョン。
pub const CAPI_VERSION: &str = "0.16.4";

/// 公式ダウンローダ (Windows x64, 0.16.4)。
const DOWNLOADER_URL: &str =
    "https://github.com/VOICEVOX/voicevox_core/releases/download/0.16.4/download-windows-x64.exe";

/// 資産の取得に使う HTTP クライアント（v0.5.6 項目 2）。
///
/// **上限を付ける。** 既定の reqwest は接続も読み取りも待ち続けるので、相手が生きたまま黙ると
/// 取得が終わらない（子プロセスの無進捗の中断と同じ形が、通信の側に残っていた）。全体の上限は
/// 付けない — 回線が遅いだけの正常な取得を切ってしまう。`read_timeout` は読めるたびに数え直す。
pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

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
    let bytes = http_client()
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
        .args(["--models-pattern", "[0-9]*.vvm"]);
    if let Some(t) = gh_token {
        let t = t.trim();
        if !t.is_empty() {
            cmd.env("GH_TOKEN", t);
        }
    }

    let mut detected: Option<String> = None;
    let mut rate_limited = false;
    // 利用規約への対話的同意は標準入力で渡す。ユーザーは UI で事前同意済み
    // (download_voicevox_assets の agreed ガード)。進捗は stderr に出る（stdout は以前から見ていない）。
    // 窓を出さない指定と、文字コードの判定（UTF-8 で読めなければ Shift_JIS）、色付けの制御文字を
    // 落とすのは `run_streaming` が持つ（**以前はここで 1 バイトずつ文字に積み直しており、日本語の
    // エラー文が化けていた**。v0.5.6 項目 2）。
    let ended = child_process::run_streaming(
        cmd,
        Some(b"y\ny\ny\ny\ny\n"),
        Some(DOWNLOADER_STALL_AFTER),
        |line| {
            if line.stream != Stream::Stderr {
                return;
            }
            let t = line.text;
            if t.contains("API rate limit exceeded") {
                rate_limited = true;
            } else if detected.is_none() && t.starts_with("Error:") {
                detected = Some(t.to_string());
            }
            on_line(t);
        },
    )
    .map_err(|e| format!("ダウンローダ起動に失敗: {e}"))?;
    let status = match ended {
        Ended::Exited(status) => status,
        Ended::Stalled => {
            // 止めたときも、それまでに読み取った理由を捨てない（レート制限に当たったあとで
            // 黙り込む場合、PAT を入れる案内が消えてしまう）。
            let why = if rate_limited {
                format!("（{RATE_LIMITED}）")
            } else {
                detected
                    .map(|d| format!("（直前の出力: {d}）"))
                    .unwrap_or_default()
            };
            return Err(format!(
                "ダウンローダが {} 分間、出力も読み書きもしないまま止まっていたので中断しました{why}",
                DOWNLOADER_STALL_AFTER.as_secs() / 60
            ));
        }
    };
    if rate_limited {
        detected = Some(RATE_LIMITED.to_string());
    }

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


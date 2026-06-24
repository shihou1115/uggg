//! Irodori-TTS Python サイドカーの起動・停止・通信路解決 (architecture §8.4, M4c Phase D)。
//!
//! - 起動: `python.exe sidecar.py --asset-dir ... --ready-file ... --port 0 [--mock]`
//! - ポート解決: sidecar.py が動的割当ポートを `ready.json` に書き出すまで polling
//! - 停止: `POST /shutdown` → 1 秒待って `child.kill()` でフォールバック
//!
//! ヘルスチェックの定期監視 / アイドル監視 (5 分で自動 kill) は Phase E で `tasks.rs`
//! の `spawn_*_watcher` 群に並べる予定。本ファイルは「立ち上げて port を得て、終わったら殺す」
//! 最小機能のみ提供する。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::process::{Child, Command};
use tokio::time::sleep;

use crate::tts::irodori_download;

/// 起動済みサイドカーの参照。drop しても子プロセスは生き続けるので、明示的に
/// [`shutdown_sidecar`] を呼ぶこと (Phase E の `quit_app` フックで一括処理)。
#[derive(Debug)]
pub struct SidecarHandle {
    pub port: u16,
    /// Phase E のヘルスチェック失敗時に PID を出してデバッグログに使う想定。
    #[allow(dead_code)]
    pub pid: u32,
    /// `wait()` を呼ばずに保持し続けるとゾンビ化するため、`shutdown_sidecar` で wait する。
    pub child: Child,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReadyFile {
    port: u16,
    pid: u32,
}

/// 既定の `ready.json` 配置先 (asset_root/ready.json)。
fn ready_path_for(asset_root: &Path) -> PathBuf {
    asset_root.join("ready.json")
}

/// 同梱 `sidecar.py` を `%APPDATA%\ugg\irodori\sidecar.py` にコピーする。
/// `resource_dir/python/sidecar.py` を上書き配置。
pub fn install_sidecar_script(resource_dir: &Path, asset_root: &Path) -> Result<PathBuf> {
    let src = resource_dir.join("python").join("sidecar.py");
    if !src.is_file() {
        return Err(anyhow!(
            "sidecar.py がリソースに見つかりません: {}",
            src.display()
        ));
    }
    std::fs::create_dir_all(asset_root)
        .with_context(|| format!("create asset_root: {}", asset_root.display()))?;
    let dest = asset_root.join("sidecar.py");
    std::fs::copy(&src, &dest)
        .with_context(|| format!("copy {} -> {}", src.display(), dest.display()))?;
    Ok(dest)
}

/// サイドカーを起動して ready.json を待つ。
///
/// `mock=true` の場合、Aratako モデルを使わずモック wav を返すモードで起動。
/// `mock=false` でも Phase D 時点では sidecar.py 内で 501 が返るが、起動経路の検証は可能。
pub async fn start_sidecar(
    asset_root: &Path,
    sidecar_py: &Path,
    mock: bool,
) -> Result<SidecarHandle> {
    let python = irodori_download::python_exe()?;
    if !python.is_file() {
        return Err(anyhow!(
            "Python ランタイムが未配置です ({}). 設定パネルから Irodori 資産 DL を実行してください",
            python.display()
        ));
    }
    if !sidecar_py.is_file() {
        return Err(anyhow!(
            "sidecar.py が配置されていません: {}",
            sidecar_py.display()
        ));
    }
    let ready_file = ready_path_for(asset_root);
    // 古い ready.json を消してから起動 (port 誤読を防ぐ)
    let _ = std::fs::remove_file(&ready_file);

    let mut cmd = Command::new(&python);
    cmd.arg(sidecar_py)
        .arg("--asset-dir")
        .arg(asset_root)
        .arg("--ready-file")
        .arg(&ready_file)
        .arg("--port")
        .arg("0")
        .arg("--log-level")
        .arg("warning");
    if mock {
        cmd.arg("--mock");
    }
    cmd.kill_on_drop(false); // shutdown_sidecar で明示的に倒す

    let child = cmd
        .spawn()
        .with_context(|| format!("python サイドカー起動失敗: {}", python.display()))?;
    let pid = child.id().unwrap_or(0);

    let port = wait_for_ready_file(&ready_file, Duration::from_secs(30))
        .await
        .with_context(|| {
            format!(
                "サイドカーの起動待ちでタイムアウトしました (ready.json: {})",
                ready_file.display()
            )
        })?;

    Ok(SidecarHandle { port, pid, child })
}

/// `POST /shutdown` を打って 1 秒待ち、ダメなら `child.kill()` する。
pub async fn shutdown_sidecar(mut handle: SidecarHandle, http: &reqwest::Client) -> Result<()> {
    let url = format!("http://127.0.0.1:{}/shutdown", handle.port);
    // shutdown 要求はベストエフォート: 失敗しても kill にフォールバック
    let _ = http
        .post(&url)
        .timeout(Duration::from_secs(2))
        .send()
        .await;

    // 1 秒待って終了していなければ kill
    match tokio::time::timeout(Duration::from_secs(1), handle.child.wait()).await {
        Ok(Ok(_status)) => Ok(()),
        Ok(Err(err)) => Err(anyhow!("サイドカーの wait に失敗: {err}")),
        Err(_elapsed) => {
            // タイムアウト → kill
            if let Err(err) = handle.child.kill().await {
                return Err(anyhow!("サイドカーの kill に失敗: {err}"));
            }
            let _ = handle.child.wait().await;
            Ok(())
        }
    }
}

// === ready.json polling ===

async fn wait_for_ready_file(path: &Path, deadline: Duration) -> Result<u16> {
    let start = Instant::now();
    loop {
        if let Some(port) = try_read_port(path)? {
            return Ok(port);
        }
        if start.elapsed() > deadline {
            return Err(anyhow!("timeout"));
        }
        sleep(Duration::from_millis(200)).await;
    }
}

/// ready.json を 1 回だけ試し読みする。書き込み途中で JSON が壊れていれば None を返してリトライさせる。
fn try_read_port(path: &Path) -> Result<Option<u16>> {
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(None);
    };
    let ready: ReadyFile = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(_) => return Ok(None), // 書き込み中の可能性 → 次のティックで再試行
    };
    Ok(Some(ready.port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_file_parse_extracts_port_and_pid() {
        let json = br#"{"port":54321,"pid":4242}"#;
        let v: ReadyFile = serde_json::from_slice(json).unwrap();
        assert_eq!(v.port, 54321);
        assert_eq!(v.pid, 4242);
    }

    #[test]
    fn try_read_port_returns_none_for_missing_file() {
        let p = std::env::temp_dir().join("ugg-test-nonexistent-ready.json");
        let _ = std::fs::remove_file(&p);
        assert!(try_read_port(&p).unwrap().is_none());
    }

    #[test]
    fn try_read_port_returns_none_for_partial_write() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"{\"port\": ").unwrap();
        assert!(try_read_port(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn try_read_port_returns_port_for_valid_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), br#"{"port":12345,"pid":99}"#).unwrap();
        assert_eq!(try_read_port(tmp.path()).unwrap(), Some(12345));
    }
}

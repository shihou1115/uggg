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
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::sleep;

use crate::tts::irodori_download;

/// 起動済みサイドカーの参照。drop しても子プロセスは生き続けるので、明示的に
/// [`shutdown_sidecar`] を呼ぶこと (Phase E の `quit_app` フックで一括処理)。
#[derive(Debug)]
pub struct SidecarHandle {
    /// 台帳（`sidecars.json`）の位置を知るために持つ。止めたときに記録を消す。
    asset_root: PathBuf,
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

/// 起動したサイドカーの台帳 (v0.5.5 項目 2、spec §6.0)。
///
/// **`ready.json` では足りない。** あれは起動のたびに上書き削除される**単一スロット**で、
/// 孤児が 2 つ以上できると古い方の記録が消えて**到達不能**になる（実機で 2 つ同時に
/// 走った実績がある）。こちらは終了を見届けるまで消さない追記式。
const LEDGER_FILE: &str = "sidecars.json";

fn ledger_path(asset_root: &Path) -> PathBuf {
    asset_root.join(LEDGER_FILE)
}

fn read_ledger(asset_root: &Path) -> Vec<ReadyFile> {
    std::fs::read_to_string(ledger_path(asset_root))
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<ReadyFile>>(&t).ok())
        .unwrap_or_default()
}

fn write_ledger(asset_root: &Path, entries: &[ReadyFile]) {
    if let Ok(json) = serde_json::to_string(entries) {
        let _ = std::fs::write(ledger_path(asset_root), json);
    }
}

/// 台帳へ 1 件足す（起動直後に呼ぶ）。
fn ledger_add(asset_root: &Path, entry: ReadyFile) {
    let mut entries = read_ledger(asset_root);
    entries.retain(|e| e.port != entry.port);
    entries.push(entry);
    write_ledger(asset_root, &entries);
}

/// 台帳から 1 件消す（正常に止められたときに呼ぶ）。
fn ledger_remove(asset_root: &Path, port: u16) {
    let mut entries = read_ledger(asset_root);
    entries.retain(|e| e.port != port);
    write_ledger(asset_root, &entries);
}

/// `/health` の応答が**自分たちのサイドカーのもの**かを判定する (v0.5.5 項目 2)。
///
/// **成否では判定できない。** 実モデルモードで GPU が無いと `/health` は **503** を返す
/// （`sidecar.py`）。一方、無関係なサービスがたまたまそのポートを持っている可能性はある。
/// **pid も撃たず、ポートだけでも撃たない** — 死んだ記録のポートを今持っている別サービスへ
/// `/shutdown` を投げてしまう。**応答の形で自分のものだと確かめてから**止める。
pub(crate) fn looks_like_our_sidecar(body: &serde_json::Value) -> bool {
    let Some(obj) = body.as_object() else {
        return false;
    };
    obj.get("status").and_then(|v| v.as_str()).is_some()
        && obj.get("mock").and_then(|v| v.as_bool()).is_some()
        && obj.contains_key("gpu")
}

/// 前回の実行が残したサイドカーを止める (v0.5.5 項目 2)。
///
/// **サイドカーを 1 つも起動する前に呼ぶこと。** 後から呼ぶと、掃除対象のポートを
/// 新しいサイドカーが取っている可能性があり、自分で立てたものを止めてしまう。
///
/// 台帳に加えて**旧 `ready.json` も 1 度だけ見る** — v0.5.5 より前に導入した環境には
/// 台帳が無く、孤児の手がかりがそこにしか無いため。
pub async fn sweep_orphans(asset_root: &Path, client: &reqwest::Client) -> usize {
    let mut candidates = read_ledger(asset_root);
    if let Ok(Some(port)) = try_read_port(&ready_path_for(asset_root)) {
        if !candidates.iter().any(|e| e.port == port) {
            candidates.push(ReadyFile { port, pid: 0 });
        }
    }
    // **先に台帳を空にする。** そのうえで、掃除の最中に新しく立ったサイドカーの
    // ポートは対象から外す（掃除対象のポートを新しい子が取っていたら、自分で立てたものを
    // 止めてしまう）。台帳は起動直後に書かれるので、ここを見れば「いま生きている自分の子」が分かる。
    write_ledger(asset_root, &[]);

    let mut stopped = 0usize;
    for entry in &candidates {
        if read_ledger(asset_root).iter().any(|e| e.port == entry.port) {
            continue;
        }
        match identify_sidecar(client, entry.port).await {
            true => {
                let _ = request_shutdown(entry.port, client).await;
                crate::ulog!(
                    "[irodori] 前回の実行が残したサイドカーを止めました (port={} pid={})",
                    entry.port,
                    entry.pid
                );
                stopped += 1;
            }
            false => {
                // 応答が無い / 形が違う = すでに死んでいるか、別のサービスのポート。触らない。
                crate::ulog!(
                    "[irodori] 記録のサイドカーは見つかりません (port={}、記録だけ捨てます)",
                    entry.port
                );
            }
        }
    }
    stopped
}

/// そのポートの相手が自分たちのサイドカーか確かめる。
async fn identify_sidecar(client: &reqwest::Client, port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/health");
    let Ok(resp) = client
        .get(&url)
        .timeout(Duration::from_millis(800))
        .send()
        .await
    else {
        return false;
    };
    // **status は見ない**（GPU 不在で 503 を返す）。本文の形だけで判断する。
    match resp.json::<serde_json::Value>().await {
        Ok(body) => looks_like_our_sidecar(&body),
        Err(_) => false,
    }
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
///
/// `on_stderr_line` は子プロセス stderr の各行を受ける callback。HF モデル DL の進捗
/// (`[hf-download] ...` 行) を `irodori-download` イベントへ転送するために呼び出し側で
/// AppHandle を closure に閉じ込めて渡す (M4c Phase G)。テスト/内部利用は no-op で渡してよい。
pub async fn start_sidecar<F>(
    asset_root: &Path,
    sidecar_py: &Path,
    mock: bool,
    on_stderr_line: F,
) -> Result<SidecarHandle>
where
    F: FnMut(&str) + Send + 'static,
{
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
    // サイドカーが生きている間ずっとコンソール窓が残らないようにする。
    cmd.creation_flags(crate::tts::irodori_download::CREATE_NO_WINDOW);
    cmd.arg(sidecar_py)
        .arg("--asset-dir")
        .arg(asset_root)
        .arg("--ready-file")
        .arg(&ready_file)
        .arg("--port")
        .arg("0")
        .arg("--log-level")
        .arg("warning")
        // **モデルの正本は Rust 側** (v0.5.5 項目 3)。`sidecar.py` は毎起動で上書き
        // コピーされるので、あちらのハードコードを正本にすると「コードだけ新しくなって
        // 重みが無い」状態を作る。取得側（`--download-only`）と同じ値をここでも渡す。
        .args(crate::tts::irodori_download::model_args())
        // HF モデル DL は起動 hot path から外し、download_irodori_assets ステップ 6
        // (irodori_download::install_irodori_models) で先に取得する。ここでは常に --no-download。
        // モデル不在のまま実モード起動した場合は RealModelBackend.synth が FileNotFoundError を
        // 投げて 500 を返し、Rust 側 fallback で voicevox に流れる。
        .arg("--no-download")
        // stderr を piped にして HF DL 進捗を行単位で吸い上げる
        .stderr(Stdio::piped());
    if mock {
        cmd.arg("--mock");
    }
    // 起動中にコケた / ready_file の deadline を超えた場合、明示 kill しないと python が
    // バックグラウンドで HF モデル DL を続けるゾンビ化する。下の wait_for_ready_file が Err
    // を返す経路で `child.start_kill()` を呼ぶ。
    cmd.kill_on_drop(false); // shutdown_sidecar / Err 経路で明示的に倒す

    let mut child = cmd
        .spawn()
        .with_context(|| format!("python サイドカー起動失敗: {}", python.display()))?;
    let pid = child.id().unwrap_or(0);

    // stderr を別タスクで非同期 read。サイドカー終了で EOF → タスクも終わる。
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(spawn_stderr_pump(stderr, on_stderr_line));
    }

    let port = match wait_for_ready_file(&ready_file, Duration::from_secs(30), pid).await {
        Ok(p) => p,
        Err(err) => {
            // ready.json が来ない = 起動失敗 or 長時間 HF DL 中。
            // 明示 kill して python orphan を防ぐ (kill_on_drop=false のため)。
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(err).with_context(|| {
                format!(
                    "サイドカーの起動待ちでタイムアウトしました (ready.json: {})",
                    ready_file.display()
                )
            });
        }
    };

    ledger_add(asset_root, ReadyFile { port, pid });
    Ok(SidecarHandle {
        asset_root: asset_root.to_path_buf(),
        port,
        pid,
        child,
    })
}

/// 子プロセス stderr を行単位で読み、each line を callback に流す。
/// stderr EOF (サイドカー終了) で自然終了。
async fn spawn_stderr_pump<R, F>(stderr: R, mut on_line: F)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    F: FnMut(&str) + Send + 'static,
{
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        on_line(&line);
    }
}

/// `POST /shutdown` を打って 1 秒待ち、ダメなら `child.kill()` する。
/// ポートだけで止める（孤児にはハンドルが無いので kill にフォールバックできない）。
///
/// **`looks_like_our_sidecar` で自分のものだと確かめてから呼ぶこと。**
async fn request_shutdown(port: u16, http: &reqwest::Client) -> bool {
    let url = format!("http://127.0.0.1:{port}/shutdown");
    http.post(&url)
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .is_ok()
}

pub async fn shutdown_sidecar(mut handle: SidecarHandle, http: &reqwest::Client) -> Result<()> {
    // 正常に止めるので台帳から消す（残すと次回の掃除が無駄に叩く）。
    ledger_remove(&handle.asset_root, handle.port);
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

/// `ready.json` を読み、**起動した子の pid と一致するときだけ**そのポートを返す。
///
/// **pid を見ないと、古い `ready.json` を信じて死んだポートへ投げる**（2026-09-13 実機で発覚）。
/// 起動前に `remove_file` しているが、その削除が効かない状況（ファイルロック・ウイルス対策・
/// 仮想化されたプロファイル）では、新しい子が書くより先に**前回の子の記録**を読んでしまう。
/// 実機では前回セッションで止めた孤児のポート 60005 へ合成を投げ、起動時の挨拶が失敗した。
/// `sidecar.py` は `os.getpid()` を書くので、一致しないものは古い記録と判断できる。
async fn wait_for_ready_file(path: &Path, deadline: Duration, expected_pid: u32) -> Result<u16> {
    let start = Instant::now();
    loop {
        if let Some(ready) = try_read_ready(path)? {
            if ready_belongs_to(&ready, expected_pid) {
                return Ok(ready.port);
            }
            // 前回の子の記録。新しい子が上書きするまで待つ。
        }
        if start.elapsed() > deadline {
            return Err(anyhow!("timeout"));
        }
        sleep(Duration::from_millis(200)).await;
    }
}

/// ready.json を 1 回だけ試し読みする。書き込み途中で JSON が壊れていれば None を返してリトライさせる。
/// `ready.json` をそのまま読む（port と pid の両方）。
fn try_read_ready(path: &Path) -> Result<Option<ReadyFile>> {
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(None);
    };
    match serde_json::from_slice::<ReadyFile>(&bytes) {
        Ok(r) => Ok(Some(r)),
        Err(_) => Ok(None), // 書き込み中の可能性 → 次のティックで再試行
    }
}

/// その記録が、いま起動した子のものか。
///
/// pid を取れなかった（子がすでに終了している）場合は 0 が来る。**0 とは一致させない** —
/// 記録側の pid が偶然 0 でも受け入れず、待ちをタイムアウトさせて起動失敗として扱う。
fn ready_belongs_to(ready: &ReadyFile, expected_pid: u32) -> bool {
    expected_pid != 0 && ready.pid == expected_pid
}

/// ポートだけを読む。**孤児掃除が古い記録を拾うため**に残す（こちらは pid で絞らない）。
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
    /// **成否では自分のものだと判定できない** (v0.5.5 項目 2)。
    ///
    /// 実モデルモードで GPU が無いと `/health` は **503** を返す。`is_success` で弾くと
    /// 自分のサイドカーを「別物」と誤認して掃除できない。逆に成否だけで通すと、
    /// たまたまそのポートを持っている無関係なサービスへ `/shutdown` を投げてしまう。
    /// **応答の形**で判断する。
    #[test]
    fn a_sidecar_is_identified_by_the_shape_of_its_answer() {
        let ours_ok = serde_json::json!({"status": "ok", "gpu": "RTX 5080", "mock": false});
        let ours_no_gpu = serde_json::json!({"status": "no_gpu", "gpu": null, "mock": false});
        assert!(super::looks_like_our_sidecar(&ours_ok));
        assert!(
            super::looks_like_our_sidecar(&ours_no_gpu),
            "503 で返る形も自分のもの（GPU 不在でこれを返す）"
        );

        // 無関係なサービス
        for other in [
            serde_json::json!({"status": "ok"}),
            serde_json::json!({"status": "ok", "gpu": null}),
            serde_json::json!({"ok": true, "mock": false}),
            serde_json::json!("ok"),
            serde_json::json!([1, 2, 3]),
        ] {
            assert!(
                !super::looks_like_our_sidecar(&other),
                "他人のポートへ shutdown を投げてはいけない: {other}"
            );
        }
    }

    /// 台帳は**複数**持てること (v0.5.5 項目 2)。
    ///
    /// `ready.json` は起動のたびに上書き削除される単一スロットなので、孤児が 2 つ以上
    /// できると古い方が到達不能になる（実機で 2 つ同時に走った実績がある）。
    #[test]
    fn the_ledger_keeps_every_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        super::ledger_add(dir.path(), super::ReadyFile { port: 50073, pid: 1 });
        super::ledger_add(dir.path(), super::ReadyFile { port: 59533, pid: 2 });
        let got = super::read_ledger(dir.path());
        assert_eq!(got.len(), 2, "2 つ目で 1 つ目を消してはいけない: {got:?}");

        // 正常に止めた分だけ消える
        super::ledger_remove(dir.path(), 50073);
        let got = super::read_ledger(dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].port, 59533);
    }

    /// 同じポートを 2 度足しても重複しない（再起動で同じポートを引くことはある）。
    #[test]
    fn the_ledger_does_not_duplicate_a_port() {
        let dir = tempfile::tempdir().unwrap();
        super::ledger_add(dir.path(), super::ReadyFile { port: 50073, pid: 1 });
        super::ledger_add(dir.path(), super::ReadyFile { port: 50073, pid: 9 });
        let got = super::read_ledger(dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].pid, 9, "新しい方で置き換わること");
    }

    use super::*;

    /// **古い `ready.json` を信じない**（2026-09-13 実機で発覚）。
    ///
    /// 数値は実機のもの。前回セッションで止めた孤児（port 60005 / pid 43140）の記録が残り、
    /// 新しい子（port 62776 / pid 15316）が書く前にそれを読んで、死んだポートへ合成を投げた。
    #[test]
    fn only_the_spawned_childs_ready_file_is_trusted() {
        let stale = ReadyFile { port: 60005, pid: 43140 };
        let fresh = ReadyFile { port: 62776, pid: 15316 };
        assert!(!ready_belongs_to(&stale, 15316), "前回の子の記録を信じてはいけない");
        assert!(ready_belongs_to(&fresh, 15316), "いま起動した子の記録は受け入れる");
        assert!(
            !ready_belongs_to(&ReadyFile { port: 1, pid: 0 }, 0),
            "pid を取れなかった（子が終了済み）なら、偶然 0 の記録にも一致させない"
        );
    }

    /// **待ちの配線まで固定する**（関数だけ正しくて呼ばれていない、を素通りさせない）。
    ///
    /// `ready_belongs_to` 単体のテストでは、`wait_for_ready_file` がそれを使っているかまでは
    /// 分からない（`backfill_baseline` で同じ取りこぼしを実際にやった）。古い記録しか無ければ
    /// **ポートを返さずに待ちがタイムアウトする**こと、一致すれば返すことを見る。
    #[tokio::test]
    async fn waiting_ignores_a_stale_ready_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), br#"{"port":60005,"pid":43140}"#).unwrap();

        let got = wait_for_ready_file(tmp.path(), Duration::from_millis(500), 15316).await;
        assert!(got.is_err(), "古い記録のポートを返してはいけない: {got:?}");

        std::fs::write(tmp.path(), br#"{"port":62776,"pid":15316}"#).unwrap();
        let got = wait_for_ready_file(tmp.path(), Duration::from_millis(500), 15316).await;
        assert_eq!(got.unwrap(), 62776, "いま起動した子の記録なら返す");
    }

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

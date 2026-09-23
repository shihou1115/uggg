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
    /// 台帳の記録を**ポートと pid の組**で消すために持つ（同じポートを後から別の子が取りうる）。
    pub pid: u32,
    /// `wait()` を呼ばずに保持し続けるとゾンビ化するため、`shutdown_sidecar` で wait する。
    pub child: Child,
}

impl SidecarHandle {
    /// このサイドカーの資産ルート（採用の時点で、導入・更新の錠を見るのに使う）。
    pub(crate) fn asset_root(&self) -> &Path {
        &self.asset_root
    }
}

#[cfg(test)]
impl SidecarHandle {
    /// テスト用。本物の起動を経ずに、採用の判定（`IrodoriClient::adopt_sidecar`）を確かめる。
    pub(crate) fn for_test(asset_root: &Path, port: u16, pid: u32, child: Child) -> Self {
        Self {
            asset_root: asset_root.to_path_buf(),
            port,
            pid,
            child,
        }
    }
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

/// 台帳の「読む → 書き戻す」を直列にする（2026-09-13）。
///
/// **排他が無いと、後から書いた側が先の変更を消す。** 孤児掃除は起動直後に非同期で走り、
/// 同じ時期に起動時の挨拶がサイドカーを立てて `ledger_add` しうる。両者が同じ内容を読んで
/// それぞれ書き戻すと、**新しく立てた子の記録が消え、次に強制終了されたとき孤児を追えない**。
static LEDGER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_ledger() -> std::sync::MutexGuard<'static, ()> {
    // 中身を持たない錠なので、毒されていても続行してよい。
    LEDGER_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 台帳へ 1 件足す（起動直後に呼ぶ）。
fn ledger_add(asset_root: &Path, entry: ReadyFile) {
    let _guard = lock_ledger();
    let mut entries = read_ledger(asset_root);
    entries.retain(|e| e.port != entry.port);
    entries.push(entry);
    write_ledger(asset_root, &entries);
}

/// 台帳からその記録を 1 件消す（止まったのを見届けたとき・掃除で確かめ終えたときに呼ぶ）。
///
/// **ポートだけでなく pid も一致したものだけを消す。** 同じポートを後から新しい子が取ると、
/// `ledger_add` はその記録を新しい子の pid で置き換える。ポートだけで消すと、
/// **いま生きている自分の子の記録**を消してしまう。
fn ledger_remove(asset_root: &Path, record: &ReadyFile) {
    let _guard = lock_ledger();
    let mut entries = read_ledger(asset_root);
    entries.retain(|e| !(e.port == record.port && e.pid == record.pid));
    write_ledger(asset_root, &entries);
}

/// 台帳のそのポートを、候補とは**別の子**（pid が違う）がいま持っているか。
///
/// 掃除の最中に同じポートで新しい子が立ったなら、それは自分で立てたもの。触らない。
fn taken_by_a_new_child(ledger: &[ReadyFile], candidate: &ReadyFile) -> bool {
    ledger
        .iter()
        .any(|e| e.port == candidate.port && e.pid != candidate.pid)
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
    sweep_orphans_with(asset_root, client, PROBE_TIMEOUT).await
}

/// 掃除で、つながった相手の HTTP 応答を待つ時間。
///
/// 合成中・モデル読み込み中のサイドカーは `/health` に答えない。待ちきれなかった記録は
/// 「応答しない」として残す（次の起動でまた確かめる）。
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// 掃除で、TCP の接続だけを確かめる時間。
///
/// **Windows は閉じたポートへの接続が拒否されるまで約 2 秒かかる**（SYN を再送する。
/// 2026-09-14 実測 2.02〜2.04 秒）。これより短いと、死んだ記録の拒否を待ちきれず
/// 「応答しない」と取り違えて永久に残す。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 前回までの実行が残したかもしれないサイドカーの記録（台帳 + 旧 `ready.json`）。
/// 掃除（`sweep_orphans`）と、更新の前の確認（`live_sidecars`）で同じ集め方をする。
fn orphan_candidates(asset_root: &Path) -> Vec<ReadyFile> {
    let mut candidates = read_ledger(asset_root);
    if let Ok(Some(port)) = try_read_port(&ready_path_for(asset_root)) {
        if !candidates.iter().any(|e| e.port == port) {
            candidates.push(ReadyFile { port, pid: 0 });
        }
    }
    candidates
}

/// 記録にあるサイドカーのうち、**いま生きているもの**のポート（v0.5.6 項目 3e）。**読むだけで止めない。**
///
/// 更新の前に使う。生きているサイドカーは torch の DLL（`site-packages\torch\lib\c10.dll` など）を
/// 読み込んだままで、Windows はそれを消すことも上書きすることも拒む。そのまま入れ替えを始めると、
/// **入れ替えも、失敗したときの全戻しも**同じファイルで失敗する（反証レビュー #4）。
/// 「生きている」は、応答の形が自分たちのもの（`Ours`）か、つながったのに答えない（`Unanswered`。
/// 合成中・読み込み中）もの。
///
/// **止めないのは、所有者を見分けられないから**（項目 4 で台帳に所有者の欄が入るまで）。応答の形だけで
/// 止めると、もう 1 つの ugg が使っている最中のサイドカーを止めうる。
pub async fn live_sidecars(asset_root: &Path, client: &reqwest::Client) -> Vec<u16> {
    let mut live = Vec::new();
    for entry in orphan_candidates(asset_root) {
        match identify_sidecar(client, entry.port, PROBE_TIMEOUT).await {
            Probe::Ours | Probe::Unanswered => live.push(entry.port),
            Probe::NotOurs => {}
        }
    }
    live
}

async fn sweep_orphans_with(
    asset_root: &Path,
    client: &reqwest::Client,
    probe_timeout: Duration,
) -> usize {
    let candidates = orphan_candidates(asset_root);
    if !candidates.is_empty() {
        // 開始と 1 件ごとの所要時間をログに残す（2026-09-14、実環境で起動から判定まで 22 秒
        // かかっていた原因を、ログから切り分けられなかったため）。
        crate::ulog!(
            "[irodori] 前回の実行の記録を確かめます ({} 件)",
            candidates.len()
        );
    }
    // **記録は、確かめ終えたものから 1 件ずつ消す**（2026-09-13 実機で発覚）。
    // 以前は先に台帳を空にしてから確かめていた。実機では起動直後に dev が落ち、
    // **記録を 1 件も確かめないまま 2 件とも消えた**。本物の孤児でも同じで、掃除の途中で
    // アプリが落ちると（起動直後に落ちる不具合と重なれば毎回）**GPU を掴んだ孤児の手がかりが
    // 永久に失われる**。まだ確かめていない記録は、次の起動のために残す。
    let mut stopped = 0usize;
    for entry in &candidates {
        // 掃除の最中に同じポートで新しい子が立ったなら自分で立てたもの。止めない。
        // 台帳は起動直後に書かれるので、ここを見れば「いま生きている自分の子」が分かる。
        if taken_by_a_new_child(&read_ledger(asset_root), entry) {
            continue;
        }
        let started = Instant::now();
        let probe = identify_sidecar(client, entry.port, probe_timeout).await;
        let took = started.elapsed().as_millis();
        match probe {
            Probe::Ours => {
                if !request_shutdown(entry.port, client).await {
                    // 自分のものなのに止める要求が届かなかった。記録を残して次の起動で再試行する。
                    crate::ulog!(
                        "[irodori] 前回の実行が残したサイドカーを止められませんでした (port={} pid={}、{}ms、次の起動で再試行します)",
                        entry.port,
                        entry.pid,
                        took
                    );
                    continue;
                }
                crate::ulog!(
                    "[irodori] 前回の実行が残したサイドカーを止めました (port={} pid={}、{}ms)",
                    entry.port,
                    entry.pid,
                    took
                );
                stopped += 1;
            }
            Probe::Unanswered => {
                // つながったのに応答が無い = 合成中・モデル読み込み中の自分の孤児でありうる
                // （`/speech` は同期の合成を async の中で呼ぶので、その間 `/health` に答えない）。
                // **捨てると、合成中に強制終了された孤児に二度と届かない**（2026-09-14 監査で発覚）。
                crate::ulog!(
                    "[irodori] 記録のサイドカーが応答しません (port={}、{}ms、使用中の可能性があるので記録を残します)",
                    entry.port,
                    took
                );
                continue;
            }
            Probe::NotOurs => {
                // 接続を拒否された = 死んでいる / 答えたが形が違う = 別のサービス。触らない。
                crate::ulog!(
                    "[irodori] 記録のサイドカーは見つかりません (port={}、{}ms、記録だけ捨てます)",
                    entry.port,
                    took
                );
            }
        }
        ledger_remove(asset_root, entry);
    }
    stopped
}

/// 掃除の相手の見立て。
#[derive(Debug, PartialEq, Eq)]
enum Probe {
    /// 応答の形が自分たちのサイドカー。
    Ours,
    /// 接続を拒否された（死んでいる）か、答えたが形が違う（別のサービス）。
    NotOurs,
    /// つながったが時間内に答えない。使用中の自分の孤児でありうる。
    Unanswered,
}

/// そのポートの相手が何者か確かめる。
///
/// **まず TCP の接続だけを、非同期ランタイムの外（待機スレッド）で確かめる**（2026-09-14 実環境で発覚）。
/// HTTP のタイムアウトだけで判定すると、起動直後の混雑で「接続拒否の知らせ」と「タイマー」が
/// 同時に処理待ちになったとき、reqwest は**タイムアウトを先に見る**（`PendingRequest::poll`）ため、
/// 拒否された死んだ記録を「応答しない」と取り違えて残し続けた。v0.5.5 インストール版は、何も
/// 待ち受けていないポートを起動のたびに「応答しません」と判定していた。待機スレッドでの接続の
/// 結果はタイマーと先着を争わない。
async fn identify_sidecar(client: &reqwest::Client, port: u16, timeout: Duration) -> Probe {
    match tcp_reach(port, CONNECT_TIMEOUT).await {
        Reach::Refused => return Probe::NotOurs,
        Reach::NoAnswer => return Probe::Unanswered,
        Reach::Connected => {}
    }
    let url = format!("http://127.0.0.1:{port}/health");
    let resp = match client.get(&url).timeout(timeout).send().await {
        Ok(resp) => resp,
        Err(err) if err.is_timeout() => return Probe::Unanswered,
        Err(_) => return Probe::NotOurs,
    };
    // **status は見ない**（GPU 不在で 503 を返す）。本文の形だけで判断する。
    match resp.json::<serde_json::Value>().await {
        Ok(body) if looks_like_our_sidecar(&body) => Probe::Ours,
        Ok(_) => Probe::NotOurs,
        Err(err) if err.is_timeout() => Probe::Unanswered,
        Err(_) => Probe::NotOurs,
    }
}

/// TCP で見たそのポートの様子。
#[derive(Debug, PartialEq, Eq)]
enum Reach {
    /// 接続できた（待ち受けている相手がいる）。
    Connected,
    /// 接続を拒否された、またはつなげなかった（誰もいない）。
    Refused,
    /// 時間内に結果が出なかった（決めつけない）。
    NoAnswer,
}

/// 待機スレッドで TCP の接続だけを確かめる。
async fn tcp_reach(port: u16, timeout: Duration) -> Reach {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let joined =
        tokio::task::spawn_blocking(move || std::net::TcpStream::connect_timeout(&addr, timeout))
            .await;
    match joined {
        Ok(Ok(_stream)) => Reach::Connected,
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::TimedOut => Reach::NoAnswer,
        Ok(Err(_)) => Reach::Refused,
        Err(_) => Reach::NoAnswer,
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
    // 読む先は導入記録から決める（v0.5.6 項目 3a）。**どこから決めたかを残す** — 実機で
    // 「記録どおりの重みを読んでいるか」を追える唯一の観測点で、実データの確認もこの行で見る。
    let (model_args, from) = crate::tts::irodori_download::model_args_for_read(asset_root);
    crate::ulog!("[irodori] モデルの読み先を{}から決めました: {}", from, model_args.join(" "));

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
        // 重みが無い」状態を作る。**読む先は導入記録から決める**（v0.5.6 項目 3a）— 取得側
        // （`--download-only`）はいまのビルドの値を使い、**更新が成功したときだけ記録が追いつく**。
        .args(&model_args)
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
///
/// **読めない行でも止まらず、止まるなら理由を残す**（2026-09-14、インストール版の実環境で発覚）。
/// `lines()` は UTF-8 として読めない行や読み取りの失敗で `Err` を返し、`while let Ok(Some(..))`
/// はそこで**黙って抜けていた**。実機では起動の約 10 秒後に出る asyncio のトレースバックの途中で
/// 読み取りが止まり、以後の stderr が 1 行も `ugg.log` に残らなかった（同じサイドカーへ送った
/// 不正なリクエストに uvicorn は 400 を返したが、その警告が届かないことで確認）。
/// 読めない部分は置換文字にして流し、読み取りそのものが失敗したら理由を 1 行流してから終える。
async fn spawn_stderr_pump<R, F>(stderr: R, mut on_line: F)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    F: FnMut(&str) + Send + 'static,
{
    let mut reader = BufReader::new(stderr);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) => break,
            Ok(_) => {
                // `lines()` と同じく、末尾の `\n` とその直前の `\r` を 1 つだけ落とす。
                let mut end = buf.len();
                if buf[end - 1] == b'\n' {
                    end -= 1;
                    if end > 0 && buf[end - 1] == b'\r' {
                        end -= 1;
                    }
                }
                // UTF-8 で読めなければ Shift_JIS（cp932）で読む（v0.5.6 項目 2）。
                on_line(&crate::tts::reader::decode_output_line(&buf[..end]));
            }
            Err(e) => {
                on_line(&format!("(stderr の読み取りが止まりました: {e})"));
                break;
            }
        }
    }
}

/// 止まったと確かめるまで待つ時間（v0.5.6 項目 3e）。Windows は閉じたポートへの接続が拒否されるまで
/// 約 2 秒かかる（`CONNECT_TIMEOUT` の注記）ので、2 回は確かめられる長さにする。
const STOP_CONFIRM: Duration = Duration::from_secs(8);

/// 孤児に `POST /shutdown` を送り、**本当に止まったか**まで確かめる（v0.5.6 項目 3e）。
///
/// 以前は**送れただけ**で成功とし（4xx / 5xx も成功扱い）、呼び出し側がすぐ台帳から消していたので、
/// **答えたが止まらなかった孤児は台帳から消えて二度と追えなかった**。サイドカーの `/shutdown` は
/// 200 を返してから 0.1 秒後に終わる（「答えた」は「終わった」ではない）。応答が 2xx で、そのあと
/// **TCP が接続を拒否するようになったら**止まったとみなす。確かめられなければ偽（記録を残す）。
///
/// ポートだけで止める（孤児にはハンドルが無いので kill にはできない。自分のサイドカーは
/// `shutdown_sidecar` がハンドルで終わりを確かめる）。
/// **`looks_like_our_sidecar` で自分のものだと確かめてから呼ぶこと。**
async fn request_shutdown(port: u16, http: &reqwest::Client) -> bool {
    let url = format!("http://127.0.0.1:{port}/shutdown");
    match http.post(&url).timeout(Duration::from_secs(2)).send().await {
        Ok(resp) if resp.status().is_success() => {}
        _ => return false,
    }
    let deadline = Instant::now() + STOP_CONFIRM;
    loop {
        if tcp_reach(port, Duration::from_secs(3)).await == Reach::Refused {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub async fn shutdown_sidecar(mut handle: SidecarHandle, http: &reqwest::Client) -> Result<()> {
    let record = ReadyFile {
        port: handle.port,
        pid: handle.pid,
    };
    let url = format!("http://127.0.0.1:{}/shutdown", handle.port);
    // shutdown 要求はベストエフォート: 失敗しても kill にフォールバック
    let _ = http
        .post(&url)
        .timeout(Duration::from_secs(2))
        .send()
        .await;

    // 1 秒待って終了していなければ kill
    let stopped = match tokio::time::timeout(Duration::from_secs(1), handle.child.wait()).await {
        Ok(Ok(_status)) => Ok(()),
        Ok(Err(err)) => Err(anyhow!("サイドカーの wait に失敗: {err}")),
        Err(_elapsed) => {
            // タイムアウト → kill
            match handle.child.kill().await {
                Ok(()) => {
                    let _ = handle.child.wait().await;
                    Ok(())
                }
                Err(err) => Err(anyhow!("サイドカーの kill に失敗: {err}")),
            }
        }
    };
    // **止まったのを見届けてから記録を消す**（2026-09-13、孤児掃除と同じ形）。
    // 以前は冒頭で消していた。止めている数秒の間にアプリが落ちたときや kill に失敗したとき、
    // **生きているサイドカーの記録だけが消え**、次の起動の掃除が届かない。
    if stopped.is_ok() {
        ledger_remove(&handle.asset_root, &record);
    }
    stopped
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
        super::ledger_remove(dir.path(), &super::ReadyFile { port: 50073, pid: 1 });
        let got = super::read_ledger(dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].port, 59533);

        // ポートが同じでも pid が違えば**別の子**（後から同じポートを取った新しい子）。消さない
        super::ledger_remove(dir.path(), &super::ReadyFile { port: 59533, pid: 7 });
        assert_eq!(
            super::read_ledger(dir.path()).len(),
            1,
            "同じポートを持つ別の子の記録を消してはいけない"
        );
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

    /// 台帳の「読む → 書き戻す」は直列（2026-09-13）。並行に足しても**1 件も取りこぼさない**。
    ///
    /// 孤児掃除は起動直後に非同期で走り、同じ時期に起動時の挨拶がサイドカーを立てうる。
    /// 排他が無いと、後から書いた側が先の記録を消す。
    #[test]
    fn concurrent_ledger_writes_lose_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let threads: Vec<_> = (0..4u16)
            .map(|t| {
                let root = root.clone();
                std::thread::spawn(move || {
                    for i in 0..50u16 {
                        super::ledger_add(
                            &root,
                            super::ReadyFile {
                                port: 10_000 + t * 100 + i,
                                pid: 1,
                            },
                        );
                    }
                })
            })
            .collect();
        for th in threads {
            th.join().unwrap();
        }
        assert_eq!(
            super::read_ledger(&root).len(),
            200,
            "並行に足した記録を取りこぼしてはいけない"
        );
    }

    use super::*;

    /// テスト用の HTTP クライアント（環境の proxy 設定に左右されないように）。
    fn test_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    /// いま誰も待ち受けていないポート（死んだ記録の役）。
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// `/health` に**自分たちの形**で答える偽のサイドカー。`/shutdown` に答えるかを選べる。
    /// 受けたリクエスト行を記録する。
    /// 偽サイドカーが `/shutdown` にどう応じるか。
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum OnShutdown {
        /// 答えて、待ち受けをやめる（本物と同じ）。
        Exits,
        /// 答えるが、待ち受けを続ける（止まらない）。
        AnswersButStays,
        /// 答えずに切る。
        Ignores,
    }

    fn fake_sidecar(on_shutdown: OnShutdown) -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let line = String::from_utf8_lossy(&buf)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                log.lock().unwrap().push(line.clone());
                let shutdown = line.starts_with("POST /shutdown");
                let body = if line.starts_with("GET /health") {
                    r#"{"status":"ok","gpu":null,"mock":true}"#
                } else if shutdown && on_shutdown != OnShutdown::Ignores {
                    r#"{"status":"ok"}"#
                } else {
                    continue; // 答えずに切る
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                if shutdown && on_shutdown == OnShutdown::Exits {
                    break; // 待ち受けをやめる（listener が落ちてポートが閉じる）
                }
            }
        });
        (port, seen)
    }

    /// **掃除が途中で止まっても、確かめていない記録は残る**（2026-09-13 実機で発覚）。
    ///
    /// 実機では起動直後に dev が落ち、先に台帳を空にする作りだったため
    /// **記録を 1 件も確かめないまま 2 件とも消えた**。本物の孤児なら GPU を掴んだまま
    /// 二度と止められない。応答しない相手を確かめている最中（確かめる時間いっぱい待たされる間）に中断して見る。
    #[tokio::test]
    async fn an_interrupted_sweep_keeps_the_records_it_has_not_checked() {
        let dir = tempfile::tempdir().unwrap();
        // 接続は受けるが応答しない相手（確かめる間ずっと待たされる）
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let silent_port = silent.local_addr().unwrap().port();
        let dead_port = free_port();
        ledger_add(dir.path(), ReadyFile { port: silent_port, pid: 1 });
        ledger_add(dir.path(), ReadyFile { port: dead_port, pid: 2 });

        let client = test_client();
        let interrupted = tokio::time::timeout(
            Duration::from_millis(300),
            sweep_orphans(dir.path(), &client),
        )
        .await;
        assert!(interrupted.is_err(), "前提: 確かめている最中に中断できていること");

        let ports: Vec<u16> = read_ledger(dir.path()).iter().map(|e| e.port).collect();
        assert!(
            ports.contains(&silent_port) && ports.contains(&dead_port),
            "確かめ終えていない記録を消してはいけない: {ports:?}"
        );
        drop(silent);
    }

    /// 最後まで走ると、**死んでいた記録は消え、応答しない記録と、途中で立った自分の子の
    /// 記録は残る**（2026-09-14 監査で「応答しない」を分けた）。
    ///
    /// - 応答しない相手 = 合成中の自分の孤児でありうる（`/health` に答えない）。捨てると二度と届かない
    /// - 接続を拒否された相手 = 死んでいる。記録だけ捨てる
    /// - 掃除の最中に同じポートで新しい子が立つと、`ledger_add` がその記録を新しい子の pid で
    ///   置き換える。**確かめに行ってはいけない**（自分の子に `/shutdown` を投げうる）し、消してもいけない
    #[tokio::test]
    async fn a_finished_sweep_drops_the_dead_but_keeps_the_busy_and_a_new_child() {
        let dir = tempfile::tempdir().unwrap();
        // 接続は受けるが応答しない相手（合成中の孤児の役）
        let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let busy_port = busy.local_addr().unwrap().port();
        let dead_port = free_port();
        // 掃除の最中に新しい子が取るポート。接続が来たかどうかをあとで見る。
        let new_child = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        new_child.set_nonblocking(true).unwrap();
        let child_port = new_child.local_addr().unwrap().port();
        ledger_add(dir.path(), ReadyFile { port: busy_port, pid: 1 });
        ledger_add(dir.path(), ReadyFile { port: dead_port, pid: 2 });
        ledger_add(dir.path(), ReadyFile { port: child_port, pid: 3 });

        let root = dir.path().to_path_buf();
        // 確かめる時間は、閉じたポートの拒否（Windows で約 2 秒）より長くとる
        let sweep = tokio::spawn(async move {
            sweep_orphans_with(&root, &test_client(), Duration::from_secs(3)).await
        });
        // 1 件目（応答しない相手）を確かめている間に、新しい子が同じポートで立つ
        tokio::time::sleep(Duration::from_millis(200)).await;
        ledger_add(dir.path(), ReadyFile { port: child_port, pid: 999 });
        assert_eq!(sweep.await.unwrap(), 0);

        let mut got: Vec<(u16, u32)> = read_ledger(dir.path())
            .iter()
            .map(|e| (e.port, e.pid))
            .collect();
        got.sort();
        let mut want = vec![(busy_port, 1), (child_port, 999)];
        want.sort();
        assert_eq!(
            got, want,
            "死んだ記録だけが消える（応答しない記録と新しい子の記録は残る）"
        );
        assert!(
            matches!(new_child.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "新しい子へ確かめに行ってはいけない（自分の子に /shutdown を投げうる）"
        );
        drop(busy);
    }

    /// **TCP の接続を確かめる時間は、閉じたポートの拒否より長い**（2026-09-14 実測で発覚）。
    ///
    /// Windows は閉じたポートへの接続が拒否されるまで約 2 秒かかる。確かめる時間がそれより
    /// 短いと、死んだ記録を「応答しない」と取り違えて**永久に残す**。
    #[test]
    fn the_connect_check_outlasts_a_refused_connection() {
        let port = free_port();
        let started = Instant::now();
        let refused = std::net::TcpStream::connect(("127.0.0.1", port));
        let took = started.elapsed();
        assert!(refused.is_err(), "前提: 誰も待ち受けていない");
        assert!(
            CONNECT_TIMEOUT > took * 2,
            "拒否に {took:?} かかる環境で、確かめる時間 {CONNECT_TIMEOUT:?} は短すぎる"
        );
    }

    /// **HTTP の待ちが拒否より先に切れても、死んだ記録を「応答しない」と取り違えない**（2026-09-14 実環境で発覚）。
    ///
    /// v0.5.5 インストール版は、何も待ち受けていないポートを起動のたびに「応答しません」と判定し、
    /// 記録を残し続けた。起動直後の混雑で拒否の知らせとタイマーが同時に処理待ちになると、reqwest は
    /// タイムアウトを先に見る。ここでは **HTTP の待ちを拒否（Windows で約 2 秒）より短くして**同じ状況を作る。
    #[tokio::test]
    async fn a_dead_port_is_not_mistaken_for_a_busy_one() {
        let probe = identify_sidecar(&test_client(), free_port(), Duration::from_millis(50)).await;
        assert_eq!(probe, Probe::NotOurs, "拒否されたら死んでいる");
    }

    /// つながったのに答えない相手は、TCP の確認を足しても「応答しない」のまま（使用中の孤児の役）。
    #[tokio::test]
    async fn a_silent_listener_is_still_unanswered() {
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = silent.local_addr().unwrap().port();
        let probe = identify_sidecar(&test_client(), port, Duration::from_millis(300)).await;
        assert_eq!(probe, Probe::Unanswered, "つながったが答えない = 使用中でありうる");
        drop(silent);
    }

    /// 自分の孤児は止めて記録を消し、**止める要求が届かなければ記録を残す**（次の起動で再試行）。
    #[tokio::test]
    async fn a_sweep_keeps_the_record_of_an_orphan_it_could_not_stop() {
        let dir = tempfile::tempdir().unwrap();
        let (stoppable, stoppable_seen) = fake_sidecar(OnShutdown::Exits);
        let (stubborn, _) = fake_sidecar(OnShutdown::Ignores);
        ledger_add(dir.path(), ReadyFile { port: stoppable, pid: 1 });
        ledger_add(dir.path(), ReadyFile { port: stubborn, pid: 2 });

        let stopped = sweep_orphans(dir.path(), &test_client()).await;

        assert_eq!(stopped, 1, "止められたのは 1 件だけ");
        assert!(
            stoppable_seen
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with("POST /shutdown")),
            "自分の孤児には /shutdown を投げる"
        );
        let got = read_ledger(dir.path());
        assert_eq!(got.len(), 1, "止められなかった記録だけ残る: {got:?}");
        assert_eq!(got[0].port, stubborn);
    }

    /// **止める要求に答えても、止まらなければ止めたと言わない**（v0.5.6 項目 3e）。記録を残す。
    /// 以前は送れただけで成功とし、台帳から消していたので、止まらなかった孤児を二度と追えなかった。
    #[tokio::test]
    async fn an_orphan_that_answers_but_stays_is_not_counted_as_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let (polite, polite_seen) = fake_sidecar(OnShutdown::AnswersButStays);
        ledger_add(dir.path(), ReadyFile { port: polite, pid: 3 });

        assert!(!request_shutdown(polite, &test_client()).await, "止まっていない");
        let stopped = sweep_orphans(dir.path(), &test_client()).await;

        assert_eq!(stopped, 0);
        assert!(polite_seen.lock().unwrap().iter().any(|l| l.starts_with("POST /shutdown")));
        assert_eq!(read_ledger(dir.path()).len(), 1, "止まらなかった記録は残す");
    }

    /// 答えて待ち受けをやめたら、止まったと確かめられる。
    #[tokio::test]
    async fn an_orphan_that_answers_and_exits_is_confirmed_stopped() {
        let (exits, _) = fake_sidecar(OnShutdown::Exits);
        assert!(request_shutdown(exits, &test_client()).await);
    }

    /// **更新の前の確認は、生きているサイドカーだけを数え、止めない**（v0.5.6 項目 3e）。
    /// 死んだ記録（接続を拒否される）は数えない。
    #[tokio::test]
    async fn live_sidecars_are_counted_without_being_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let (alive, alive_seen) = fake_sidecar(OnShutdown::Exits);
        let dead = free_port();
        ledger_add(dir.path(), ReadyFile { port: alive, pid: 4 });
        ledger_add(dir.path(), ReadyFile { port: dead, pid: 5 });

        let live = live_sidecars(dir.path(), &test_client()).await;

        assert_eq!(live, [alive], "生きているものだけ");
        assert!(
            !alive_seen.lock().unwrap().iter().any(|l| l.starts_with("POST /shutdown")),
            "止めていない（所有者を見分けられないうちは止めない）"
        );
        assert_eq!(read_ledger(dir.path()).len(), 2, "記録も変えない");
    }

    /// 長く走る子プロセス（サイドカーの代役）。ポートは誰も待ち受けていないもの。
    fn long_running_handle(asset_root: &Path) -> SidecarHandle {
        let child = Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(Stdio::null())
            .kill_on_drop(false)
            .spawn()
            .expect("ping を起動できること");
        let pid = child.id().unwrap();
        SidecarHandle {
            asset_root: asset_root.to_path_buf(),
            port: free_port(),
            pid,
            child,
        }
    }

    /// **止まったのを見届けてから記録を消す**（2026-09-13、孤児掃除と同じ形）。
    ///
    /// 冒頭で消していたため、止めている数秒の間にアプリが落ちると**生きているサイドカーの
    /// 記録だけが消えた**。
    #[tokio::test]
    async fn a_shutdown_forgets_the_record_only_after_the_child_is_gone() {
        let client = test_client();

        // 止めている最中に中断すると、記録は残る
        let dir = tempfile::tempdir().unwrap();
        let handle = long_running_handle(dir.path());
        let (port, pid) = (handle.port, handle.pid);
        ledger_add(dir.path(), ReadyFile { port, pid });
        let interrupted =
            tokio::time::timeout(Duration::from_millis(100), shutdown_sidecar(handle, &client))
                .await;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
        assert!(interrupted.is_err(), "前提: 止めている最中に中断できていること");
        assert_eq!(
            read_ledger(dir.path()).len(),
            1,
            "止まるのを見届ける前に記録を消してはいけない"
        );

        // 最後まで走れば消える
        let dir = tempfile::tempdir().unwrap();
        let handle = long_running_handle(dir.path());
        ledger_add(
            dir.path(),
            ReadyFile {
                port: handle.port,
                pid: handle.pid,
            },
        );
        shutdown_sidecar(handle, &client).await.unwrap();
        assert!(read_ledger(dir.path()).is_empty(), "止まったら記録は消える");
    }

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

    /// stderr の行を集めるだけの受け手。
    fn collected() -> (
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        impl FnMut(&str) + Send + 'static,
    ) {
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = lines.clone();
        (lines, move |l: &str| sink.lock().unwrap().push(l.to_string()))
    }

    /// **UTF-8 として読めない行で stderr の読み取りを止めない**（2026-09-14、インストール版の実環境で発覚）。
    ///
    /// `lines()` は読めない行で `Err` を返し、`while let Ok(Some(..))` はそこで黙って抜けていた。
    /// 実機では起動の約 10 秒後から、以後の stderr が 1 行も `ugg.log` に残らなかった。
    /// **cp932 の行は日本語として読む**（v0.5.6 項目 2。サイドカーの stderr は実際に cp932 だった）。
    /// どちらの文字コードでも読めない行は置換文字で流し、その後も読み続ける。
    #[tokio::test]
    async fn a_line_that_is_not_utf8_does_not_stop_the_stderr_pump() {
        let mut bytes = b"before\r\n".to_vec();
        // cp932 の「既存」。UTF-8 としては読めない。
        bytes.extend_from_slice(b"ConnectionResetError: \x8a\xf9\x91\xb6\n");
        // UTF-8 でも Shift_JIS でも読めない。
        bytes.extend_from_slice(b"broken: \xff\xfe\n");
        bytes.extend_from_slice(b"after\n");
        let (lines, sink) = collected();
        spawn_stderr_pump(std::io::Cursor::new(bytes), sink).await;
        let got = lines.lock().unwrap().clone();
        assert_eq!(got.len(), 4, "読めない行で止まっている: {got:?}");
        assert_eq!(got[0], "before", "行末の CR を落としていない");
        assert_eq!(
            got[1], "ConnectionResetError: 既存",
            "cp932 の行を日本語として読んでいない: {got:?}"
        );
        assert!(
            got[2].starts_with("broken: ") && got[2].contains('\u{FFFD}'),
            "どちらでも読めない行を置換文字で流していない: {got:?}"
        );
        assert_eq!(got[3], "after", "読めない行の後が届いていない");
    }

    /// 読み取りそのものが失敗したら、**黙って終えずに理由を残す**（同上）。
    #[tokio::test]
    async fn a_failed_read_leaves_its_reason() {
        struct FailsAfterOneLine {
            sent: bool,
        }
        impl tokio::io::AsyncRead for FailsAfterOneLine {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                if self.sent {
                    return std::task::Poll::Ready(Err(std::io::Error::other("pipe broke")));
                }
                self.sent = true;
                buf.put_slice(b"one\n");
                std::task::Poll::Ready(Ok(()))
            }
        }
        let (lines, sink) = collected();
        spawn_stderr_pump(FailsAfterOneLine { sent: false }, sink).await;
        let got = lines.lock().unwrap().clone();
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0], "one");
        assert!(got[1].contains("pipe broke"), "止まった理由が残っていない: {got:?}");
    }
}

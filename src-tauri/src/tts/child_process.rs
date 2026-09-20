//! 子プロセスを起動し、出力を**行ごとに**流す（v0.5.6 項目 2、spec §6.0）。
//!
//! pip・モデル取得（`irodori_download::run_python`）と VOICEVOX のダウンローダが使う。
//! 以前はどちらも子プロセスが終わってから出力をまとめて読んでいたので、数 GB の取得中は
//! 画面が 1 行のまま固まり、固まっても待つだけだった（Irodori は更新の錠を握ったままになり、
//! 再起動まで使えない）。
//!
//! - stdout と stderr を別々のスレッドで読み、**行が確定してから**文字コードを判定する
//!   （読み取りの切れ目で多バイト文字が割れても化けないように）。区切りは `\n` と `\r` の両方で、
//!   `\r` だけで終わった行は進捗の上書き（`Line::overwritten`）。2 つの流れは届いた順に混ざるので、
//!   「最後の 1 行」を使う側は `Line::stream` で流れを選ぶ
//! - **無進捗**は「出力も、読み書きも、CPU も、決めた時間ずっと無い」で判定する。pip と
//!   huggingface_hub はパイプ越しだと取得中の進捗を出さず、pip は torch を展開している間も黙るので、
//!   出力だけで数えると正常な取得を止めてしまう。読み書きと CPU は Job Object の集計（孫も含む）で見る
//! - 止めるときは **Job Object ごと**止める（pip が起こしたビルドのプロセスまで）。子を 1 つ
//!   止めるだけだと、孫がパイプを握ったまま残って読み取りが終わらない。Job は「閉じたら中身ごと
//!   終わらせる」設定にしてあり、子が終わったあとに残った孫も片付く
//! - Job に入れられなかったときは、止め方も無進捗の判定も持たないので**無進捗では止めない**
//!   （出力だけで判定すると、正常な取得を止めうる）

use std::io::{ErrorKind, Read, Write};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAndIoAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// どちらの出力から来た行か。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stream {
    Stdout,
    Stderr,
}

/// 確定した 1 行（前後の空白は落としてある。空行は来ない）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Line<'a> {
    pub stream: Stream,
    pub text: &'a str,
    /// `\r` だけで終わった行（進捗の上書き。tqdm など）。記録に残す側は読み飛ばしてよい。
    pub overwritten: bool,
}

/// 子プロセスがどう終わったか。
#[derive(Debug)]
pub(crate) enum Ended {
    Exited(ExitStatus),
    /// 無進捗が続いたので、Job ごと止めた。
    Stalled,
}

/// 1 行の上限。改行を出さずに書き続けるプロセスで、メモリを使い切らないため。
const MAX_LINE_BYTES: usize = 64 * 1024;

/// 止めたあと・子が終わったあとに、残りの出力を待つ上限。
const DRAIN_GRACE: Duration = Duration::from_secs(5);

/// バイト列を行に組み立てる（純粋な状態機械。読み取りスレッドが 1 つずつ持つ）。
#[derive(Default)]
pub(crate) struct LineAssembler {
    buf: Vec<u8>,
    /// 直前が `\r` だった。次が `\n` なら普通の行末（Windows の `\r\n`）、それ以外なら上書き。
    cr: bool,
}

impl LineAssembler {
    /// `bytes` を足し、確定した行を `(文字列, 上書きか)` で `out` に積む。
    pub(crate) fn push(&mut self, bytes: &[u8], out: &mut Vec<(String, bool)>) {
        for &b in bytes {
            if self.cr {
                self.cr = false;
                if b == b'\n' {
                    self.flush(false, out);
                    continue;
                }
                self.flush(true, out);
            }
            match b {
                b'\n' => self.flush(false, out),
                b'\r' => self.cr = true,
                _ => {
                    self.buf.push(b);
                    if self.buf.len() >= MAX_LINE_BYTES {
                        self.flush(false, out);
                    }
                }
            }
        }
    }

    /// 出力の終わり。改行の無い最後の行も流す。
    pub(crate) fn finish(&mut self, out: &mut Vec<(String, bool)>) {
        self.cr = false;
        self.flush(false, out);
    }

    fn flush(&mut self, overwritten: bool, out: &mut Vec<(String, bool)>) {
        if self.buf.is_empty() {
            return;
        }
        let (clean, moved_up) = strip_ansi(&self.buf);
        self.buf.clear();
        // UTF-8 で読めなければ Shift_JIS（cp932）で読む（v0.5.6 項目 2）。
        let s = crate::tts::reader::decode_output_line(&clean);
        let t = s.trim();
        if !t.is_empty() {
            out.push((t.to_string(), overwritten || moved_up));
        }
    }
}

/// ANSI の制御の並びを落とし、**カーソルを上へ戻す並びがあったか**を返す。
///
/// 落とす理由は 2 つ。① 画面（設定パネルの最新の 1 行）と `ugg.log` に `[A` や `[31m` がそのまま
/// 出る ② VOICEVOX のダウンローダの色付きの行を、以前は 1 バイトずつ文字に積み直して落としており、
/// **日本語のエラー文が化けていた**（`アクセスが拒否されました` が読めなかった）。
///
/// 戻す並び（`ESC[A`）が入っていた行は、進捗の上書きとして扱う。tqdm は 2 本目以降の進捗バーを
/// 「`\n` で下げて描いて `ESC[A` で戻す」形で書くので、`\r` では終わらない（モデルを並行して取得する
/// ときに出る）。上書きとして扱わないと、失敗したときにログへ残す直前の行が進捗で埋まる。
///
/// バイト列のまま落として構わない: `ESC`(0x1B) は Shift_JIS の 1 バイト目にも 2 バイト目にも現れない。
fn strip_ansi(bytes: &[u8]) -> (Vec<u8>, bool) {
    if !bytes.contains(&0x1B) {
        return (bytes.to_vec(), false);
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut moved_up = false;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1B {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        match bytes.get(i) {
            // CSI: `ESC [` 引数 … 終端（0x40〜0x7E）
            Some(b'[') => {
                i += 1;
                while let Some(&b) = bytes.get(i) {
                    i += 1;
                    if (0x40..=0x7E).contains(&b) {
                        moved_up |= b == b'A';
                        break;
                    }
                }
            }
            // `ESC` + 1 文字（文字集合の切り替えなど）
            Some(_) => i += 1,
            None => {}
        }
    }
    (out, moved_up)
}

enum Msg {
    Line {
        stream: Stream,
        text: String,
        overwritten: bool,
    },
    /// 読めたが、まだ行になっていない（改行を出さない進捗など）。
    Activity,
}

fn spawn_reader<R: Read + Send + 'static>(mut src: R, stream: Stream, tx: mpsc::Sender<Msg>) {
    std::thread::spawn(move || {
        let mut asm = LineAssembler::default();
        let mut chunk = [0u8; 8192];
        let mut lines = Vec::new();
        loop {
            match src.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    asm.push(&chunk[..n], &mut lines);
                    if lines.is_empty() && tx.send(Msg::Activity).is_err() {
                        return;
                    }
                    for (text, overwritten) in lines.drain(..) {
                        if tx
                            .send(Msg::Line {
                                stream,
                                text,
                                overwritten,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    // 黙って抜けない（v0.5.5 のサイドカーの stderr と同じ規律）。
                    lines.push((format!("(出力の読み取りが止まりました: {e})"), false));
                    break;
                }
            }
        }
        asm.finish(&mut lines);
        for (text, overwritten) in lines {
            let _ = tx.send(Msg::Line {
                stream,
                text,
                overwritten,
            });
        }
    });
}

/// Job の中で何かが動いた印。前回と 1 つでも違えば「進捗がある」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Activity {
    /// 木全体の CPU 時間（100ns 単位）。終わったプロセスの分も含む。
    cpu: i64,
    /// 木全体の読み書きの量と回数の合計。
    io: u64,
}

/// 子プロセスを 1 つ入れる Job Object（閉じたら中身ごと終わらせる設定）。
struct Job(HANDLE);

impl Job {
    fn for_child(child: &Child) -> windows::core::Result<Job> {
        // SAFETY: 作った HANDLE は `Job` が持ち、Drop で 1 回だけ閉じる。構造体は大きさを渡して
        // 読み書きさせるだけで、所有権は移らない。子の HANDLE は `child` が生きている間は有効。
        unsafe {
            let job = Job(CreateJobObjectW(None, windows::core::PCWSTR::null())?);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const core::ffi::c_void,
                std::mem::size_of_val(&limits) as u32,
            )?;
            AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle()))?;
            Ok(job)
        }
    }

    /// 木全体の CPU 時間と読み書きの量。前回と違えば、何かしている。
    ///
    /// **読み書きを落としてはいけない。** pip が torch を展開している間のように、短い書き込みを
    /// 続ける子は CPU 時間がほとんど増えない（Windows の CPU 時間は約 15.6ms 刻みで課金されるので、
    /// 1 回の書き込みでは 0 のままになりやすい）。読み書きを見ずに CPU だけで判定すると、
    /// 正常に展開している最中に止めてしまう。
    fn activity(&self) -> Option<Activity> {
        let mut info = JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION::default();
        // SAFETY: 書き込み先は `info` で、大きさを正しく渡している。
        unsafe {
            QueryInformationJobObject(
                self.0,
                JobObjectBasicAndIoAccountingInformation,
                &mut info as *mut _ as *mut core::ffi::c_void,
                std::mem::size_of_val(&info) as u32,
                None,
            )
        }
        .ok()?;
        let io = &info.IoInfo;
        Some(Activity {
            cpu: info.BasicInfo.TotalUserTime + info.BasicInfo.TotalKernelTime,
            io: io
                .ReadTransferCount
                .wrapping_add(io.WriteTransferCount)
                .wrapping_add(io.OtherTransferCount)
                .wrapping_add(io.ReadOperationCount)
                .wrapping_add(io.WriteOperationCount)
                .wrapping_add(io.OtherOperationCount),
        })
    }

    fn terminate(&self) {
        // SAFETY: 自分が持っている Job の HANDLE。
        unsafe {
            let _ = TerminateJobObject(self.0, 1);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: `for_child` で作った HANDLE を 1 回だけ閉じる。
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// 無進捗を見る間隔。
const TICK: Duration = Duration::from_millis(250);

/// `cmd` を起動し、出力を行ごとに `on_line` へ流しながら終わるのを待つ（同期）。
///
/// - 窓は出さない（`CREATE_NO_WINDOW`）。`stdin` を渡せばそれを書いて閉じ、渡さなければ
///   標準入力は空（確認を求めるプロセスが入力待ちで固まらないように）
/// - `stall_after` の間、出力も読み書きも CPU も無ければ、Job ごと止めて `Ended::Stalled`
/// - 非同期の文脈から呼ぶときは `off_the_async_workers` で包む
pub(crate) fn run_streaming(
    mut cmd: Command,
    stdin: Option<&[u8]>,
    stall_after: Option<Duration>,
    mut on_line: impl FnMut(Line<'_>),
) -> std::io::Result<Ended> {
    cmd.creation_flags(crate::tts::irodori_download::CREATE_NO_WINDOW)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let job = match Job::for_child(&child) {
        Ok(job) => Some(job),
        Err(err) => {
            crate::ulog!("[child] 子プロセスを Job Object に入れられないので、無進捗では止めません: {err}");
            None
        }
    };
    if let (Some(bytes), Some(mut w)) = (stdin, child.stdin.take()) {
        let _ = w.write_all(bytes);
        let _ = w.flush();
    }

    let (tx, rx) = mpsc::channel();
    if let Some(out) = child.stdout.take() {
        spawn_reader(out, Stream::Stdout, tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        spawn_reader(err, Stream::Stderr, tx.clone());
    }
    drop(tx);

    let mut deliver = |msg: Msg| {
        if let Msg::Line {
            stream,
            text,
            overwritten,
        } = msg
        {
            on_line(Line {
                stream,
                text: &text,
                overwritten,
            });
        }
    };

    let stall_after = stall_after.filter(|_| job.is_some());
    let mut last_progress = Instant::now();
    let mut last_activity = job.as_ref().and_then(Job::activity);
    let mut last_check = Instant::now();
    let mut open = true;
    let exited = loop {
        if open {
            match rx.recv_timeout(TICK) {
                Ok(msg) => {
                    last_progress = Instant::now();
                    deliver(msg);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => open = false,
            }
        } else {
            // 出力は閉じたが、子はまだ動いている。
            std::thread::sleep(TICK);
        }
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if let (Some(limit), Some(job)) = (stall_after, job.as_ref()) {
            if last_check.elapsed() >= TICK {
                last_check = Instant::now();
                let now = job.activity();
                if now != last_activity {
                    last_activity = now;
                    last_progress = Instant::now();
                }
            }
            if last_progress.elapsed() >= limit {
                break None;
            }
        }
    };

    // 子が終わった／止めると決めた。孫がパイプを握っていても、Job を止めれば閉じる。
    match job.as_ref() {
        Some(job) => job.terminate(),
        None if exited.is_none() => {
            let _ = child.kill();
        }
        None => {}
    }
    let deadline = Instant::now() + DRAIN_GRACE;
    while let Ok(msg) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        deliver(msg);
    }
    match exited {
        Some(status) => Ok(Ended::Exited(status)),
        None => {
            let _ = child.wait();
            Ok(Ended::Stalled)
        }
    }
}

/// 同期の待ちを、非同期のワーカーを塞がずに行う（v0.5.4 で見送った件、v0.5.6 項目 2）。
///
/// マルチスレッドのランタイム（アプリ本体）の上では `block_in_place` で、ワーカーの持ち分を
/// 別のスレッドへ渡してから待つ。ランタイムの外や、1 スレッドのランタイム（`#[tokio::test]`）
/// では `block_in_place` が使えない（後者は panic する）ので、そのまま呼ぶ。
pub(crate) fn off_the_async_workers<R>(f: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assemble(chunks: &[&[u8]]) -> Vec<(String, bool)> {
        let mut asm = LineAssembler::default();
        let mut out = Vec::new();
        for c in chunks {
            asm.push(c, &mut out);
        }
        asm.finish(&mut out);
        out
    }

    fn owned(v: &[(&str, bool)]) -> Vec<(String, bool)> {
        v.iter().map(|(s, o)| (s.to_string(), *o)).collect()
    }

    /// `\r\n`（Windows の Python が書く行末）は上書きではない。`\r` だけなら上書き（tqdm）。
    #[test]
    fn crlf_is_a_line_end_and_a_lone_cr_is_an_overwrite() {
        assert_eq!(
            assemble(&[b"Collecting torch\r\nDownloading\r\n"]),
            owned(&[("Collecting torch", false), ("Downloading", false)])
        );
        assert_eq!(
            assemble(&[b"\r 10%|#\r 20%|##\r100%|###\n"]),
            owned(&[("10%|#", true), ("20%|##", true), ("100%|###", false)])
        );
    }

    /// 読み取りの切れ目が `\r` と `\n` の間に来ても、`\r\n` として扱う。
    #[test]
    fn a_crlf_split_across_reads_is_still_one_line_end() {
        assert_eq!(
            assemble(&[b"first\r", b"\nsecond\n"]),
            owned(&[("first", false), ("second", false)])
        );
    }

    /// **行が確定してから文字コードを判定する。** cp932 の 2 バイト文字が読み取りの切れ目で
    /// 割れても化けない（割れた半分ずつを判定すると、両方とも読めずに置換文字になる）。
    #[test]
    fn a_multibyte_char_split_across_reads_is_decoded_whole() {
        // cp932 の「既存の接続」
        let line: &[u8] = &[0x8a, 0xf9, 0x91, 0xb6, 0x82, 0xcc, 0x90, 0xda, 0x91, 0xb1];
        let got = assemble(&[&line[..3], &line[3..], b"\r\n"]);
        assert_eq!(got, owned(&[("既存の接続", false)]));
    }

    /// 改行の無い最後の行も落とさない。空行と空白だけの行は流さない。
    #[test]
    fn the_last_line_without_a_newline_is_kept_and_blank_lines_are_not() {
        assert_eq!(
            assemble(&[b"a\n\n   \r\nlast"]),
            owned(&[("a", false), ("last", false)])
        );
    }

    /// **色付けの制御文字を落とし、日本語は壊さない。** 以前 VOICEVOX のダウンローダの側で
    /// 1 バイトずつ文字に積み直しており、`アクセスが拒否されました` が化けていた。
    #[test]
    fn ansi_sequences_are_dropped_without_breaking_japanese() {
        let mut line: Vec<u8> = b"\x1b[31mError:\x1b[0m ".to_vec();
        // cp932 の「アクセスが拒否されました」
        line.extend_from_slice(&[
            0x83, 0x41, 0x83, 0x4e, 0x83, 0x5a, 0x83, 0x58, 0x82, 0xaa, 0x8b, 0x91, 0x94, 0xdb,
            0x82, 0xb3, 0x82, 0xea, 0x82, 0xdc, 0x82, 0xb5, 0x82, 0xbd,
        ]);
        line.extend_from_slice(b"\r\n");
        assert_eq!(
            assemble(&[&line]),
            owned(&[("Error: アクセスが拒否されました", false)])
        );
    }

    /// **入れ子の進捗バーも上書きとして扱う。** tqdm は 2 本目以降のバーを「`\n` で下げて描いて
    /// `ESC[A` で戻す」形で書くので `\r` では終わらない。上書きにしないと、失敗したときログに
    /// 残す直前の行が進捗で埋まる。
    #[test]
    fn a_nested_progress_bar_is_an_overwrite_too() {
        let got = assemble(&[b"\n\r 45%|####  | 900M/2.00G\x1b[A\n\r 46%|##### | 920M/2.00G\x1b[A\n"]);
        assert_eq!(
            got,
            owned(&[("45%|####  | 900M/2.00G", true), ("46%|##### | 920M/2.00G", true)])
        );
    }

    /// 改行を出さずに書き続けても、上限で区切って流す（メモリを使い切らない）。
    #[test]
    fn an_endless_line_is_cut_at_the_limit() {
        let long = vec![b'x'; MAX_LINE_BYTES + 10];
        let got = assemble(&[&long]);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0.len(), MAX_LINE_BYTES);
        assert_eq!(got[1].0.len(), 10);
    }

    /// cmd.exe は `\"` を解さないので、スクリプトは引用し直さずにそのまま渡す。
    fn cmd(script: &str) -> Command {
        let mut c = Command::new("cmd.exe");
        c.args(["/d", "/c"]).raw_arg(script);
        c
    }

    /// **行は子が動いている間に届く**（v0.5.6 項目 2 の本体）。以前は終わってから一括で読んでおり、
    /// 最初の行も終了と同時にしか届かなかった。
    #[test]
    fn lines_arrive_while_the_child_is_still_running() {
        let started = Instant::now();
        let mut seen: Vec<(String, Duration)> = Vec::new();
        let ended = run_streaming(
            cmd("echo first& ping -n 3 127.0.0.1 >nul& echo second"),
            None,
            None,
            |l| seen.push((l.text.to_string(), started.elapsed())),
        )
        .unwrap();
        assert!(matches!(ended, Ended::Exited(s) if s.success()), "{ended:?}");
        let texts: Vec<&str> = seen.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(texts, ["first", "second"]);
        assert!(
            seen[0].1 + Duration::from_millis(1500) <= seen[1].1,
            "最初の行が、終わる直前まで届いていない: {seen:?}"
        );
    }

    /// stdout と stderr を取り違えない（「最後の 1 行」を stderr から選ぶ側の前提）。
    #[test]
    fn each_line_carries_its_stream() {
        let mut seen: Vec<(Stream, String)> = Vec::new();
        run_streaming(cmd("echo out& echo err 1>&2"), None, None, |l| {
            seen.push((l.stream, l.text.to_string()))
        })
        .unwrap();
        seen.sort_by_key(|(s, _)| *s == Stream::Stderr);
        assert_eq!(
            seen,
            [
                (Stream::Stdout, "out".to_string()),
                (Stream::Stderr, "err".to_string())
            ]
        );
    }

    /// 渡した入力を書いて閉じる（VOICEVOX のダウンローダは利用規約への同意を標準入力で受ける）。
    #[test]
    fn the_given_input_reaches_the_child() {
        let mut seen = Vec::new();
        run_streaming(cmd("findstr ."), Some(b"agreed\r\n"), None, |l| {
            seen.push(l.text.to_string())
        })
        .unwrap();
        assert_eq!(seen, ["agreed"]);
    }

    /// 終了コードを失わない。
    #[test]
    fn a_failure_keeps_its_exit_code() {
        let ended = run_streaming(cmd("exit 3"), None, None, |_| {}).unwrap();
        assert!(matches!(ended, Ended::Exited(s) if s.code() == Some(3)), "{ended:?}");
    }

    /// **黙って止まった子は、孫ごと止める。** 孫が生き残ると、パイプを握ったまま読み取りが
    /// 終わらず、しかも止めたはずの処理が裏で続く（ここでは印のファイルを書く）。
    #[test]
    fn a_silent_child_is_stopped_with_its_whole_tree() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("grandchild-finished");
        // 孫は 3 秒眠ってから印を書く。子（cmd）は孫を起こしたあと 30 秒眠る。
        // 眠るのに ping は使えない — ICMP の読み書きが Job の集計に数えられ、止まっていると判定されない。
        // `waitfor` は来ない合図を待つだけで、読み書きも CPU も使わない。**合図の名前はマシン全体で
        // 共有される**ので、pid を混ぜて一意にする（同じ名前で待っているものがあると、あとから来た
        // `waitfor` は即エラーで終わり、「無進捗で止めた」を確かめられないまま緑にも赤にもなる）。
        let tag = std::process::id();
        let script = format!(
            "start \"\" /b cmd /d /c \"waitfor /t 3 UggNever{tag}A >nul 2>&1 & echo x> \"{}\"\" & waitfor /t 30 UggNever{tag}B >nul 2>&1",
            marker.display()
        );
        let started = Instant::now();
        let ended = run_streaming(cmd(&script), None, Some(Duration::from_millis(1500)), |_| {}).unwrap();
        assert!(matches!(ended, Ended::Stalled), "{ended:?}");
        assert!(
            started.elapsed() < Duration::from_secs(12),
            "止めるまでに {:?}",
            started.elapsed()
        );
        std::thread::sleep(Duration::from_secs(5));
        assert!(!marker.exists(), "孫が生き残って印を書いた");
    }

    /// **出力が無くても、読み書きしている間は止めない。** pip は torch を展開している間、
    /// huggingface_hub はパイプ越しの取得の間、何も出さない。出力だけで判定すると正常な処理を止める。
    #[test]
    fn a_quiet_child_that_keeps_writing_files_is_not_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("busy.txt");
        // 約 4 秒、0.1 秒おきにファイルへ書く。出力は出さない。
        let mut c = Command::new("powershell.exe");
        c.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "1..40 | ForEach-Object {{ Add-Content -LiteralPath '{}' -Value x; Start-Sleep -Milliseconds 100 }}",
                file.display()
            ),
        ]);
        let ended = run_streaming(c, None, Some(Duration::from_millis(1500)), |_| {}).unwrap();
        assert!(matches!(ended, Ended::Exited(s) if s.success()), "{ended:?}");
    }

    /// **進捗の集計は読み書きも数える**（上のテストは CPU だけでも通りうるので、集計そのものを見る）。
    /// 静かに書き続ける子は CPU 時間がほとんど増えない（Windows の CPU 時間は約 15.6ms 刻み）。
    /// `Job::activity` から読み書きを落とすと、このテストが落ちる。
    #[test]
    fn the_accounting_counts_reads_and_writes_not_only_cpu() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("busy.txt");
        let mut c = Command::new("powershell.exe");
        c.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "1..60 | ForEach-Object {{ Add-Content -LiteralPath '{}' -Value x; Start-Sleep -Milliseconds 100 }}",
                file.display()
            ),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        let mut child = c.spawn().expect("powershell を起動できること");
        let job = Job::for_child(&child).expect("Job に入れられること");
        // 起動の読み書きが落ち着くのを待つ（1 件目が書かれたら本処理に入っている）。
        let deadline = Instant::now() + Duration::from_secs(20);
        while !file.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let before = job.activity().expect("集計が取れること");
        std::thread::sleep(Duration::from_millis(1200));
        let after = job.activity().expect("集計が取れること");
        drop(job); // Job を閉じると中身ごと終わる
        let _ = child.wait();
        assert!(
            after.io > before.io,
            "書き続けているのに読み書きが増えていない: {before:?} → {after:?}"
        );
    }

    /// `#[tokio::test]` は 1 スレッドのランタイムで、`block_in_place` は panic する。そこでも動くこと。
    #[tokio::test]
    async fn waiting_inside_a_single_threaded_runtime_does_not_panic() {
        let got = off_the_async_workers(|| 7);
        assert_eq!(got, 7);
    }

    /// マルチスレッドのランタイムでは、待っている間もほかのタスクが進む。
    ///
    /// **待ち合わせと判定に tokio のタイマーを使わない。** ワーカーが 1 本だけのときは、
    /// タイマーを進めるのもそのワーカーなので、塞がれるとタイマーごと止まる。`block_on` している
    /// こちらのスレッドはワーカーではないので、タイマーは進まないまま経過し、
    /// **塞いでいても `timeout` に掛からず通ってしまう**（このテストの最初の形がそうだった。
    /// リポジトリの外で tokio 1.52.3 に同じ手順を写して確かめた: `block_in_place` を外しても
    /// 3 回とも通り、下の形では 3 回とも 1.5 秒で落ちた）。
    #[test]
    fn waiting_on_a_multi_threaded_runtime_lets_other_tasks_run() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .unwrap();
        rt.block_on(async {
            let (started, has_started) = std::sync::mpsc::channel();
            let blocked = tokio::spawn(async move {
                off_the_async_workers(move || {
                    let _ = started.send(());
                    std::thread::sleep(Duration::from_millis(1500));
                })
            });
            // 1 本しかないワーカーが待ちを始めたことを確かめてから、別のタスクを積む。
            has_started
                .recv_timeout(Duration::from_secs(5))
                .expect("同期待ちが始まらない");
            let queued = Instant::now();
            let ran_after = tokio::spawn(async move { queued.elapsed() }).await.unwrap();
            assert!(
                ran_after < Duration::from_millis(1000),
                "ワーカー 1 本を同期待ちが塞いでいる: {ran_after:?}"
            );
            blocked.await.unwrap();
        });
    }
}

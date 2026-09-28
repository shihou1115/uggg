//! ファイルログ (`%APPDATA%\ugg\ugg.log`)。
//!
//! **なぜ必要か**: spec §5 は `%APPDATA%\ugg\` の中身として「DB、TTS 資産、ログ」を
//! 挙げているが、出力先が実装されていなかった。コード中の 50 件超の `eprintln!` は
//! dev のコンソールにしか出ず、**リリース版ではコンソールが無いため 1 行も残らない**。
//! 「keyring が保存できていない」「補充が毎回タイムアウトしている」といった無言の
//! 失敗を、後から確認する手段が無かった。
//!
//! 意図的に小さく作る: 追記 + サイズによる 1 世代ローテーションのみ。
//! ログレベルもフィルタも入れない (必要になってから足す)。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::Local;

/// 1 ファイルの上限。超えたら `ugg.log` → `ugg.log.1` へ 1 世代だけ退避する。
const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// 出力先。`init` を呼ぶまでは None で、その間はファイルへ書かない
/// (stderr へは常に出るので dev の挙動は変わらない)。
static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// 出力先を確定する。アプリ起動時に 1 回だけ呼ぶ。
pub fn init(dir: &std::path::Path) {
    if let Err(err) = std::fs::create_dir_all(dir) {
        eprintln!("[log] ログディレクトリを作れません {}: {err}", dir.display());
        return;
    }
    match LOG_PATH.lock() {
        Ok(mut g) => *g = Some(dir.join("ugg.log")),
        Err(poisoned) => *poisoned.into_inner() = Some(dir.join("ugg.log")),
    }
    write_line(&format!("=== ugg {} 起動 ===", env!("CARGO_PKG_VERSION")));
}

/// 1 行書く。**失敗しても何もしない** (ログのためにアプリを壊さない)。
pub fn write_line(line: &str) {
    // **poison しても panic しない。** ここで panic すると、パニックフックが
    // write_line を呼ぶ構造上「フック内 panic → abort」になり、起動時 panic の
    // ダイアログごと失われる。診断のための機構が診断を殺してはいけない。
    let guard = match LOG_PATH.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let Some(path) = guard.as_ref() else {
        return;
    };
    rotate_if_needed(path);
    let stamped = format!("{} {}\n", Local::now().format("%Y-%m-%d %H:%M:%S%.3f"), line);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(stamped.as_bytes());
    }
}

fn rotate_if_needed(path: &std::path::Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() < MAX_BYTES {
        return;
    }
    let _ = std::fs::rename(path, path.with_extension("log.1"));
}

/// ユーザーが設定した値・ユーザーの入力から作った URL を、ログ・エラー文・画面へ返す文に書くときの形
/// （spec §5、v0.5.8 項目 1）。**スキームとホスト（とポート）まで。** パス・クエリ・フラグメント・利用者名と
/// パスワードは書かない — 公開していない予定表の URL は合言葉を、時事ネタの検索はユーザーの関心事を、天気は
/// 座標をそこに含み、ログは履歴クリアの対象外なので、載せると消す手段が無いまま溜まる。
/// 何の取得で失敗したかは文脈の文言で分かる。
pub fn url_for_log(url: &str) -> String {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        // 読めない値はそのまま書かない（入力の打ち間違いに合言葉が混ざりうる）。
        return "（URL として読めない値）".to_string();
    };
    match (parsed.host_str(), parsed.port()) {
        (Some(host), Some(port)) => format!("{}://{host}:{port}", parsed.scheme()),
        (Some(host), None) => format!("{}://{host}", parsed.scheme()),
        (None, _) => format!("{}:（ホストの無い URL）", parsed.scheme()),
    }
}

/// reqwest のエラーから URL を外す（spec §5、v0.5.8 項目 1）。
///
/// reqwest のエラー文は、接続の失敗だけでなく状態コード（`error_for_status()`）や本文の読み取り
/// （`.text()` / `.json()` / `.bytes()`）の失敗でも末尾に `for url (...)` を付け、`{err:#}` で包んだ
/// 文脈の外へそのまま出る。**reqwest の呼び出しにはすべて一律に当てる**（取得先が出荷物に固定した公開の
/// URL でも同じ。例外を作ると次に足した取得で漏れる）。
pub trait StripUrl<T> {
    fn strip_url(self) -> Result<T, reqwest::Error>;
}

impl<T> StripUrl<T> for Result<T, reqwest::Error> {
    fn strip_url(self) -> Result<T, reqwest::Error> {
        self.map_err(reqwest::Error::without_url)
    }
}

/// `eprintln!` の置き換え。stderr にも出しつつファイルにも残す。
///
/// dev ではこれまでどおりコンソールに出て、リリース版でもファイルに残る。
#[macro_export]
macro_rules! ulog {
    ($($arg:tt)*) => {{
        let __line = format!($($arg)*);
        eprintln!("{}", __line);
        $crate::system::log::write_line(&__line);
    }};
}

/// 取得の失敗を 3 通り作るテスト用の HTTP サーバー（v0.5.8 項目 1。カレンダー・LLM の経路のテストも使う）。
#[cfg(test)]
pub(crate) mod http_fixture {
    use std::io::{Read, Write};

    /// 失敗の作り方。
    #[derive(Clone, Copy, Debug)]
    pub enum Failure {
        /// 誰も待ち受けていない（接続の失敗）。
        Refused,
        /// 404 を返す（状態コードの失敗）。
        NotFound,
        /// 200 で長さを約束しておいて、途中で切る（本文の読み取りの失敗）。
        CutBody,
    }

    pub const ALL: [Failure; 3] = [Failure::Refused, Failure::NotFound, Failure::CutBody];

    /// その失敗を起こす `http://127.0.0.1:<port>` を返す（末尾の `/` なし）。
    pub fn base_url(failure: Failure) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        match failure {
            Failure::Refused => drop(listener),
            Failure::NotFound | Failure::CutBody => {
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
                        let reply: &[u8] = match failure {
                            Failure::NotFound => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            _ => b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n{\"trunc",
                        };
                        let _ = stream.write_all(reply);
                        let _ = stream.flush();
                        // 約束した長さを送らずに切る
                    }
                });
            }
        }
        format!("http://127.0.0.1:{port}")
    }

    /// テスト用のクライアント（環境の proxy 設定に左右されないように）。
    pub fn client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::http_fixture::{self, Failure};
    use super::*;

    /// 合言葉（パス・クエリ・利用者情報）が消え、スキームとホストとポートだけが残る。
    #[test]
    fn a_url_is_cut_down_to_its_host() {
        assert_eq!(
            url_for_log("https://calendar.example.com/ical/abc/private-SECRET/basic.ics?token=SECRET#x"),
            "https://calendar.example.com"
        );
        assert_eq!(url_for_log("http://127.0.0.1:1234/v1/SECRET"), "http://127.0.0.1:1234");
        assert_eq!(url_for_log("https://user:SECRET@example.com/a"), "https://example.com");
        assert_eq!(url_for_log("http://[::1]:8080/SECRET"), "http://[::1]:8080");
        // 読めない値は、そのまま書かない（打ち間違いに合言葉が混ざりうる）
        assert!(!url_for_log("not a url SECRET").contains("SECRET"));
        assert!(!url_for_log("mailto:SECRET@example.com").contains("SECRET"));
    }

    /// reqwest のエラーから URL が消える — 接続・状態コード・本文の読み取りの 3 通りとも。
    /// 対照として、外さなければ URL が出ることも見る（テストが空振りしていないこと）。**本文の読み取りの失敗は、
    /// reqwest 0.12 ではいまのところ URL を付けない**（実測）ので対照は接続と状態コードだけ。付けないからと
    /// 外さずにおくと、reqwest の版を上げたときに黙って漏れるので、本文の読み取りにも一律に当てる。
    #[tokio::test]
    async fn reqwest_errors_lose_their_url_in_every_failure() {
        for failure in http_fixture::ALL {
            let url = format!("{}/private-SECRET/basic.ics?token=SECRET", http_fixture::base_url(failure));
            let attempt = |strip: bool| {
                let url = url.clone();
                async move {
                    let sent = http_fixture::client().get(&url).send().await;
                    let sent = if strip { sent.strip_url() } else { sent };
                    let resp = sent.map_err(anyhow::Error::from)?;
                    let checked = resp.error_for_status();
                    let checked = if strip { checked.strip_url() } else { checked };
                    let resp = checked.map_err(anyhow::Error::from)?;
                    let text = resp.text().await;
                    let text = if strip { text.strip_url() } else { text };
                    text.map_err(anyhow::Error::from)
                }
            };
            let bare = attempt(false).await.expect_err(&format!("{failure:?} で失敗しなかった"));
            if !matches!(failure, Failure::CutBody) {
                assert!(format!("{bare:#}").contains("SECRET"), "{failure:?}: 対照に URL が出ない: {bare:#}");
            }
            let stripped = attempt(true).await.expect_err(&format!("{failure:?} で失敗しなかった"));
            let text = format!("{stripped:#}");
            assert!(!text.contains("SECRET"), "{failure:?}: URL が残っている: {text}");
            assert!(!text.contains("127.0.0.1"), "{failure:?}: URL が残っている: {text}");
        }
    }

    /// **全経路がこの規則を通る**（v0.5.8 項目 1。テキストの契約）。reqwest の失敗しうる呼び出し
    /// （`.send()` / `error_for_status()` / `.text()` / `.json()` / `.bytes()`）のすぐ後ろには、
    /// `.strip_url()` か、エラーを捨てる形（`.unwrap_or_default()` / `match … {` / 文の終わり /
    /// `and_then` の閉じ括弧）しか来ない。取得先のホストが固定の経路（時事ネタ・天気・地名検索）は
    /// 通信なしでは失敗を作れないので、ここで固定する。**テスト自身の文字列に当たらないよう、各ファイルの
    /// `#[cfg(test)]` より前だけを読む。**
    #[test]
    fn every_reqwest_call_strips_its_url() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut checked = 0;
        let mut bad = Vec::new();
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let body = std::fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
                let body = body.split("#[cfg(test)]").next().unwrap_or("");
                for (start, end) in fallible_reqwest_calls(body) {
                    let rest: String = body[end..].split_whitespace().collect::<Vec<_>>().join("");
                    // 行コメントを挟む書き方（`.strip_url()\n// …\n.with_context`）はそのままでよい
                    let ok = rest.starts_with(".strip_url()")
                        || rest.starts_with(".unwrap_or_default()")
                        || rest.starts_with('{')
                        || rest.starts_with(';')
                        || rest.starts_with(").strip_url()")
                        // `.send().await.and_then(|r| r.error_for_status()).strip_url()`（まとめて外す）
                        || rest.starts_with(".and_then(|r|r.error_for_status()).strip_url()");
                    checked += 1;
                    if !ok {
                        let line = body[..start].lines().count();
                        bad.push(format!("{}:{line}: {}", path.display(), &rest[..rest.len().min(40)]));
                    }
                }
            }
        }
        assert!(checked >= 20, "走査が空振りしている（{checked} 件）");
        assert!(bad.is_empty(), "URL を外していない reqwest の呼び出し:\n{}", bad.join("\n"));
    }

    /// reqwest の失敗しうる呼び出しの位置（始まりと、`.await` まで含めた終わり）。
    /// `.send()` / `.text()` / `.bytes()` / `.json()` / `.json::<T>()` は後ろの `.await` まで、
    /// `.error_for_status()` はそれ自体。
    fn fallible_reqwest_calls(body: &str) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut i = 0;
        while let Some(off) = body[i..].find('.') {
            let start = i + off;
            let rest = &body[start + 1..];
            let after_name = ["send()", "text()", "bytes()", "json()"]
                .iter()
                .find(|n| rest.starts_with(**n))
                .map(|n| start + 1 + n.len())
                .or_else(|| {
                    // `.json::<T>()`
                    let generic = rest.strip_prefix("json::<")?;
                    let close = generic.find(">()")?;
                    Some(start + 1 + "json::<".len() + close + ">()".len())
                });
            if let Some(end) = after_name {
                let tail = &body[end..];
                let trimmed = tail.trim_start();
                if trimmed.starts_with(".await") {
                    let await_end = end + (tail.len() - trimmed.len()) + ".await".len();
                    out.push((start, await_end));
                    i = await_end;
                    continue;
                }
            } else if rest.starts_with("error_for_status()") {
                let end = start + 1 + "error_for_status()".len();
                out.push((start, end));
                i = end;
                continue;
            }
            i = start + 1;
        }
        out
    }

    /// 走査そのものの確かめ（手で書いた照合なので、取りこぼしが無いこと）。
    #[test]
    fn the_scan_finds_each_kind_of_call() {
        let body = "a.send()\n  .await.x; b.text().await; c.bytes() .await; d.json::<T>().await; e.error_for_status(); f.send(); g.json()\n.await";
        assert_eq!(fallible_reqwest_calls(body).len(), 6);
    }

    #[test]
    fn writes_and_rotates() {
        let dir = tempfile::tempdir().unwrap();
        init(dir.path());
        write_line("テスト行");
        let path = dir.path().join("ugg.log");
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("起動"), "{body}");
        assert!(body.contains("テスト行"), "{body}");

        // 上限を超えたら 1 世代だけ退避する。
        std::fs::write(&path, vec![b'x'; (MAX_BYTES + 1) as usize]).unwrap();
        write_line("ローテーション後");
        assert!(dir.path().join("ugg.log.1").is_file(), "退避ファイルが無い");
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("ローテーション後"), "{after}");
        assert!(!after.contains("xxxx"), "新しいファイルに旧内容が残っている");

        // 後片付け: 他テストへ影響させないため出力先を戻す。
        *LOG_PATH.lock().unwrap() = None;
    }

    #[test]
    fn write_before_init_is_noop() {
        // init 前 (LOG_PATH = None) でもパニックしないこと。
        let saved = LOG_PATH.lock().unwrap().take();
        write_line("どこにも書かれない");
        *LOG_PATH.lock().unwrap() = saved;
    }
}

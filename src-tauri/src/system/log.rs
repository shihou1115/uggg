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
    *LOG_PATH.lock().expect("log path poisoned") = Some(dir.join("ugg.log"));
    write_line(&format!("=== ugg {} 起動 ===", env!("CARGO_PKG_VERSION")));
}

/// 1 行書く。**失敗しても何もしない** (ログのためにアプリを壊さない)。
pub fn write_line(line: &str) {
    let guard = LOG_PATH.lock().expect("log path poisoned");
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

#[cfg(test)]
mod tests {
    use super::*;

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

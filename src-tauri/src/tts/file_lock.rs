//! プロセスをまたぐ錠（v0.5.6 項目 3f、spec §6.0）。
//!
//! Irodori の導入・更新の錠（`IrodoriBusyGuard`）はプロセスの中だけで効いていたので、2 つの ugg が
//! 同時に動くと、退避と復元を互いに壊したり、片方の更新中にもう片方が入れ替え途中の site-packages で
//! サイドカーを起動したりできた。
//!
//! **ファイルのバイト範囲の錠（`LockFileEx`）**で作る:
//! - **プロセスが落ちたら OS が外す。** ファイルの有無で判定すると、落ちたときに永久に更新できなくなる
//! - 錠を掛けるのは開いたファイルの先頭 1 バイトだけで、ファイルを開くこと自体は妨げない
//!   （共有モードで締め出す方式は、ウイルス対策や検索インデクサが開いただけで「ほかの ugg が更新中」と
//!   誤判定する）
//! - 名前付きミューテックスは所有権がスレッドに紐づき、`await` でワーカーを移る tokio のコマンドと
//!   噛み合わない（同じスレッドの 2 回目は黙って成功する）ので使わない
//! - 錠は**ハンドル単位**なので、同じプロセスの中でも別の取得とは衝突する（プロセス内の二重取得は
//!   呼び出し側が先に弾く）
//!
//! **効かない条件**: `%APPDATA%` が MSIX の複製で別物に見えるプロセス（Claude のツールから起動した dev
//! など）とは錠が共有されない。ツールから dev・更新を実行しない規律（CLAUDE.md）が前提。

use std::fs::{File, OpenOptions};
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows::core::HRESULT;
use windows::Win32::Foundation::{ERROR_LOCK_VIOLATION, HANDLE};
use windows::Win32::Storage::FileSystem::{
    LockFileEx, UnlockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
};
use windows::Win32::System::IO::OVERLAPPED;

/// 握っている間だけ有効な錠。落とすと外れる（プロセスが落ちても OS が外す）。
///
/// `std::fs::File` を持つので `Send`（`HANDLE` をそのまま持つと `Send` でなくなり、
/// `async fn` の中で `await` をまたいで持てない）。
#[derive(Debug)]
pub(crate) struct FileLock {
    file: File,
}

impl FileLock {
    /// 錠を試す（待たない）。取れたら `Some`、ほかのハンドル（別のプロセス、または同じプロセスの
    /// 別の取得）が握っていれば `None`。ファイルが無ければ作る（中身は使わない。消さない）。
    pub(crate) fn try_acquire(path: &Path) -> std::io::Result<Option<FileLock>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let mut overlapped = OVERLAPPED::default();
        // SAFETY: `file` が生きている間の有効な HANDLE。`overlapped` は呼び出しの間だけ使われる
        // （同期で開いたファイルなので、呼び出しは完了してから返る）。
        let locked = unsafe {
            LockFileEx(
                HANDLE(file.as_raw_handle()),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut overlapped,
            )
        };
        match locked {
            Ok(()) => Ok(Some(FileLock { file })),
            Err(err) if err.code() == HRESULT::from_win32(ERROR_LOCK_VIOLATION.0) => Ok(None),
            Err(err) => Err(std::io::Error::other(err)),
        }
    }

    /// ほかが握っているか（取れたらすぐ放す）。確かめられなければ偽（止める側に倒さない —
    /// 合成を VOICEVOX へ落とすだけで、害は小さいが、理由の無い劣化は避ける）。
    pub(crate) fn is_held_elsewhere(path: &Path) -> bool {
        matches!(Self::try_acquire(path), Ok(None))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let mut overlapped = OVERLAPPED::default();
        // SAFETY: 自分が掛けた範囲を外す。失敗しても、続く `File` の drop（ハンドルを閉じる）で外れる。
        unsafe {
            let _ = UnlockFileEx(HANDLE(self.file.as_raw_handle()), 0, 1, 0, &mut overlapped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// 同じプロセスでも、別の取得とは衝突する（錠はハンドル単位）。放せばまた取れる。
    #[test]
    fn a_held_lock_blocks_another_acquisition_until_released() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.lock");
        let first = FileLock::try_acquire(&path).unwrap().expect("1 本目は取れる");
        assert!(FileLock::try_acquire(&path).unwrap().is_none(), "握られている間は取れない");
        assert!(FileLock::is_held_elsewhere(&path));
        drop(first);
        assert!(FileLock::try_acquire(&path).unwrap().is_some(), "放したら取れる");
        assert!(!FileLock::is_held_elsewhere(&path));
    }

    /// **別のプロセスが握っていれば取れず、そのプロセスが落ちたら取れる**（v0.5.6 項目 3f の要）。
    /// ファイルの有無で判定すると、落ちたときに永久に更新できなくなる。
    #[test]
    fn a_lock_held_by_another_process_is_released_when_it_dies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update.lock");
        let ready = dir.path().join("locked.flag");
        // 別のプロセス（PowerShell）が同じ 1 バイトに錠を掛けて眠る。FileStream.Lock は LockFile。
        let script = format!(
            "$f=[System.IO.File]::Open('{}','OpenOrCreate','ReadWrite','ReadWrite'); $f.Lock(0,1); \
             Set-Content -LiteralPath '{}' -Value x; Start-Sleep -Seconds 60",
            path.display(),
            ready.display()
        );
        let mut other = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("powershell を起動できること");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(ready.exists(), "別のプロセスが錠を掛け終えていない");

        let while_alive = FileLock::try_acquire(&path).unwrap();
        other.kill().unwrap();
        let _ = other.wait();
        let after_death = FileLock::try_acquire(&path).unwrap();

        assert!(while_alive.is_none(), "別のプロセスが握っている間は取れない");
        assert!(after_death.is_some(), "握っていたプロセスが落ちたら取れる（OS が外す）");
    }
}

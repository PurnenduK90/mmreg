//! Inter-process lock per physical page, held for one access or read-modify-write.
//!
//! Lock files (`<page base>-<page end>.lock`) are never deleted: unlinking one while another
//! process waits on it lets two processes hold "the" lock on different inodes. A lock is released
//! when its file is closed, including when the process exits.

use crate::error::MmregError;
use crate::mmio::page_size;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Locks the page containing `address`; the lock is held until the returned file is dropped.
pub(crate) fn lock_page(address: u64) -> Result<File, MmregError> {
    lock_page_in(lock_dir(), address)
}

pub(crate) fn lock_page_in(dir: &Path, address: u64) -> Result<File, MmregError> {
    let page = page_size();
    let base = address & !(page - 1);
    let path = dir.join(format!(
        "{:016x}-{:016x}.lock",
        base,
        base.wrapping_add(page)
    ));
    let file = open_lock_file(&path).map_err(|e| MmregError::LockOpen {
        lock_path: path,
        details: e.to_string(),
    })?;
    file.lock().map_err(|e| MmregError::LockAcquire {
        details: e.to_string(),
    })?;
    Ok(file)
}

/// Opens a lock file read-only (enough for `flock`, whoever created it), creating it if missing.
///
/// The plain open comes first because, with `fs.protected_regular`, even root may not `O_CREAT`
/// a file that another user owns in a sticky directory; the second plain open covers losing the
/// creation race to such a user.
fn open_lock_file(path: &Path) -> io::Result<File> {
    match File::open(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .or_else(|_| File::open(path)),
        result => result,
    }
}

/// `/run/lock/mmreg`, else `/tmp/mmreg-lock`, else `<temp dir>/mmreg-lock`; created
/// world-writable and sticky so every user shares the same lock files.
fn lock_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        #[cfg(unix)]
        for dir in ["/run/lock/mmreg", "/tmp/mmreg-lock"] {
            if fs::create_dir_all(dir).is_ok() {
                use std::os::unix::fs::PermissionsExt;
                // Fails unless we created it; whoever did set the same mode.
                let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o1777));
                return PathBuf::from(dir);
            }
        }
        let dir = std::env::temp_dir().join("mmreg-lock");
        let _ = fs::create_dir_all(&dir);
        dir
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    #[test]
    fn test_page_lock_excludes_other_lockers_and_keeps_file() {
        let dir = crate::test_dir("page_lock");
        let active = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(3));

        let handles: Vec<_> = (0..3u64)
            .map(|i| {
                let (dir, active, max_seen, start) =
                    (dir.clone(), active.clone(), max_seen.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    // Different addresses in the same page share one lock.
                    let _lock = lock_page_in(&dir, 0x5000_0000 + i * 4).unwrap();
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_seen.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(25));
                    active.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(max_seen.load(Ordering::SeqCst), 1);
        let page = page_size();
        let name = format!("{:016x}-{:016x}.lock", 0x5000_0000u64, 0x5000_0000 + page);
        assert!(dir.join(name).exists());

        // A different page is a different lock: both can be held at once.
        let _a = lock_page_in(&dir, 0x5000_0000).unwrap();
        let _b = lock_page_in(&dir, 0x5000_0000 + page).unwrap();

        let _ = fs::remove_dir_all(&dir);
    }
}

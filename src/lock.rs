//! Inter-process locking, held for one access or read-modify-write. The protocol, which any program
//! can follow to exclude and be excluded by mmreg:
//!
//! - **UIO:** exclusive `flock(2)` on an open file of the `/dev/uioN` device. C programs that lock
//!   their own `/dev/uioN` file descriptor this way take part without any mmreg code.
//! - **`/dev/mem` and custom mappers:** exclusive `flock(2)` on
//!   `<lock dir>/<page base>-<page end>.lock` for the physical page holding the register.
//!
//! `flock` locks belong to an open file, so two files opened in one process exclude each other too.
//! Lock files are never deleted: unlinking one while another process waits on it lets two
//! processes hold "the" lock on different inodes. A lock is released when its file is unlocked or
//! closed, including when the process exits.

use crate::error::MmregError;
use crate::mmio::{lock_exclusive, page_size, unlock};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// An open `/dev/uioN`, locked for every access to that device (UIO lock protocol).
pub(crate) struct DeviceLock(File);

impl DeviceLock {
    /// Opens `device_path` read-only: enough for `flock`.
    pub(crate) fn open(device_path: &str) -> Result<Self, MmregError> {
        File::open(device_path)
            .map(DeviceLock)
            .map_err(|e| MmregError::LockOpen {
                lock_path: PathBuf::from(device_path),
                details: e.to_string(),
            })
    }
}

/// A held lock, released when dropped.
pub(crate) enum LockGuard<'a> {
    Device(&'a File),
    /// Closing the page lock file releases it.
    Page(#[allow(dead_code)] File),
}

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        if let LockGuard::Device(file) = self {
            unlock(file);
        }
    }
}

/// Takes the lock for an access at physical `address`: the device's lock when it has one, the
/// page lock file otherwise.
pub(crate) fn lock(device: Option<&DeviceLock>, address: u64) -> Result<LockGuard<'_>, MmregError> {
    match device {
        Some(DeviceLock(file)) => {
            lock_exclusive(file).map_err(|e| MmregError::LockAcquire {
                details: e.to_string(),
            })?;
            Ok(LockGuard::Device(file))
        }
        None => lock_page_in(lock_dir(), address).map(LockGuard::Page),
    }
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
    lock_exclusive(&file).map_err(|e| MmregError::LockAcquire {
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

/// `/run/lock/mmreg` when `/run/lock` exists, else `/tmp/mmreg-lock` (or `<temp dir>/mmreg-lock`);
/// created world-writable and sticky (`1777`) so root and non-root always use the same directory.
fn lock_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        #[cfg(unix)]
        let dir = {
            let dir = if Path::new("/run/lock").is_dir() {
                "/run/lock/mmreg"
            } else {
                "/tmp/mmreg-lock"
            };
            if fs::create_dir_all(dir).is_ok() {
                use std::os::unix::fs::PermissionsExt;
                // Fails unless we created it; whoever did set the same mode.
                let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o1777));
            }
            PathBuf::from(dir)
        };
        #[cfg(not(unix))]
        let dir = {
            let dir = std::env::temp_dir().join("mmreg-lock");
            let _ = fs::create_dir_all(&dir);
            dir
        };
        dir
    })
}

#[cfg(all(test, unix))]
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

    #[test]
    fn test_device_lock_excludes_other_opens_and_unlocks_on_drop() {
        let dir = crate::test_dir("device_lock");
        let dev = dir.join("uio0");
        fs::write(&dev, b"").unwrap();
        let path = dev.to_str().unwrap().to_string();
        let active = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..3)
            .map(|_| {
                let (path, active, max_seen) = (path.clone(), active.clone(), max_seen.clone());
                std::thread::spawn(move || {
                    let device = DeviceLock::open(&path).unwrap();
                    for _ in 0..3 {
                        let _guard = lock(Some(&device), 0).unwrap();
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        max_seen.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(5));
                        active.fetch_sub(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(max_seen.load(Ordering::SeqCst), 1);
        assert_eq!(
            DeviceLock::open("/nonexistent/uio9").err().unwrap().code(),
            "E_LOCK_OPEN"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}

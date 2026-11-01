//! Inter-process and intra-process locking for register accesses and multi-access sequences.
//!
//! The inter-process protocol, which any program can follow to exclude and be excluded by mmreg:
//! - **UIO:** exclusive `flock(2)` on an open file of the `/dev/uioN` device. C programs that lock
//!   their own `/dev/uioN` file descriptor this way take part without any mmreg code.
//! - **`/dev/mem` and custom mappers:** exclusive `flock(2)` on
//!   `<lock dir>/<page base>-<page end>.lock` for the physical page holding the register.
//!
//! Within a single process, all [`DeviceLock`] handles referring to the same `(st_dev, st_ino)`
//! share one open file descriptor and a reentrant per-thread mutex so that:
//! - two `Device` handles in the same process never deadlock on each other's `flock(2)` descriptors,
//! - two threads sharing one `Device` exclude each other even though they share one file descriptor,
//! - a thread holding a sequence guard (`let _g = dev.lock()?;`) can perform nested reads, writes,
//!   and `update_u32` calls reentrantly without releasing the lock mid-sequence.
//!
//! Lock files are never deleted: unlinking one while another process waits on it lets two
//! processes hold "the" lock on different inodes. A lock is released when its guard is dropped or
//! when the process exits.

use crate::error::MmregError;
use crate::mmio::{lock_exclusive, page_size, unlock};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread::ThreadId;

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct FileKey {
    dev: u64,
    ino: u64,
}

#[cfg(not(unix))]
#[derive(Clone, PartialEq, Eq)]
struct FileKey(PathBuf);

#[cfg(unix)]
fn file_key(file: &File, _path: &Path) -> io::Result<FileKey> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata()?;
    Ok(FileKey {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

#[cfg(not(unix))]
fn file_key(_file: &File, path: &Path) -> io::Result<FileKey> {
    Ok(FileKey(
        fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
    ))
}

struct OwnerState {
    owner: Option<ThreadId>,
    depth: usize,
}

pub(crate) struct SharedLock {
    file: File,
    state: Mutex<OwnerState>,
    cvar: Condvar,
}

impl SharedLock {
    fn new(file: File) -> Self {
        SharedLock {
            file,
            state: Mutex::new(OwnerState {
                owner: None,
                depth: 0,
            }),
            cvar: Condvar::new(),
        }
    }

    fn acquire<'a>(self: &Arc<Self>) -> Result<LockGuard<'a>, MmregError> {
        let current = std::thread::current().id();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.owner == Some(current) {
            state.depth += 1;
            return Ok(LockGuard {
                held: HeldLocks::One(Arc::clone(self)),
                _marker: PhantomData,
            });
        }
        while state.owner.is_some() {
            state = self.cvar.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        if let Err(e) = lock_exclusive(&self.file) {
            self.cvar.notify_one();
            return Err(MmregError::LockAcquire {
                details: e.to_string(),
            });
        }
        state.owner = Some(current);
        state.depth = 1;
        Ok(LockGuard {
            held: HeldLocks::One(Arc::clone(self)),
            _marker: PhantomData,
        })
    }

    fn release(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.depth -= 1;
        if state.depth == 0 {
            unlock(&self.file);
            state.owner = None;
            self.cvar.notify_one();
        }
    }

    fn is_owned_by(&self, thread: ThreadId) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.owner == Some(thread) && state.depth > 0
    }
}

type LockTable = Vec<(FileKey, Weak<SharedLock>)>;

/// Maps an I/O error on lock file `path` to `MmregError::LockOpen`.
fn lock_open_error(path: &Path) -> impl Fn(io::Error) -> MmregError + '_ {
    move |e| MmregError::LockOpen {
        lock_path: path.to_path_buf(),
        details: e.to_string(),
    }
}

/// Returns the registry's lock for `key`, creating it from `make_file()` (called only when absent).
fn insert_or_get_shared_lock(
    key: FileKey,
    make_file: impl FnOnce() -> io::Result<File>,
) -> io::Result<Arc<SharedLock>> {
    static REGISTRY: OnceLock<Mutex<LockTable>> = OnceLock::new();
    let mutex = REGISTRY.get_or_init(|| Mutex::new(Vec::new()));
    let mut table = mutex.lock().unwrap_or_else(|e| e.into_inner());

    table.retain(|(_, weak)| weak.strong_count() > 0);
    if let Some((_, weak)) = table.iter().find(|(k, _)| *k == key) {
        if let Some(existing) = weak.upgrade() {
            return Ok(existing);
        }
    }

    let shared = Arc::new(SharedLock::new(make_file()?));
    table.push((key, Arc::downgrade(&shared)));
    Ok(shared)
}

/// A shared handle to a device or page lock file in the per-process registry.
#[derive(Clone)]
pub(crate) struct DeviceLock(Arc<SharedLock>);

impl DeviceLock {
    /// Reuses an already-open `/dev/uioN` descriptor (`file`) for the per-process lock registry
    /// without a second `open(2)` syscall, cloning the descriptor only if this `(st_dev, st_ino)`
    /// is not yet in the registry.
    pub(crate) fn from_open_file(file: &File, device_path: &str) -> Result<Self, MmregError> {
        let path = Path::new(device_path);
        let key = file_key(file, path).map_err(lock_open_error(path))?;
        Self::registered(key, path, || file.try_clone())
    }

    /// Opens `device_path` read-only (enough for `flock`) or reuses the process's open descriptor
    /// for the same `(st_dev, st_ino)`.
    pub(crate) fn open(device_path: &str) -> Result<Self, MmregError> {
        let path = Path::new(device_path);
        let file = File::open(path).map_err(lock_open_error(path))?;
        Self::from_file(file, path)
    }

    /// Registers the freshly opened lock `file`, or drops it for the process's existing descriptor.
    fn from_file(file: File, path: &Path) -> Result<Self, MmregError> {
        let key = file_key(&file, path).map_err(lock_open_error(path))?;
        Self::registered(key, path, || Ok(file))
    }

    fn registered(
        key: FileKey,
        path: &Path,
        make_file: impl FnOnce() -> io::Result<File>,
    ) -> Result<Self, MmregError> {
        insert_or_get_shared_lock(key, make_file)
            .map(DeviceLock)
            .map_err(lock_open_error(path))
    }

    /// Opens or reuses the page lock file for `address` in `dir`.
    pub(crate) fn open_page_in(dir: &Path, address: u64) -> Result<Self, MmregError> {
        let page = page_size();
        let base = address & !(page - 1);
        let path = dir.join(format!(
            "{:016x}-{:016x}.lock",
            base,
            base.wrapping_add(page)
        ));
        let file = open_lock_file(&path).map_err(lock_open_error(&path))?;
        Self::from_file(file, &path)
    }

    /// Opens or reuses the page lock file for `address` in the system lock directory.
    pub(crate) fn open_page(address: u64) -> Result<Self, MmregError> {
        Self::open_page_in(lock_dir(), address)
    }

    /// Acquires the lock (reentrant on the holding thread).
    #[inline]
    pub(crate) fn lock(&self) -> Result<LockGuard<'_>, MmregError> {
        self.0.acquire()
    }
}

enum HeldLocks {
    None,
    One(Arc<SharedLock>),
    Many(Vec<Arc<SharedLock>>),
}

impl HeldLocks {
    fn release_all(&self) {
        match self {
            HeldLocks::None => {}
            HeldLocks::One(lock) => lock.release(),
            HeldLocks::Many(locks) => {
                for lock in locks.iter().rev() {
                    lock.release();
                }
            }
        }
    }
}

/// One or more held device or page locks, released when dropped.
///
/// Returned by [`crate::Device::lock`]. Reentrant on the holding thread: while a `LockGuard` is
/// alive, register reads, writes, `update_u32`, and nested `dev.lock()` calls on the same thread
/// reuse the held locks without unlocking between operations.
pub struct LockGuard<'a> {
    /// Released in reverse order of acquisition; single-lock acquisitions do not allocate.
    held: HeldLocks,
    _marker: PhantomData<(&'a (), *const ())>,
}

impl LockGuard<'_> {
    /// Detaches the guard from the [`crate::Device`] borrow, for layers that hold a lock across
    /// calls (the C ABI's `mmreg_lock` / `mmreg_unlock`). The locks stay held by this thread until
    /// the returned [`OwnedLockGuard`] is dropped.
    pub fn into_owned(mut self) -> OwnedLockGuard {
        OwnedLockGuard {
            held: std::mem::replace(&mut self.held, HeldLocks::None),
            _marker: PhantomData,
        }
    }
}

impl std::fmt::Debug for LockGuard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockGuard").finish_non_exhaustive()
    }
}

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        self.held.release_all();
    }
}

/// A [`LockGuard`] detached from its [`crate::Device`] borrow by [`LockGuard::into_owned`],
/// releasing its locks when dropped. Like `LockGuard`, it is not `Send`: the locks belong to the
/// thread that took them.
pub struct OwnedLockGuard {
    held: HeldLocks,
    _marker: PhantomData<*const ()>,
}

impl std::fmt::Debug for OwnedLockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedLockGuard").finish_non_exhaustive()
    }
}

impl OwnedLockGuard {
    /// Whether the calling thread holds every lock in this guard.
    pub fn is_owned_by_current_thread(&self) -> bool {
        let current = std::thread::current().id();
        match &self.held {
            HeldLocks::None => true,
            HeldLocks::One(lock) => lock.is_owned_by(current),
            HeldLocks::Many(locks) => locks.iter().all(|l| l.is_owned_by(current)),
        }
    }
}

impl Drop for OwnedLockGuard {
    fn drop(&mut self) {
        self.held.release_all();
    }
}

/// Takes every lock in `locks`, in the order given, as one guard. Callers pass page locks in
/// ascending address order, so two sequences over overlapping windows cannot deadlock. On an
/// error the locks already taken are released.
pub(crate) fn lock_all<'a>(
    locks: impl IntoIterator<Item = DeviceLock>,
) -> Result<LockGuard<'a>, MmregError> {
    let mut all = LockGuard {
        held: HeldLocks::None,
        _marker: PhantomData,
    };
    for lock in locks {
        let mut guard = lock.0.acquire()?;
        if let HeldLocks::One(acquired) = std::mem::replace(&mut guard.held, HeldLocks::None) {
            all.held = match std::mem::replace(&mut all.held, HeldLocks::None) {
                HeldLocks::None => HeldLocks::One(acquired),
                HeldLocks::One(first) => HeldLocks::Many(vec![first, acquired]),
                HeldLocks::Many(mut vec) => {
                    vec.push(acquired);
                    HeldLocks::Many(vec)
                }
            };
        }
    }
    Ok(all)
}

/// Takes the lock for an access at physical `address`: the device's lock when it has one, the
/// page lock file otherwise.
pub(crate) fn lock(device: Option<&DeviceLock>, address: u64) -> Result<LockGuard<'_>, MmregError> {
    match device {
        Some(dev_lock) => dev_lock.lock(),
        None => lock_page_in(lock_dir(), address),
    }
}

pub(crate) fn lock_page_in(dir: &Path, address: u64) -> Result<LockGuard<'static>, MmregError> {
    DeviceLock::open_page_in(dir, address)?.0.acquire()
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

    #[test]
    fn test_reentrant_sequence_guard_and_two_handles_same_process() {
        let dir = crate::test_dir("reentrant_lock");
        let dev = dir.join("uio0");
        fs::write(&dev, b"").unwrap();
        let path = dev.to_str().unwrap().to_string();

        // Two independent DeviceLock::open calls in one process share the same SharedLock:
        // acquiring both on the same thread is reentrant instead of self-deadlocking on flock(2).
        let handle_a = DeviceLock::open(&path).unwrap();
        let handle_b = DeviceLock::open(&path).unwrap();

        let g1 = handle_a.lock().unwrap();
        assert_eq!(format!("{:?}", g1), "LockGuard { .. }");
        let g2 = handle_b.lock().unwrap();

        let step = Arc::new(AtomicUsize::new(0));
        let step_bg = Arc::clone(&step);
        let handle_bg = handle_a.clone();
        let bg = std::thread::spawn(move || {
            step_bg.store(1, Ordering::SeqCst);
            let _g = handle_bg.lock().unwrap();
            step_bg.store(2, Ordering::SeqCst);
        });

        while step.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        std::thread::sleep(Duration::from_millis(15));
        // Dropping the inner guard keeps the outer guard locked.
        drop(g2);
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(step.load(Ordering::SeqCst), 1);

        // Dropping the outer guard releases the lock and unblocks the waiting thread.
        drop(g1);
        bg.join().unwrap();
        assert_eq!(step.load(Ordering::SeqCst), 2);

        let _ = fs::remove_dir_all(&dir);
    }

    /// Child half of the inter-process tests: when spawned by [`assert_child_excluded`], takes the
    /// lock on `MMREG_TEST_CHILD_LOCK_FILE` and returns `true` so the test returns at once.
    #[cfg(unix)]
    fn run_as_lock_child() -> bool {
        let Ok(lock_path) = std::env::var("MMREG_TEST_CHILD_LOCK_FILE") else {
            return false;
        };
        let dev_lock = DeviceLock::open(&lock_path).unwrap();
        let _g = dev_lock.lock().unwrap();
        true
    }

    /// Re-runs `test_name` as a child process locking `path`, asserts that it blocks while `guard`
    /// is held, then releases `guard` and asserts that the child finishes.
    #[cfg(unix)]
    fn assert_child_excluded(path: &str, test_name: &str, guard: LockGuard<'_>) {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .env("MMREG_TEST_CHILD_LOCK_FILE", path)
            .arg("--exact")
            .arg(test_name)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();

        std::thread::sleep(Duration::from_millis(40));
        assert!(
            child.try_wait().unwrap().is_none(),
            "child process must block while parent holds flock"
        );

        drop(guard);
        assert!(child.wait().unwrap().success());
    }

    #[cfg(unix)]
    #[test]
    fn test_interprocess_flock_excludes_child_process() {
        if run_as_lock_child() {
            return;
        }
        let dir = crate::test_dir("interprocess_lock");
        let dev = dir.join("uio0");
        fs::write(&dev, b"").unwrap();
        let path = dev.to_str().unwrap().to_string();

        let parent_lock = DeviceLock::open(&path).unwrap();
        assert_child_excluded(
            &path,
            "lock::tests::test_interprocess_flock_excludes_child_process",
            parent_lock.lock().unwrap(),
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn test_from_open_file_shares_registry_and_outlives_source_descriptor() {
        if run_as_lock_child() {
            return;
        }
        let dir = crate::test_dir("from_open_file");
        let dev = dir.join("uio1");
        fs::write(&dev, b"").unwrap();
        let path = dev.to_str().unwrap().to_string();

        // Stands in for the mapper's open `/dev/uioN` descriptor.
        let source = File::open(&dev).unwrap();
        let from_fd = DeviceLock::from_open_file(&source, &path).unwrap();
        let reopened = DeviceLock::open(&path).unwrap();
        assert!(
            Arc::ptr_eq(&from_fd.0, &reopened.0),
            "the same inode must share one registry entry"
        );
        drop(reopened);

        // The mapper closes its descriptor on unmap; the lock's duplicate must keep excluding
        // other processes.
        drop(source);
        assert_child_excluded(
            &path,
            "lock::tests::test_from_open_file_shares_registry_and_outlives_source_descriptor",
            from_fd.lock().unwrap(),
        );

        let _ = fs::remove_dir_all(&dir);
    }
}

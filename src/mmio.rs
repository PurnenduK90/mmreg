//! The only module with `unsafe`: OS calls (page size, `mmap(2)` of a device file, `flock(2)`,
//! UIO interrupt `poll(2)`/`read(2)`/`write(2)`) and volatile 8/16/32/64-bit access.

use crate::memregs::Width;
use std::fs::{File, OpenOptions};
use std::io;
use std::time::Duration;

/// Returns the system page size in bytes.
#[cfg(unix)]
pub(crate) fn page_size() -> u64 {
    // sysconf(_SC_PAGESIZE) is answered from the auxiliary vector: no syscall, nothing to cache.
    match unsafe { libc::sysconf(libc::_SC_PAGESIZE) } {
        n if n > 0 => n as u64,
        _ => 4096,
    }
}

#[cfg(not(unix))]
pub(crate) fn page_size() -> u64 {
    4096
}

/// Opens a device file; `read_only` opens with `O_RDONLY`, otherwise `O_RDWR`, and `sync` adds
/// `O_SYNC` so `/dev/mem` maps the range uncached even where it is backed by RAM.
pub(crate) fn open_device(path: &str, sync: bool, read_only: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(!read_only);
    #[cfg(unix)]
    if sync {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_SYNC);
    }
    #[cfg(not(unix))]
    let _ = sync;
    options.open(path)
}

/// Takes an exclusive `flock(2)` on `file`, waiting for it; released when the file is closed.
#[cfg(unix)]
pub(crate) fn lock_exclusive(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Releases a `flock(2)` taken with [`lock_exclusive`], keeping the file open.
#[cfg(unix)]
pub(crate) fn unlock(file: &File) {
    use std::os::unix::io::AsRawFd;
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
}

/// No-op: hardware access is Unix-only, so there is nothing to serialize.
#[cfg(not(unix))]
pub(crate) fn lock_exclusive(_: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn unlock(_: &File) {}

/// Writes `1u32` (native-endian, 4 bytes) to a `/dev/uioN` descriptor to unmask its interrupt.
#[cfg(unix)]
pub(crate) fn irq_unmask(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let bytes = 1u32.to_ne_bytes();
    let fd = file.as_raw_fd();
    loop {
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written == 4 {
            return Ok(());
        }
        if written < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        return Err(io::Error::from(io::ErrorKind::WriteZero));
    }
}

#[cfg(not(unix))]
pub(crate) fn irq_unmask(_: &File) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// Waits up to `timeout` for a `/dev/uioN` interrupt with `poll(POLLIN)` and reads the 4-byte
/// kernel interrupt counter, returning `Ok(None)` on timeout.
#[cfg(unix)]
pub(crate) fn irq_wait(file: &File, timeout: Duration) -> io::Result<Option<u32>> {
    use std::os::unix::io::AsRawFd;
    use std::time::Instant;

    let fd = file.as_raw_fd();
    let deadline = Instant::now().checked_add(timeout);
    loop {
        let remaining_ms = match deadline {
            Some(end) => {
                let rem = end.saturating_duration_since(Instant::now());
                i32::try_from(rem.as_millis()).unwrap_or(i32::MAX)
            }
            None => i32::MAX,
        };
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut pfd, 1, remaining_ms) };
        if ready == 0 {
            return Ok(None);
        }
        if ready < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if pfd.revents & libc::POLLIN == 0 {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        let mut buf = [0u8; 4];
        loop {
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n == 4 {
                return Ok(Some(u32::from_ne_bytes(buf)));
            }
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn irq_wait(_: &File, _: Duration) -> io::Result<Option<u32>> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

// 32-bit glibc and Android have a 32-bit `off_t`: physical addresses at or above 2 GiB need `mmap64`.
#[cfg(all(
    unix,
    not(any(all(target_os = "linux", target_env = "gnu"), target_os = "android"))
))]
use libc::{mmap, off_t};
#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "android"))]
use libc::{mmap64 as mmap, off64_t as off_t};

/// A shared mapping of a device file, unmapped on drop.
pub(crate) struct Mapping {
    ptr: *mut u8,
    len: usize,
    /// Bytes from `ptr` to the first byte the caller asked for.
    data_offset: usize,
}

// Safety: a `Mapping` is the sole owner of its pages; callers serialize access through `Device`.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    /// Maps `data_offset + size` bytes of `file` from `file_offset` (a multiple of the page size).
    #[cfg(unix)]
    pub(crate) fn new(
        file: &File,
        file_offset: u64,
        data_offset: usize,
        size: usize,
        read_only: bool,
    ) -> io::Result<Self> {
        use std::os::unix::io::AsRawFd;
        let invalid = || io::Error::from(io::ErrorKind::InvalidInput);
        if size == 0 {
            return Err(invalid());
        }
        let len = data_offset.checked_add(size).ok_or_else(invalid)?;
        let offset = off_t::try_from(file_offset).map_err(|_| invalid())?;
        let prot = if read_only {
            libc::PROT_READ
        } else {
            libc::PROT_READ | libc::PROT_WRITE
        };
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                prot,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                offset,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Mapping {
            ptr: ptr.cast(),
            len,
            data_offset,
        })
    }

    #[cfg(not(unix))]
    pub(crate) fn new(_: &File, _: u64, _: usize, _: usize, _: bool) -> io::Result<Self> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// Volatile read of `width` bits at `offset` bytes past the requested address.
    #[inline]
    pub(crate) fn read(&self, offset: u32, width: Width) -> u64 {
        let p = self.register(offset, width);
        unsafe {
            match width {
                Width::U8 => u64::from(p.read_volatile()),
                Width::U16 => u64::from(p.cast::<u16>().read_volatile()),
                Width::U32 => u64::from(p.cast::<u32>().read_volatile()),
                Width::U64 => p.cast::<u64>().read_volatile(),
            }
        }
    }

    /// Volatile write of the low `width` bits of `value` at `offset` bytes past the requested
    /// address.
    #[inline]
    pub(crate) fn write(&self, offset: u32, width: Width, value: u64) {
        let p = self.register(offset, width);
        unsafe {
            match width {
                Width::U8 => p.write_volatile(value as u8),
                Width::U16 => p.cast::<u16>().write_volatile(value as u16),
                Width::U32 => p.cast::<u32>().write_volatile(value as u32),
                Width::U64 => p.cast::<u64>().write_volatile(value),
            }
        }
    }

    /// Callers guarantee `offset` is aligned to `width` and the access lies inside the window.
    #[inline]
    fn register(&self, offset: u32, width: Width) -> *mut u8 {
        let at = self.data_offset.saturating_add(offset as usize);
        let bytes = width.bytes();
        assert!(
            at % bytes == 0 && at.checked_add(bytes).is_some_and(|end| end <= self.len),
            "register access out of bounds or unaligned"
        );
        unsafe { self.ptr.add(at) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::munmap(self.ptr.cast(), self.len);
        }
    }
}

#[cfg(all(test, unix))]
pub(crate) fn test_pipe() -> (File, File) {
    use std::os::unix::io::FromRawFd;
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_size_and_volatile_access() {
        let ps = page_size();
        assert!(ps >= 4096 && ps.is_power_of_two());

        let mut backing = [0u64; 4];
        let mapping = std::mem::ManuallyDrop::new(Mapping {
            ptr: backing.as_mut_ptr().cast(),
            len: 32,
            data_offset: 8,
        });
        mapping.write(8, Width::U64, 0x0123_4567_89AB_CDEF);
        assert_eq!(backing[2], 0x0123_4567_89AB_CDEF);
        assert_eq!(mapping.read(8, Width::U64), 0x0123_4567_89AB_CDEF);
        mapping.write(0, Width::U8, 0x1FF);
        mapping.write(2, Width::U16, 0xBEEF);
        assert_eq!(mapping.read(0, Width::U8), 0xFF);
        assert_eq!(mapping.read(2, Width::U16), 0xBEEF);
        mapping.write(4, Width::U32, 0xCAFE_BABE);
        assert_eq!(mapping.read(4, Width::U32), 0xCAFE_BABE);
    }

    #[test]
    fn test_open_device_missing_path_fails() {
        assert!(open_device("/dev/nonexistent_mmreg_device", true, false).is_err());
        assert!(open_device("/dev/nonexistent_mmreg_device", false, true).is_err());
    }

    /// A read-only open (`O_RDONLY` + `PROT_READ`) works on a file with read permission only,
    /// and the read-only descriptor still takes the `flock` of the lock protocol.
    #[cfg(unix)]
    #[test]
    fn test_read_only_open_maps_a_read_permission_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = crate::test_dir("read_only_open");
        let path = dir.join("uio0");
        let page = page_size() as usize;
        let mut content = vec![0u8; page];
        content[4..8].copy_from_slice(&0xCAFE_F00Du32.to_ne_bytes());
        std::fs::write(&path, &content).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let p = path.to_str().unwrap();

        let file = open_device(p, false, true).unwrap();
        let mapping = Mapping::new(&file, 0, 0, page, true).unwrap();
        assert_eq!(mapping.read(4, Width::U32), 0xCAFE_F00D);
        // A writable mapping needs a writable descriptor: the read-only flag is what makes the
        // open work with read permission alone (also as root, who ignores the 0444 mode).
        assert!(Mapping::new(&file, 0, 0, page, false).is_err());
        lock_exclusive(&file).unwrap();
        unlock(&file);

        drop(mapping);
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn test_irq_unmask_and_wait_on_pipe() {
        use std::io::Read;
        use std::os::unix::io::FromRawFd;

        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let mut reader = unsafe { File::from_raw_fd(fds[0]) };
        let writer = unsafe { File::from_raw_fd(fds[1]) };

        // Empty pipe times out cleanly.
        assert_eq!(irq_wait(&reader, Duration::from_millis(10)).unwrap(), None);

        // irq_unmask writes 1u32 in native byte order.
        irq_unmask(&writer).unwrap();
        let mut raw = [0u8; 4];
        reader.read_exact(&mut raw).unwrap();
        assert_eq!(u32::from_ne_bytes(raw), 1);

        // Writing a 4-byte count wakes irq_wait with that count.
        let count_bytes = 42u32.to_ne_bytes();
        assert_eq!(
            unsafe { libc::write(fds[1], count_bytes.as_ptr().cast(), 4) },
            4
        );
        assert_eq!(
            irq_wait(&reader, Duration::from_millis(100)).unwrap(),
            Some(42)
        );
    }
}

//! The only module with `unsafe`: OS calls (page size, `mmap(2)` of a device file, `flock(2)`)
//! and volatile 8/16/32/64-bit access.

use crate::memregs::Width;
use std::fs::{File, OpenOptions};
use std::io;

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

/// Opens a device file for reading and writing; `sync` adds `O_SYNC`, which makes `/dev/mem`
/// map the range uncached even where it is backed by RAM.
pub(crate) fn open_device(path: &str, sync: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
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

// 32-bit glibc and Android have a 32-bit `off_t`: physical addresses at or above 2 GiB need `mmap64`.
#[cfg(all(
    unix,
    not(any(all(target_os = "linux", target_env = "gnu"), target_os = "android"))
))]
use libc::{mmap, off_t};
#[cfg(any(all(target_os = "linux", target_env = "gnu"), target_os = "android"))]
use libc::{mmap64 as mmap, off64_t as off_t};

/// A shared read/write mapping of a device file, unmapped on drop.
pub(crate) struct Mapping {
    ptr: *mut u8,
    len: usize,
    /// Bytes from `ptr` to the first byte the caller asked for.
    data_offset: usize,
}

// Safety: a `Mapping` is the sole owner of its pages; callers serialize access through `Interface`.
unsafe impl Send for Mapping {}

impl Mapping {
    /// Maps `data_offset + size` bytes of `file` from `file_offset` (a multiple of the page size).
    #[cfg(unix)]
    pub(crate) fn new(
        file: &File,
        file_offset: u64,
        data_offset: usize,
        size: usize,
    ) -> io::Result<Self> {
        use std::os::unix::io::AsRawFd;
        let invalid = || io::Error::from(io::ErrorKind::InvalidInput);
        let len = data_offset.checked_add(size).ok_or_else(invalid)?;
        let offset = off_t::try_from(file_offset).map_err(|_| invalid())?;
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
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
    pub(crate) fn new(_: &File, _: u64, _: usize, _: usize) -> io::Result<Self> {
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
        let at = self.data_offset + offset as usize;
        debug_assert!(at % width.bytes() == 0 && at + width.bytes() <= self.len);
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
        assert!(open_device("/dev/nonexistent_mmreg_device", true).is_err());
    }
}

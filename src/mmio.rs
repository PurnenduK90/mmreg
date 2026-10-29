//! The only module that talks to the OS about memory: page size, `mmap(2)` of a device file,
//! and volatile 32-bit access.

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

    /// Volatile 32-bit read at `offset` bytes past the requested address.
    #[inline]
    pub(crate) fn read_u32(&self, offset: u32) -> u32 {
        unsafe { self.register(offset).read_volatile() }
    }

    /// Volatile 32-bit write at `offset` bytes past the requested address.
    #[inline]
    pub(crate) fn write_u32(&self, offset: u32, value: u32) {
        unsafe { self.register(offset).write_volatile(value) }
    }

    /// `Interface` guarantees `offset % 4 == 0` and `offset + 4 <= size` before any access.
    #[inline]
    fn register(&self, offset: u32) -> *mut u32 {
        debug_assert!(self.data_offset + offset as usize + 4 <= self.len);
        unsafe { self.ptr.add(self.data_offset + offset as usize) }.cast()
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

        let mut backing = [0u32; 8];
        let mapping = std::mem::ManuallyDrop::new(Mapping {
            ptr: backing.as_mut_ptr().cast(),
            len: 32,
            data_offset: 4,
        });
        mapping.write_u32(8, 0xCAFE_BABE);
        assert_eq!(mapping.read_u32(8), 0xCAFE_BABE);
        assert_eq!(backing[3], 0xCAFE_BABE);
    }

    #[test]
    fn test_open_device_missing_path_fails() {
        assert!(open_device("/dev/nonexistent_mmreg_device", true).is_err());
    }
}

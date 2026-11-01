//! # mmreg Library API
//!
//! Safe, concurrent access to 8/16/32/64-bit memory-mapped IO registers via UIO (`/dev/uioN`) or
//! `/dev/mem`, backed by blueprint address validation, Hardware Pre-Flight checks, reentrant
//! sequence locking, and UIO interrupt control.
//!
//! ## Features
//! - `profile` (default): load [`SafetyProfile`]s from JSON files.
//! - `debug` (off): `/dev/mem` access for addresses no UIO region covers, for debug and bring-up
//!   builds. Without it such addresses fail with `[E_DEBUG_DISABLED]` before anything is opened, so
//!   release builds cannot reach arbitrary physical memory.
//!
//! ## Example Usage
//!
//! ```no_run
//! use mmreg::{Device, MmregError};
//!
//! // Open a UIO device by its /sys/class/uio/uio*/name
//! let dev = Device::open_uio("axi_gpio")?;
//! let status = dev.read_u32(0x04)?;
//!
//! // Hold a reentrant sequence lock across multiple register accesses
//! {
//!     let _guard = dev.lock()?;
//!     dev.update_u32(0x00, 0x0000_000F, 0x0000_0005)?;
//!     dev.write_u32(0x08, status)?;
//! }
//! # Ok::<(), MmregError>(())
//! ```

mod device;
mod error;
mod interface;
mod lock;
mod memcheck;
mod memregs;
mod mmio;
pub mod parse;
mod register;
mod validation;

pub use device::{uio, Device, DeviceBuilder};
pub use error::MmregError;
pub use interface::Interface;
pub use lock::{LockGuard, OwnedLockGuard};
pub use memregs::{MapperType, MemoryMapper, Width};
pub use register::{Register, SubRegister};
pub use validation::{SafetyProfile, ValidationCheck};

/// Reads 32 bits at a 4-byte-aligned physical address (opened read-only), through UIO when a
/// region covers it, `/dev/mem` otherwise.
///
/// `force` bypasses blueprint and pre-flight validation (alignment is still checked).
///
/// # Example
/// ```no_run
/// let value = mmreg::read_register_at(0x4000_0000, false)?;
/// println!("Value: 0x{:08X}", value);
/// # Ok::<(), mmreg::MmregError>(())
/// ```
pub fn read_register_at(address: u64, force: bool) -> Result<u32, MmregError> {
    if address % 4 != 0 {
        return Err(MmregError::UnalignedAccess { address, offset: 0 });
    }
    Device::addr(address, 4)
        .read_only()
        .with_force(force)
        .open()?
        .read_u32(0)
}

/// Writes 32 bits at a 4-byte-aligned physical address, through UIO when a region covers it,
/// `/dev/mem` otherwise.
///
/// `force` bypasses blueprint and pre-flight validation (alignment is still checked).
///
/// # Example
/// ```no_run
/// mmreg::write_register_at(0x4000_0000, 0xDEADBEEF, false)?;
/// # Ok::<(), mmreg::MmregError>(())
/// ```
pub fn write_register_at(address: u64, value: u32, force: bool) -> Result<(), MmregError> {
    if address % 4 != 0 {
        return Err(MmregError::UnalignedAccess { address, offset: 0 });
    }
    Device::addr(address, 4)
        .with_force(force)
        .open()?
        .write_u32(0, value)
}

/// Creates an empty, uniquely named directory under the system temp dir.
#[cfg(test)]
pub(crate) fn test_dir(prefix: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mmreg_{}_{}_{}",
        prefix,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Shared byte-addressable mock [`MemoryMapper`] used across unit tests in `device` and `interface`.
#[cfg(test)]
pub(crate) struct TestMockMapper {
    bytes: std::sync::Mutex<Vec<u8>>,
    mapped: bool,
}

#[cfg(test)]
impl TestMockMapper {
    pub(crate) fn boxed(size: usize) -> Box<Self> {
        Box::new(Self {
            bytes: std::sync::Mutex::new(vec![0; size]),
            mapped: false,
        })
    }
}

#[cfg(test)]
impl MemoryMapper for TestMockMapper {
    fn map(&mut self, _address: u64, _size: usize) -> Result<(), MmregError> {
        self.mapped = true;
        Ok(())
    }

    fn unmap(&mut self) {
        self.mapped = false;
    }

    fn is_mapped(&self) -> bool {
        self.mapped
    }

    fn read(&self, offset: u32, width: Width) -> u64 {
        let at = offset as usize;
        let bytes = self.bytes.lock().unwrap();
        match width {
            Width::U8 => u64::from(bytes[at]),
            Width::U16 => u64::from(u16::from_ne_bytes(bytes[at..at + 2].try_into().unwrap())),
            Width::U32 => u64::from(u32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap())),
            Width::U64 => u64::from_ne_bytes(bytes[at..at + 8].try_into().unwrap()),
        }
    }

    fn write(&self, offset: u32, width: Width, value: u64) {
        let at = offset as usize;
        let mut slice = self.bytes.lock().unwrap();
        match width {
            Width::U8 => slice[at] = value as u8,
            Width::U16 => slice[at..at + 2].copy_from_slice(&(value as u16).to_ne_bytes()),
            Width::U32 => slice[at..at + 4].copy_from_slice(&(value as u32).to_ne_bytes()),
            Width::U64 => slice[at..at + 8].copy_from_slice(&value.to_ne_bytes()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_and_write_register_at_validation_and_alignment_error() {
        assert!(read_register_at(0xDEAD_BEEF_0000, false).is_err());
        assert!(write_register_at(0xDEAD_BEEF_0000, 0x1234_5678, false).is_err());
        assert_eq!(
            read_register_at(0x4000_0002, true).unwrap_err().code(),
            "E_UNALIGNED_ACCESS"
        );
        assert_eq!(
            write_register_at(0x4000_0001, 0x1, true)
                .unwrap_err()
                .code(),
            "E_UNALIGNED_ACCESS"
        );
    }
}

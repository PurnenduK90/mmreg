//! # mmreg Library API
//!
//! Safe, concurrent access to 32-bit memory-mapped IO registers via UIO (`/dev/uioN`) or
//! `/dev/mem`, backed by blueprint address validation and Hardware Pre-Flight checks.
//!
//! ## Features
//! - `profile` (default): load `SafetyProfile`s from TOML files. Without it the crate depends on
//!   `libc` only; profiles can still be built in code.
//!
//! ## Example Usage
//!
//! ```no_run
//! use mmreg::{MmregError, read_register_at, write_register_at};
//!
//! // Read a 32-bit value from a physical address (with pre-flight validation)
//! let value = read_register_at(0x4000_0000, false)?;
//!
//! // Write a 32-bit value to a physical address
//! write_register_at(0x4000_0000, 0xDEADBEEF, false)?;
//! # Ok::<(), MmregError>(())
//! ```

mod error;
mod interface;
mod lock;
mod memcheck;
mod memregs;
mod mmio;
pub mod parse;
mod register;
mod validation;

pub use error::MmregError;
pub use interface::Interface;
pub use memregs::{MapperType, MemoryMapper};
pub use register::{Register, SubRegister};
pub use validation::{SafetyProfile, ValidationCheck};

fn single_register(address: u64, force: bool) -> Interface {
    Interface::new("mmreg", address, 4, vec![Register::new("reg", 0, vec![])]).with_force(force)
}

/// Reads 32 bits at a 4-byte-aligned physical address, through UIO when a region covers it,
/// `/dev/mem` otherwise.
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
    single_register(address, force).read_register("reg")
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
    single_register(address, force).write_register("reg", value)
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

//! # mmio Library API
//!
//! This crate provides safe, concurrent access to 32-bit memory-mapped IO registers via /dev/mem.
//! It is designed for use in CLI tools, controllers, and logging utilities that need direct register access.
//!
//! ## Example Usage
//!
//! ```rust
//! use mmreg::{read_register_at, write_register_at};
//!
//! // Read a 32-bit value from a physical address
//! let value = read_register_at(0x4000_0000)?;
//!
//! // Write a 32-bit value to a physical address
//! write_register_at(0x4000_0000, 0xDEADBEEF)?;
//! ```

mod interface;
mod register;
mod memregs;

/// The main interface for managing mapped registers and safe access.
///
/// Create an `Interface` to manage a group of registers, handle mapping/unmapping, and provide safe concurrent access.
pub use interface::Interface;

/// Represents a 32-bit register with optional subregisters (bitfields).
///
/// Use `Register::new` to construct, and access via `Interface` methods.
pub use register::{Register, SubRegister};

/// Reads a 32-bit value from a physical address using /dev/mem.
///
/// # Arguments
/// * `address` - The physical address to read from.
///
/// # Returns
/// * `Ok(u32)` - The value read from the address.
/// * `Err(String)` - Error message if mapping or reading fails.
///
/// # Example
/// ```rust
/// let value = mmreg::read_register_at(0x4000_0000)?;
/// println!("Value: 0x{:08X}", value);
/// ```
pub fn read_register_at(address: u64) -> Result<u32, String> {
	let mut interface = crate::Interface::new(
		"devmem",
		address,
		4,
		vec![crate::Register::new("reg", 0, vec![])]
	);
	interface.read_register("reg")
}

/// Writes a 32-bit value to a physical address using /dev/mem.
///
/// # Arguments
/// * `address` - The physical address to write to.
/// * `value` - The 32-bit value to write.
///
/// # Returns
/// * `Ok(())` - On success.
/// * `Err(String)` - Error message if mapping or writing fails.
///
/// # Example
/// ```rust
/// mmreg::write_register_at(0x4000_0000, 0xDEADBEEF)?;
/// println!("Wrote value.");
/// ```
pub fn write_register_at(address: u64, value: u32) -> Result<(), String> {
	let mut interface = crate::Interface::new(
		"devmem",
		address,
		4,
		vec![crate::Register::new("reg", 0, vec![])]
	);
	interface.write_register("reg", value)
}


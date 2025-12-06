use crate::memregs::{mmap_register, munmap_register};
use crate::register::Register;
use std::fs::{File, OpenOptions};

const LOCK_FILE_PATH: &str = "/tmp/mmreg.lock";

pub struct Interface {
    /// Name of the interface (for identification)
    pub name: String,
    /// Physical base address of the mapped region
    pub base_address: u64,
    /// Size of the mapped region in bytes
    pub size: usize,
    /// List of registers managed by this interface
    pub registers: Vec<Register>,
    /// Pointer to mapped memory (if mapped)
    pub map_ptr: Option<*mut u8>,
    /// File handle for /dev/mem (if mapped)
    pub file: Option<File>,
    /// Offset from base address to mapped region
    pub offset: isize,
    /// File handle for lock file (if locked)
    pub lock_file: Option<File>,
}

impl Interface {
        
    /// Create a new Interface for a set of registers at a given base address.
    ///
    /// # Example
    /// ```rust
    /// use mmreg::{Interface, Register};
    /// let iface = Interface::new("mydev", 0x4000_0000, 0x1000, vec![Register::new("reg", 0, vec![])]);
    /// ```
    pub fn new(name: &str, base_address: u64, size: usize, registers: Vec<Register>) -> Self {
        Interface {
            name: name.to_string(),
            base_address,
            size,
            registers,
            map_ptr: None,
            file: None,
            offset: 0,
            lock_file: None,
        }
    }
    /// Returns true if the interface is currently mapped and locked.
    ///
    /// # Example
    /// ```rust
    /// if iface.is_mapped() { /* ... */ }
    /// ```
    pub fn is_mapped(&self) -> bool {
        self.map_ptr.is_some() && self.lock_file.is_some()
    }

    /// Maps the interface's memory region and acquires a lock for safe access.
    ///
    /// # Returns
    /// * `Ok(())` on success
    /// * `Err(String)` if mapping or locking fails
    ///
    /// # Example
    /// ```rust
    /// iface.map()?;
    /// ```
    pub fn map(&mut self) -> Result<(), String> {
        // Acquire lock
        let lock_file = match OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(LOCK_FILE_PATH)
        {
            Ok(f) => f,
            Err(e) => {
                return Err(format!(
                    "Failed to open lock file ({}): {}",
                    LOCK_FILE_PATH, e
                ))
            }
        };
        match fs2::FileExt::lock_exclusive(&lock_file) {
            Ok(_) => {}
            Err(e) => return Err(format!("Failed to acquire exclusive lock: {}", e)),
        }
        self.lock_file = Some(lock_file);

        // Map memory
        match mmap_register(self.base_address, self.size) {
            Ok((file, map_ptr, offset)) => {
                self.map_ptr = Some(map_ptr);
                self.file = Some(file);
                self.offset = offset;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Unmaps the interface's memory region and releases the lock.
    ///
    /// # Example
    /// ```rust
    /// iface.unmap();
    /// ```
    pub fn unmap(&mut self) {
        if let Some(ptr) = self.map_ptr {
            munmap_register(ptr, self.size);
            self.map_ptr = None;
            self.file = None;
        }
        // Release lock
        if let Some(lock_file) = self.lock_file.take() {
            if let Err(e) = fs2::FileExt::unlock(&lock_file) {
                eprintln!("Warning: Failed to release lock: {}", e);
            }
        }
    }

    /// Cleans up resources on drop (unmaps and unlocks).
    pub fn drop(&mut self) {
        self.unmap();
    }

    /// Get a mutable reference to a register by name
    fn get_register_mut(&mut self, name: &str) -> Option<&mut Register> {
        self.registers.iter_mut().find(|reg| reg.name == name)
    }

    /// Writes a 32-bit value to a register by name.
    /// Automatically maps/unmaps if needed.
    ///
    /// # Arguments
    /// * `name` - Register name
    /// * `value` - Value to write
    ///
    /// # Example
    /// ```rust
    /// iface.write_register("reg", 0xDEADBEEF)?;
    /// ```
    pub fn write_register(&mut self, name: &str, value: u32) -> Result<(), String> {
        let mut did_map = false;
        if !self.is_mapped() {
            self.map()?;
            did_map = true;
        }
        let ptr = self.map_ptr;
        let offset = self.offset;
        let result = match (self.get_register_mut(name), ptr) {
            (Some(reg), Some(ptr)) => reg.write(ptr, offset, value),
            (None, _) => Err(format!("Register '{}' not found", name)),
            (_, None) => Err("Mapped pointer not available".to_string()),
        };
        if did_map {
            self.unmap();
        }
        result
    }

    /// Reads a 32-bit value from a register by name.
    /// Automatically maps/unmaps if needed.
    ///
    /// # Arguments
    /// * `name` - Register name
    ///
    /// # Returns
    /// * `Ok(u32)` - Value read
    /// * `Err(String)` - Error message
    ///
    /// # Example
    /// ```rust
    /// let val = iface.read_register("reg")?;
    /// ```
    pub fn read_register(&mut self, name: &str) -> Result<u32, String> {
        let mut did_map = false;
        if !self.is_mapped() {
            self.map()?;
            did_map = true;
        }
        let ptr = self.map_ptr;
        let offset = self.offset;
        let result = match (self.get_register_mut(name), ptr) {
            (Some(reg), Some(ptr)) => reg.read(ptr, offset),
            (None, _) => Err(format!("Register '{}' not found", name)),
            (_, None) => Err("Mapped pointer not available".to_string()),
        };
        if did_map {
            self.unmap();
        }
        result
    }

    /// Reads a subregister (bitfield) value by register and subregister name.
    /// Automatically maps/unmaps and refreshes from memory.
    ///
    /// # Arguments
    /// * `reg_name` - Register name
    /// * `sub_name` - Subregister name
    ///
    /// # Returns
    /// * `Ok(u32)` - Value read
    /// * `Err(String)` - Error message
    ///
    /// # Example
    /// ```rust
    /// let bits = iface.read_subregister("reg", "status")?;
    /// ```
    pub fn read_subregister(&mut self, reg_name: &str, sub_name: &str) -> Result<u32, String> {
        let mut did_map = false;
        if !self.is_mapped() {
            self.map()?;
            did_map = true;
        }
        let ptr = self.map_ptr;
        let offset = self.offset;
        let result = match (self.get_register_mut(reg_name), ptr) {
            (Some(reg), Some(ptr)) => {
                // Find subregister, clone it to avoid borrow conflict
                let sub = match reg.get_subregister(sub_name) {
                    Some(sub) => sub.clone(),
                    None => return Err(format!("SubRegister '{}' not found in Register '{}'", sub_name, reg_name)),
                };
                reg.read_subregister(&sub, true, Some(ptr), offset)
            },
            (None, _) => Err(format!("Register '{}' not found", reg_name)),
            (_, None) => Err("Mapped pointer not available".to_string()),
        };
        if did_map {
            self.unmap();
        }
        result
    }

    /// Writes a value to a subregister (bitfield) by register and subregister name.
    /// Automatically maps/unmaps and refreshes from memory.
    ///
    /// # Arguments
    /// * `reg_name` - Register name
    /// * `sub_name` - Subregister name
    /// * `value` - Value to write
    ///
    /// # Example
    /// ```rust
    /// iface.write_subregister("reg", "status", 0x1)?;
    /// ```
    pub fn write_subregister(&mut self, reg_name: &str, sub_name: &str, value: u32) -> Result<(), String> {
        let mut did_map = false;
        if !self.is_mapped() {
            self.map()?;
            did_map = true;
        }
        let ptr = self.map_ptr;
        let offset = self.offset;
        let result = match (self.get_register_mut(reg_name), ptr) {
            (Some(reg), Some(ptr)) => {
                // Find subregister, clone it to avoid borrow conflict
                let sub = match reg.get_subregister(sub_name) {
                    Some(sub) => sub.clone(),
                    None => return Err(format!("SubRegister '{}' not found in Register '{}'", sub_name, reg_name)),
                };
                reg.write_subregister(&sub, value, true, Some(ptr), offset)
            },
            (None, _) => Err(format!("Register '{}' not found", reg_name)),
            (_, None) => Err("Mapped pointer not available".to_string()),
        };
        if did_map {
            self.unmap();
        }
        result
    }

    /// Parses a subregister (bitfield) value from the local raw value (no refresh, no mapping).
    ///
    /// # Arguments
    /// * `reg_name` - Register name
    /// * `sub_name` - Subregister name
    ///
    /// # Returns
    /// * `Ok(u32)` - Value parsed from local raw
    /// * `Err(String)` - Error message
    ///
    /// # Example
    /// ```rust
    /// let bits = iface.parse_subregister("reg", "status")?;
    /// ```
    pub fn parse_subregister(&self, reg_name: &str, sub_name: &str) -> Result<u32, String> {
        match self.registers.iter().find(|reg| reg.name == reg_name) {
            Some(reg) => {
                match reg.get_subregister(sub_name) {
                    Some(sub) => Ok((reg.raw & sub.mask()) >> sub.lsb),
                    None => Err(format!("SubRegister '{}' not found in Register '{}'", sub_name, reg_name)),
                }
            },
            None => Err(format!("Register '{}' not found", reg_name)),
        }
    }

}
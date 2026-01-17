use crate::devmem::{read_u32_mapped, write_u32_mapped};

/// Represents a 32-bit memory-mapped register.
///
/// Use this struct to define registers for use with an Interface. Registers can have subregisters (bitfields).
///
/// # Example
/// ```rust
/// use mmreg::Register;
/// let reg = Register::new("status", 0x0, vec![]);
/// ```
pub struct Register {
    /// Name of the register
    pub name: String,
    /// Offset from the base address (in bytes)
    pub offset: u32,
    /// Last read or written value of the register
    pub raw: u32,
    /// List of subregisters (bitfields) in this register
    pub children: Vec<SubRegister>,
}

/// Represents a subregister (bitfield) within a 32-bit register.
///
/// Use this struct to define bitfields for a Register. Specify the bit range using msb and lsb.
///
/// # Example
/// ```rust
/// use mmreg::SubRegister;
/// let sub = SubRegister::new("flag", 7, 0);
/// ```
#[derive(Clone)]
pub struct SubRegister {
    /// Name of the subregister (bitfield)
    pub name: String,
    /// Most significant bit of the subregister
    pub msb: u8,
    /// Least significant bit of the subregister
    pub lsb: u8,
}

impl Register {
    /// Creates a new 32-bit register.
    ///
    /// # Arguments
    /// * `name` - Register name
    /// * `offset` - Offset from base address
    /// * `children` - List of subregisters
    ///
    /// # Example
    /// ```rust
    /// let reg = Register::new("status", 0x0, vec![]);
    /// ```
    pub fn new(name: &str, offset: u32, children: Vec<SubRegister>) -> Self {
        Register {
            name: name.to_string(),
            offset,
            raw: 0,
            children,
        }
    }
    // All other methods are private to the crate
    /// Read the value of the register, using mapped memory if available
    pub(crate) fn read(&mut self, map_ptr: *mut u8, iface_offset: isize) -> Result<u32, String> {
        let val = read_u32_mapped(map_ptr, iface_offset, self.offset);
        self.raw = val;
        Ok(val)
    }

    /// Write a value to the register, using mapped memory if available
    pub(crate) fn write(
        &mut self,
        map_ptr: *mut u8,
        iface_offset: isize,
        value: u32,
    ) -> Result<(), String> {
        write_u32_mapped(map_ptr, iface_offset, self.offset, value);
        self.raw = value;
        Ok(())
    }

    /// get a subregister by name
    pub(crate) fn get_subregister(&self, name: &str) -> Option<&SubRegister> {
        self.children.iter().find(|sub| sub.name == name)
    }

    /// Read a subregister value using its mask
    /// If refresh is true, update self.raw from mapped memory before reading
    pub(crate) fn read_subregister(
        &mut self,
        sub: &SubRegister,
        refresh: bool,
        map_ptr: Option<*mut u8>,
        iface_offset: isize,
    ) -> Result<u32, String> {
        if refresh {
            if let Some(ptr) = map_ptr {
                self.read(ptr, iface_offset)?;
            } else {
                return Err("Mapped pointer not available".to_string());
            }
        }
        let mask = sub.mask();
        Ok((self.raw & mask) >> sub.lsb)
    }

    /// Write a value to a subregister and write back to memory
    /// If refresh is true, update self.raw from mapped memory before writing
    pub(crate) fn write_subregister(
        &mut self,
        sub: &SubRegister,
        value: u32,
        refresh: bool,
        map_ptr: Option<*mut u8>,
        iface_offset: isize,
    ) -> Result<(), String> {
        if refresh {
            if let Some(ptr) = map_ptr {
                self.read(ptr, iface_offset)?;
            } else {
                return Err("Mapped pointer not available".to_string());
            }
        }
        let mask = sub.mask();
        self.raw = (self.raw & !mask) | ((value << sub.lsb) & mask);
        if let Some(ptr) = map_ptr {
            self.write(ptr, iface_offset, self.raw)?;
        } else {
            return Err("Mapped pointer not available".to_string());
        }
        Ok(())
    }
}

impl SubRegister {
    /// Creates a new subregister (bitfield).
    ///
    /// # Arguments
    /// * `name` - Subregister name
    /// * `msb` - Most significant bit
    /// * `lsb` - Least significant bit
    ///
    /// # Example
    /// ```rust
    /// let sub = SubRegister::new("flag", 7, 0);
    /// ```
    pub fn new(name: &str, msb: u8, lsb: u8) -> Self {
        SubRegister {
            name: name.to_string(),
            msb,
            lsb,
        }
    }
    // All other methods are private to the crate
    pub fn mask(&self) -> u32 {
        if self.msb < self.lsb || self.msb > 31 {
            0
        } else {
            ((1u32 << (self.msb - self.lsb + 1)) - 1) << self.lsb
        }
    }
}

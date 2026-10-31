use crate::memregs::{MemoryMapper, Width};

/// A 32-bit memory-mapped register with optional subregisters (bitfields).
///
/// # Example
/// ```rust
/// use mmreg::{Register, SubRegister};
/// let reg = Register::new("status", 0x4, vec![SubRegister::new("ready", 0, 0)]);
/// ```
#[non_exhaustive]
pub struct Register {
    /// Name of the register
    pub name: String,
    /// Byte offset from the `Interface` base address (a multiple of 4)
    pub offset: u32,
    /// Last value read from or written to the register
    pub raw: u32,
    /// Subregisters (bitfields) of this register
    pub children: Vec<SubRegister>,
}

/// A bitfield `[msb:lsb]` within a 32-bit register.
///
/// # Example
/// ```rust
/// use mmreg::SubRegister;
/// let mode = SubRegister::new("mode", 7, 4);
/// assert_eq!(mode.mask(), 0xF0);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SubRegister {
    /// Name of the subregister
    pub name: String,
    /// Most significant bit
    pub msb: u8,
    /// Least significant bit
    pub lsb: u8,
}

impl Register {
    /// Creates a register at byte `offset` from its `Interface` base address.
    pub fn new(name: &str, offset: u32, children: Vec<SubRegister>) -> Self {
        Register {
            name: name.to_string(),
            offset,
            raw: 0,
            children,
        }
    }

    /// Reads the register from hardware into `raw`.
    #[inline]
    pub(crate) fn read(&mut self, mapper: &dyn MemoryMapper) -> u32 {
        self.raw = mapper.read(self.offset, Width::U32) as u32;
        self.raw
    }

    /// Writes `value` to hardware and records it in `raw`.
    #[inline]
    pub(crate) fn write(&mut self, mapper: &dyn MemoryMapper, value: u32) {
        mapper.write(self.offset, Width::U32, u64::from(value));
        self.raw = value;
    }

    /// Returns the subregister called `name`.
    pub(crate) fn subregister(&self, name: &str) -> Option<&SubRegister> {
        self.children.iter().find(|sub| sub.name == name)
    }
}

impl SubRegister {
    /// Creates a bitfield covering bits `msb` down to `lsb`.
    pub fn new(name: &str, msb: u8, lsb: u8) -> Self {
        SubRegister {
            name: name.to_string(),
            msb,
            lsb,
        }
    }

    /// Returns the bitmask of `[msb:lsb]`, or 0 when the range is invalid (`msb < lsb` or `msb > 31`).
    #[inline]
    pub fn mask(&self) -> u32 {
        if self.msb < self.lsb || self.msb > 31 {
            return 0;
        }
        (u32::MAX >> (31 - self.msb)) & (u32::MAX << self.lsb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subregister_mask() {
        assert_eq!(SubRegister::new("low", 7, 0).mask(), 0x0000_00FF);
        assert_eq!(SubRegister::new("mid", 15, 12).mask(), 0x0000_F000);
        assert_eq!(SubRegister::new("bit", 31, 31).mask(), 0x8000_0000);
        assert_eq!(SubRegister::new("all", 31, 0).mask(), 0xFFFF_FFFF);
        assert_eq!(SubRegister::new("reversed", 3, 5).mask(), 0);
        assert_eq!(SubRegister::new("too_high", 32, 0).mask(), 0);

        let reg = Register::new("ctrl", 0, vec![SubRegister::new("en", 0, 0)]);
        assert!(reg.subregister("en").is_some());
        assert!(reg.subregister("missing").is_none());
    }
}

use crate::error::MmregError;
use crate::lock::lock_page;
use crate::memcheck::{RuntimePmGuard, resolve};
use crate::memregs::{MapperType, MemoryMapper, OsMapper};
use crate::register::Register;
use crate::validation::SafetyProfile;

/// A peripheral register bank: validated mapping, bounds/alignment enforcement and a per-page
/// inter-process lock around every access.
pub struct Interface {
    name: String,
    base_address: u64,
    size: usize,
    registers: Vec<Register>,
    force_map: bool,
    mapper_type: MapperType,
    safety_profile: Option<SafetyProfile>,
    mapper: Option<Box<dyn MemoryMapper>>,
    custom_mapper: bool,
    pm_guard: Option<RuntimePmGuard>,
}

impl Interface {
    /// Creates an `Interface` for `registers` in `[base_address, base_address + size)`.
    ///
    /// The default `MapperType::Auto` uses the UIO region covering the range when there is one,
    /// `/dev/mem` otherwise.
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
            force_map: false,
            mapper_type: MapperType::Auto,
            safety_profile: None,
            mapper: None,
            custom_mapper: false,
            pm_guard: None,
        }
    }

    /// Returns the name of this `Interface`.
    #[inline]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the physical base address.
    #[inline]
    pub fn base_address(&self) -> u64 {
        self.base_address
    }

    /// Returns the size in bytes of the register window.
    #[inline]
    pub fn size(&self) -> usize {
        self.size
    }

    /// Returns the registers, with the values last read or written in `raw`.
    #[inline]
    pub fn registers(&self) -> &[Register] {
        &self.registers
    }

    /// Returns whether blueprint and pre-flight validation are bypassed.
    #[inline]
    pub fn force_map(&self) -> bool {
        self.force_map
    }

    /// Returns the configured backend preference.
    #[inline]
    pub fn mapper_type(&self) -> &MapperType {
        &self.mapper_type
    }

    /// Returns the attached `SafetyProfile`, if any.
    #[inline]
    pub fn safety_profile(&self) -> Option<&SafetyProfile> {
        self.safety_profile.as_ref()
    }

    /// Bypasses blueprint and hardware pre-flight validation (`--force`). Alignment, bounds and an
    /// attached `SafetyProfile` are still enforced.
    #[inline]
    pub fn with_force(mut self, force: bool) -> Self {
        self.force_map = force;
        self
    }

    /// Selects the backend (`Auto`, `DevMem` or `Uio`).
    #[inline]
    pub fn with_mapper_type(mut self, mapper_type: MapperType) -> Self {
        self.mapper_type = mapper_type;
        self
    }

    /// Attaches a `SafetyProfile` whose checks must pass before every mapping.
    #[inline]
    pub fn with_safety_profile(mut self, profile: SafetyProfile) -> Self {
        self.safety_profile = Some(profile);
        self
    }

    /// Uses `mapper` instead of `/dev/mem` / UIO (for tests or custom drivers).
    #[inline]
    pub fn with_custom_mapper(mut self, mapper: Box<dyn MemoryMapper>) -> Self {
        self.mapper = Some(mapper);
        self.custom_mapper = true;
        self
    }

    /// Returns true while the register window is mapped.
    #[inline]
    pub fn is_mapped(&self) -> bool {
        self.mapper.as_ref().is_some_and(|m| m.is_mapped())
    }

    /// Checks that the base is 4-byte aligned and every register lies aligned inside `size`.
    fn validate_layout(&self) -> Result<(), MmregError> {
        if !self.base_address.is_multiple_of(4) {
            return Err(MmregError::UnalignedAccess {
                address: self.base_address,
                offset: 0,
            });
        }
        for reg in &self.registers {
            if !reg.offset.is_multiple_of(4) {
                return Err(MmregError::UnalignedAccess {
                    address: self.base_address,
                    offset: reg.offset,
                });
            }
            if (reg.offset as usize)
                .checked_add(4)
                .is_none_or(|end| end > self.size)
            {
                return Err(MmregError::OutOfBoundsAccess {
                    register_name: reg.name.clone(),
                    offset: reg.offset,
                    access_size: 4,
                    region_size: self.size,
                });
            }
        }
        Ok(())
    }

    /// Maps the register window (no-op when already mapped) after the layout check, the attached
    /// `SafetyProfile`, and, unless forced, blueprint validation and hardware pre-flight.
    ///
    /// Mapping once and then accessing many registers avoids repeating these checks; each access
    /// still takes its page lock on its own.
    pub fn map(&mut self) -> Result<(), MmregError> {
        if self.is_mapped() {
            return Ok(());
        }
        self.validate_layout()?;
        if let Some(profile) = &self.safety_profile {
            profile.execute_with(|addr| crate::read_register_at(addr, true))?;
        }

        let target = resolve(self.base_address, self.size, !self.force_map)?;
        let kind = match &self.mapper_type {
            MapperType::Auto => target.mapper,
            explicit => explicit.clone(),
        };
        // UIO keeps the device runtime-active while open; /dev/mem needs the guard.
        let pm_guard = match kind {
            MapperType::DevMem => target.owner.as_deref().and_then(RuntimePmGuard::acquire),
            _ => None,
        };

        if !self.custom_mapper {
            self.mapper = None;
        }
        self.mapper
            .get_or_insert_with(|| Box::new(OsMapper::new(kind)))
            .map(self.base_address, self.size)?;
        self.pm_guard = pm_guard;
        Ok(())
    }

    /// Unmaps the register window, then restores the device's runtime PM setting.
    pub fn unmap(&mut self) {
        if let Some(mapper) = &mut self.mapper {
            mapper.unmap();
        }
        self.pm_guard = None;
    }

    fn register_index(&self, name: &str) -> Result<usize, MmregError> {
        self.registers
            .iter()
            .position(|reg| reg.name == name)
            .ok_or_else(|| MmregError::RegisterNotFound {
                interface_name: self.name.clone(),
                register_name: name.to_string(),
                available: self.registers.iter().map(|r| r.name.clone()).collect(),
            })
    }

    /// Resolves a subregister to `(register index, mask, lsb)`.
    fn field(&self, reg_name: &str, sub_name: &str) -> Result<(usize, u32, u8), MmregError> {
        let idx = self.register_index(reg_name)?;
        let reg = &self.registers[idx];
        let sub = reg
            .subregister(sub_name)
            .ok_or_else(|| MmregError::SubRegisterNotFound {
                interface_name: self.name.clone(),
                register_name: reg.name.clone(),
                subregister_name: sub_name.to_string(),
                available: reg.children.iter().map(|s| s.name.clone()).collect(),
            })?;
        match sub.mask() {
            0 => Err(MmregError::InvalidSubRegister {
                register_name: reg.name.clone(),
                subregister_name: sub.name.clone(),
                msb: sub.msb,
                lsb: sub.lsb,
            }),
            mask => Ok((idx, mask, sub.lsb)),
        }
    }

    /// Runs `op` on register `idx` under its page lock, mapping and unmapping around it when the
    /// window is not already mapped.
    fn access<T>(
        &mut self,
        idx: usize,
        op: impl FnOnce(&mut Register, &dyn MemoryMapper) -> T,
    ) -> Result<T, MmregError> {
        let transient = !self.is_mapped();
        self.map()?;
        let reg = &mut self.registers[idx];
        let result = lock_page(self.base_address + u64::from(reg.offset)).map(|_lock| {
            let mapper = self.mapper.as_deref().expect("map() installs a mapper");
            op(reg, mapper)
        });
        if transient {
            self.unmap();
        }
        result
    }

    /// Reads a register.
    pub fn read_register(&mut self, name: &str) -> Result<u32, MmregError> {
        let idx = self.register_index(name)?;
        self.access(idx, |reg, m| reg.read(m))
    }

    /// Writes a register.
    pub fn write_register(&mut self, name: &str, value: u32) -> Result<(), MmregError> {
        let idx = self.register_index(name)?;
        self.access(idx, |reg, m| reg.write(m, value))
    }

    /// Reads a subregister (bitfield) from hardware.
    pub fn read_subregister(&mut self, reg_name: &str, sub_name: &str) -> Result<u32, MmregError> {
        let (idx, mask, lsb) = self.field(reg_name, sub_name)?;
        self.access(idx, |reg, m| (reg.read(m) & mask) >> lsb)
    }

    /// Writes a subregister (bitfield) by read-modify-write under one page lock. Bits of `value`
    /// above the field width are ignored.
    pub fn write_subregister(
        &mut self,
        reg_name: &str,
        sub_name: &str,
        value: u32,
    ) -> Result<(), MmregError> {
        let (idx, mask, lsb) = self.field(reg_name, sub_name)?;
        self.access(idx, |reg, m| {
            let merged = (reg.read(m) & !mask) | ((value << lsb) & mask);
            reg.write(m, merged)
        })
    }

    /// Extracts a subregister from the register's last read or written value (no hardware access).
    pub fn parse_subregister(&self, reg_name: &str, sub_name: &str) -> Result<u32, MmregError> {
        let (idx, mask, lsb) = self.field(reg_name, sub_name)?;
        Ok((self.registers[idx].raw & mask) >> lsb)
    }
}

impl Drop for Interface {
    fn drop(&mut self) {
        self.unmap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::register::SubRegister;
    use std::cell::UnsafeCell;

    struct MockMapper {
        storage: Box<[UnsafeCell<u32>]>,
        mapped: bool,
    }

    // Safety: tests use each mock from one thread.
    unsafe impl Send for MockMapper {}

    impl MockMapper {
        fn boxed(size: usize) -> Box<Self> {
            Box::new(Self {
                storage: (0..size / 4).map(|_| UnsafeCell::new(0)).collect(),
                mapped: false,
            })
        }
    }

    impl MemoryMapper for MockMapper {
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
        fn read_u32(&self, offset: u32) -> u32 {
            unsafe { *self.storage[offset as usize / 4].get() }
        }
        fn write_u32(&self, offset: u32, value: u32) {
            unsafe { *self.storage[offset as usize / 4].get() = value }
        }
    }

    fn mock_iface(name: &str, size: usize, registers: Vec<Register>) -> Interface {
        Interface::new(name, 0x4000_0000, size, registers)
            .with_force(true)
            .with_custom_mapper(MockMapper::boxed(size))
    }

    #[test]
    fn test_read_write_registers_and_subregisters() {
        let mut iface = mock_iface(
            "test_axi",
            16,
            vec![Register::new(
                "ctrl",
                0x0,
                vec![
                    SubRegister::new("enable", 0, 0),
                    SubRegister::new("mode", 7, 4),
                    SubRegister::new("bad", 2, 5),
                ],
            )],
        );
        assert_eq!(iface.name(), "test_axi");
        assert_eq!(iface.base_address(), 0x4000_0000);
        assert_eq!(iface.size(), 16);
        assert_eq!(iface.registers().len(), 1);
        assert!(iface.force_map());
        assert_eq!(iface.mapper_type(), &MapperType::Auto);
        assert!(iface.safety_profile().is_none());

        iface.write_register("ctrl", 0x0000_00A0).unwrap();
        assert!(!iface.is_mapped());
        assert_eq!(iface.read_register("ctrl").unwrap(), 0x0000_00A0);
        assert_eq!(iface.read_subregister("ctrl", "mode").unwrap(), 0xA);
        assert_eq!(iface.read_subregister("ctrl", "enable").unwrap(), 0);

        iface.map().unwrap();
        iface.map().unwrap();
        assert!(iface.is_mapped());
        iface.write_subregister("ctrl", "enable", 0x3).unwrap();
        assert_eq!(iface.read_register("ctrl").unwrap(), 0x0000_00A1);
        assert_eq!(iface.parse_subregister("ctrl", "mode").unwrap(), 0xA);
        iface.unmap();
        assert!(!iface.is_mapped());

        let code = |r: Result<u32, MmregError>| r.unwrap_err().code();
        assert_eq!(code(iface.read_register("missing")), "E_REG_NOT_FOUND");
        assert_eq!(
            code(iface.read_subregister("missing", "enable")),
            "E_REG_NOT_FOUND"
        );
        assert_eq!(
            code(iface.read_subregister("ctrl", "nope")),
            "E_SUBREG_NOT_FOUND"
        );
        assert_eq!(
            code(iface.parse_subregister("ctrl", "nope")),
            "E_SUBREG_NOT_FOUND"
        );
        assert_eq!(
            code(iface.read_subregister("ctrl", "bad")),
            "E_SUBREG_RANGE"
        );
        assert_eq!(
            iface.write_register("missing", 1).unwrap_err().code(),
            "E_REG_NOT_FOUND"
        );
        assert_eq!(
            iface
                .write_subregister("ctrl", "nope", 1)
                .unwrap_err()
                .code(),
            "E_SUBREG_NOT_FOUND"
        );
    }

    #[test]
    fn test_bounds_and_alignment_validation() {
        let reg = |offset| vec![Register::new("reg", offset, vec![])];
        let cases = [
            (0x4000_0002, 16, reg(0), "E_UNALIGNED_ACCESS"),
            (0x4000_0000, 16, reg(2), "E_UNALIGNED_ACCESS"),
            (0x4000_0000, 16, reg(16), "E_OUT_OF_BOUNDS"),
            (0x4000_0000, 2, reg(0), "E_OUT_OF_BOUNDS"),
        ];
        for (base, size, regs, code) in cases {
            let mut iface = Interface::new("bad", base, size, regs)
                .with_force(true)
                .with_custom_mapper(MockMapper::boxed(16));
            assert_eq!(iface.map().unwrap_err().code(), code);
            assert_eq!(iface.read_register("reg").unwrap_err().code(), code);
        }
    }

    #[test]
    fn test_two_interfaces_on_one_page_in_one_process() {
        let mut a = mock_iface("dev1", 16, vec![Register::new("ctrl", 0x0, vec![])]);
        let mut b = mock_iface("dev2", 16, vec![Register::new("status", 0x4, vec![])]);
        a.map().unwrap();
        b.map().unwrap();
        a.write_register("ctrl", 0x1111_2222).unwrap();
        b.write_register("status", 0x3333_4444).unwrap();
        assert_eq!(a.read_register("ctrl").unwrap(), 0x1111_2222);
        assert_eq!(b.read_register("status").unwrap(), 0x3333_4444);
    }

    #[test]
    fn test_failed_map_keeps_custom_mapper() {
        let profile = SafetyProfile {
            name: "always_fails".into(),
            checks: vec![crate::ValidationCheck {
                name: "unaligned".into(),
                addr: 0x2,
                mask: 1,
                expected: 1,
            }],
        };
        let mut iface = mock_iface("dev", 16, vec![Register::new("ctrl", 0, vec![])])
            .with_safety_profile(profile);
        assert_eq!(iface.map().unwrap_err().code(), "E_UNALIGNED_ACCESS");
        iface.safety_profile = None;
        iface.write_register("ctrl", 7).unwrap();
        assert_eq!(iface.read_register("ctrl").unwrap(), 7);
    }

    #[test]
    fn test_devmem_without_permission_fails_cleanly() {
        let mut iface = Interface::new("dev", 0x4000_0000, 16, vec![])
            .with_force(true)
            .with_mapper_type(MapperType::DevMem);
        let code = iface.map().unwrap_err().code();
        assert!(code == "E_DEVMEM_OPEN" || code == "E_DEVMEM_MMAP", "{code}");
        assert!(!iface.is_mapped());
    }
}

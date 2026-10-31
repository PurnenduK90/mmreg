//! [`Device`]: a single mapped register window with `&self` 8/16/32/64-bit access, reentrant
//! sequence locking, and UIO interrupt control.

use crate::error::MmregError;
use crate::lock::{lock, lock_all, DeviceLock, LockGuard};
use crate::memcheck::{find_uio, read_uio_region, resolve, RuntimePmGuard};
use crate::memregs::{MapperType, MemoryMapper, OsMapper, Width};
use crate::mmio::{irq_unmask, irq_wait, page_size};
use crate::validation::SafetyProfile;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Functions for discovering Linux Userspace I/O (`/dev/uioN`) devices.
pub mod uio {
    use crate::error::MmregError;
    use crate::memcheck::find_uio;

    /// Finds the `/dev/uioN` path whose `/sys/class/uio/uioN/name` matches `name` (trimmed).
    ///
    /// Returns [`MmregError::UioNotFound`] (listing available names) when no device matches, or
    /// [`MmregError::UioAmbiguous`] when more than one UIO device has the same name.
    ///
    /// # Example
    /// ```no_run
    /// let dev_path = mmreg::uio::find("axi_gpio")?;
    /// println!("Found UIO device at {dev_path}");
    /// # Ok::<(), mmreg::MmregError>(())
    /// ```
    pub fn find(name: &str) -> Result<String, MmregError> {
        find_uio(name)
    }
}

enum TargetSpec {
    UioName(String),
    UioPath(String),
    Addr { base: u64, size: usize },
}

enum Backend {
    Os(OsMapper),
    Custom(Mutex<Box<dyn MemoryMapper>>),
}

impl Backend {
    #[inline]
    fn read(&self, offset: u32, width: Width) -> u64 {
        match self {
            Backend::Os(m) => m.read(offset, width),
            Backend::Custom(m) => m
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .read(offset, width),
        }
    }

    #[inline]
    fn write(&self, offset: u32, width: Width, value: u64) {
        match self {
            Backend::Os(m) => m.write(offset, width, value),
            Backend::Custom(m) => m
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .write(offset, width, value),
        }
    }

    fn unmap(&mut self) {
        match self {
            Backend::Os(m) => m.unmap(),
            Backend::Custom(m) => m.get_mut().unwrap_or_else(|e| e.into_inner()).unmap(),
        }
    }

    fn take_custom(&mut self) -> Option<Box<dyn MemoryMapper>> {
        self.unmap();
        match std::mem::replace(self, Backend::Os(OsMapper::new(MapperType::Auto, true))) {
            Backend::Custom(m) => Some(m.into_inner().unwrap_or_else(|e| e.into_inner())),
            Backend::Os(_) => None,
        }
    }
}

/// Builder for opening a [`Device`].
///
/// Created via [`Device::uio`], [`Device::uio_path`], or [`Device::addr`].
pub struct DeviceBuilder {
    spec: TargetSpec,
    name_override: Option<String>,
    region_index: usize,
    read_only: bool,
    force_map: bool,
    mapper_type: MapperType,
    safety_profile: Option<SafetyProfile>,
    custom_mapper: Option<Box<dyn MemoryMapper>>,
}

impl DeviceBuilder {
    fn new(spec: TargetSpec) -> Self {
        DeviceBuilder {
            spec,
            name_override: None,
            region_index: 0,
            read_only: false,
            force_map: false,
            mapper_type: MapperType::Auto,
            safety_profile: None,
            custom_mapper: None,
        }
    }

    /// Sets the logical name reported in diagnostics and error messages.
    #[inline]
    pub fn with_name(mut self, name: &str) -> Self {
        self.name_override = Some(name.to_string());
        self
    }

    /// Selects UIO memory region `map<region_index>` (default `0`).
    #[inline]
    pub fn region(mut self, region_index: usize) -> Self {
        self.region_index = region_index;
        self
    }

    /// Opens the device read-only (`O_RDONLY` and `PROT_READ`).
    ///
    /// Read-only tools only need read permission (`0440`/`0640`) on `/dev/uioN`, and any write,
    /// `update_u32`, or `irq_unmask` call on the resulting [`Device`] fails with
    /// [`MmregError::ReadOnly`].
    #[inline]
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// Sets whether the device is opened read-only.
    #[inline]
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Bypasses blueprint and hardware pre-flight validation (`--force`).
    ///
    /// Alignment, bounds, and an attached [`SafetyProfile`] are still enforced.
    #[inline]
    pub fn with_force(mut self, force: bool) -> Self {
        self.force_map = force;
        self
    }

    /// Selects the backend (`Auto`, `DevMem`, or `Uio`) when opening by physical address.
    #[inline]
    pub fn with_mapper_type(mut self, mapper_type: MapperType) -> Self {
        self.mapper_type = mapper_type;
        self
    }

    /// Attaches a [`SafetyProfile`] whose register checks must pass before the window is mapped.
    #[inline]
    pub fn with_safety_profile(mut self, profile: SafetyProfile) -> Self {
        self.safety_profile = Some(profile);
        self
    }

    /// Uses `mapper` instead of `/dev/mem` or UIO (for tests or custom kernel drivers).
    ///
    /// Together with [`DeviceBuilder::with_force`]`(true)`, [`Device::uio`] and
    /// [`Device::uio_path`] open even when the UIO device does not exist (off-target unit tests):
    /// the window is then one page at address 0, locked with that page's lock file. Without
    /// `with_force`, the UIO device must exist and its region gives the window.
    #[inline]
    pub fn with_custom_mapper(mut self, mapper: Box<dyn MemoryMapper>) -> Self {
        self.custom_mapper = Some(mapper);
        self
    }

    /// Validates, maps, and opens the [`Device`].
    pub fn open(self) -> Result<Device, MmregError> {
        if let Some(profile) = &self.safety_profile {
            profile.execute_with(|addr| crate::read_register_at(addr, true))?;
        }

        let has_custom = self.custom_mapper.is_some();
        let (default_name, base_address, size, mapper_type) = match self.spec {
            TargetSpec::UioName(uio_name) => {
                match find_uio(&uio_name)
                    .and_then(|p| read_uio_region(&p, self.region_index).map(|w| (p, w)))
                {
                    Ok((path, win)) => (
                        uio_name,
                        win.addr,
                        win.size as usize,
                        MapperType::Uio {
                            device_path: path,
                            region_index: self.region_index,
                        },
                    ),
                    // Off-target tests: a custom mapper with `with_force(true)` stands in for a
                    // UIO device that does not exist (one page at address 0; see
                    // `with_custom_mapper`).
                    Err(_) if has_custom && self.force_map => (
                        uio_name.clone(),
                        0,
                        page_size() as usize,
                        MapperType::Uio {
                            device_path: uio_name,
                            region_index: self.region_index,
                        },
                    ),
                    Err(e) => return Err(e),
                }
            }
            TargetSpec::UioPath(path) => match read_uio_region(&path, self.region_index) {
                Ok(win) => (
                    path.clone(),
                    win.addr,
                    win.size as usize,
                    MapperType::Uio {
                        device_path: path,
                        region_index: self.region_index,
                    },
                ),
                Err(_) if has_custom && self.force_map => (
                    path.clone(),
                    0,
                    page_size() as usize,
                    MapperType::Uio {
                        device_path: path,
                        region_index: self.region_index,
                    },
                ),
                Err(e) => return Err(e),
            },
            TargetSpec::Addr { base, size } => {
                let label = self
                    .name_override
                    .clone()
                    .unwrap_or_else(|| "mmreg".to_string());
                if size == 0 || base.checked_add(size as u64).is_none() {
                    return Err(MmregError::OutOfBoundsAccess {
                        register_name: label,
                        offset: 0,
                        access_size: 1,
                        region_size: size,
                    });
                }
                (label, base, size, self.mapper_type)
            }
        };
        let name = self.name_override.unwrap_or(default_name);

        let mut target = resolve(base_address, size, &mapper_type, !self.force_map)?;

        let pm_guard = match (&target.mapper, has_custom) {
            (MapperType::DevMem, false) if cfg!(feature = "debug") => target
                .owner
                .as_deref()
                .and_then(|p| RuntimePmGuard::acquire(p, &mut target.warnings)),
            _ => None,
        };
        let uio_device_path = match (&target.mapper, has_custom) {
            (MapperType::Uio { device_path, .. }, false) => Some(device_path.clone()),
            _ => None,
        };

        let mut backend = match self.custom_mapper {
            Some(mut custom) => {
                custom.map(base_address, size)?;
                Backend::Custom(Mutex::new(custom))
            }
            None => {
                let mut os = OsMapper::new(target.mapper, self.read_only);
                os.map(base_address, size)?;
                Backend::Os(os)
            }
        };

        let uio_lock = match &uio_device_path {
            Some(path) => match DeviceLock::open(path) {
                Ok(lock) => Some(lock),
                Err(err) => {
                    backend.unmap();
                    return Err(err);
                }
            },
            None => None,
        };

        Ok(Device {
            name,
            base_address,
            size,
            read_only: self.read_only,
            device_path: uio_device_path,
            warnings: target.warnings,
            backend,
            uio_lock,
            base_page_lock: OnceLock::new(),
            _pm_guard: pm_guard,
        })
    }
}

/// A mapped peripheral register window with `&self` 8/16/32/64-bit access, inter-process and
/// multi-thread locking, and UIO interrupt support.
///
/// `Device` is `Send + Sync`: it can be shared across threads (for example in an `Arc<Device>`)
/// and accessed through `&self`. Blueprint, hardware pre-flight, and safety-profile checks run
/// once when the `Device` is opened.
///
/// # Example
/// ```no_run
/// use mmreg::Device;
///
/// let dev = Device::open_uio("axi_gpio")?;
/// let status = dev.read_u32(0x04)?;
/// dev.update_u32(0x00, 0x0000_000F, 0x0000_0005)?;
/// # Ok::<(), mmreg::MmregError>(())
/// ```
pub struct Device {
    name: String,
    base_address: u64,
    size: usize,
    read_only: bool,
    device_path: Option<String>,
    warnings: Vec<String>,
    backend: Backend,
    uio_lock: Option<DeviceLock>,
    base_page_lock: OnceLock<DeviceLock>,
    _pm_guard: Option<RuntimePmGuard>,
}

impl Device {
    /// Opens region `0` of the UIO device whose `/sys/class/uio/uioN/name` matches `name`.
    pub fn open_uio(name: &str) -> Result<Self, MmregError> {
        Self::uio(name).open()
    }

    /// Opens region `0` of the UIO device at `path` (e.g. `"/dev/uio0"`).
    pub fn open_uio_path(path: &str) -> Result<Self, MmregError> {
        Self::uio_path(path).open()
    }

    /// Opens `[base, base + size)` via a covering UIO region when available, or `/dev/mem`
    /// (requires the `debug` feature).
    pub fn open_addr(base: u64, size: usize) -> Result<Self, MmregError> {
        Self::addr(base, size).open()
    }

    /// Returns a [`DeviceBuilder`] for the UIO device named `name`.
    #[inline]
    pub fn uio(name: &str) -> DeviceBuilder {
        DeviceBuilder::new(TargetSpec::UioName(name.to_string()))
    }

    /// Returns a [`DeviceBuilder`] for the UIO device at `path` (e.g. `"/dev/uio0"`).
    #[inline]
    pub fn uio_path(path: &str) -> DeviceBuilder {
        DeviceBuilder::new(TargetSpec::UioPath(path.to_string()))
    }

    /// Returns a [`DeviceBuilder`] for the physical address window `[base, base + size)`.
    #[inline]
    pub fn addr(base: u64, size: usize) -> DeviceBuilder {
        DeviceBuilder::new(TargetSpec::Addr { base, size })
    }

    /// Logical name of the device window.
    #[inline]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Physical base address of the mapped window.
    #[inline]
    pub fn base_address(&self) -> u64 {
        self.base_address
    }

    /// Size of the mapped window in bytes.
    #[inline]
    pub fn size(&self) -> usize {
        self.size
    }

    /// Returns `true` if the device was opened in read-only mode.
    #[inline]
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Returns the `/dev/uioN` device path when backed by UIO, or `None` for `/dev/mem` and
    /// custom mappers.
    #[inline]
    pub fn device_path(&self) -> Option<&str> {
        self.device_path.as_deref()
    }

    /// Non-fatal diagnostic warnings collected during hardware pre-flight when opening the device.
    #[inline]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub(crate) fn take_custom_mapper(&mut self) -> Option<Box<dyn MemoryMapper>> {
        self.backend.take_custom()
    }

    /// Acquires a reentrant lock guard for this device.
    ///
    /// While the returned [`LockGuard`] is alive, all register reads, writes, and `update_u32`
    /// calls on the same thread reuse this lock without unlocking between operations, excluding
    /// other threads and processes for the entire sequence.
    ///
    /// A UIO device has one lock. A `/dev/mem` or custom-mapper window takes the page lock of
    /// every page it covers, in ascending address order, so a sequence spanning pages excludes
    /// accesses to all of them; each page lock file stays open only while the guard is held.
    /// Such a window may span at most [`Device::MAX_SEQUENCE_PAGES`] pages
    /// ([`MmregError::LockRangeTooLarge`] otherwise): open a smaller window for the sequence.
    pub fn lock(&self) -> Result<LockGuard<'_>, MmregError> {
        if self.uio_lock.is_some() {
            return self.lock_at(self.base_address);
        }
        let page = page_size();
        let first = self.base_address & !(page - 1);
        let last = (self.base_address + self.size as u64 - 1) & !(page - 1);
        if first == last {
            return self.lock_at(self.base_address);
        }
        let pages = (last - first) / page + 1;
        if pages > Self::MAX_SEQUENCE_PAGES {
            return Err(MmregError::LockRangeTooLarge {
                name: self.name.clone(),
                pages,
                max_pages: Self::MAX_SEQUENCE_PAGES,
            });
        }
        let mut locks = Vec::with_capacity(pages as usize);
        for i in 0..pages {
            locks.push(DeviceLock::open_page(first + i * page)?);
        }
        lock_all(locks)
    }

    /// Most pages a `/dev/mem` or custom-mapper sequence guard ([`Device::lock`]) may hold: one
    /// open lock file each while the guard lives.
    pub const MAX_SEQUENCE_PAGES: u64 = 64;

    /// Takes the lock for one access at physical `address`: the UIO device's lock, or the lock
    /// file of the exact physical page holding the register (the documented protocol, shared with
    /// the CLI and other programs).
    fn lock_at(&self, address: u64) -> Result<LockGuard<'_>, MmregError> {
        if let Some(uio_lock) = &self.uio_lock {
            return uio_lock.lock();
        }
        let page = page_size();
        let lock_addr = address & !(page - 1);
        if lock_addr == (self.base_address & !(page - 1)) {
            if let Some(dev_lock) = self.base_page_lock.get() {
                return dev_lock.lock();
            }
            let dev_lock = DeviceLock::open_page(self.base_address)?;
            let _ = self.base_page_lock.set(dev_lock);
            if let Some(cached) = self.base_page_lock.get() {
                return cached.lock();
            }
        }
        lock(None, lock_addr)
    }

    fn check_access(&self, offset: u32, width: Width, write: bool) -> Result<u64, MmregError> {
        if write && self.read_only {
            return Err(MmregError::ReadOnly {
                name: self.name.clone(),
            });
        }
        let bytes = width.bytes();
        let address = self.base_address.wrapping_add(u64::from(offset));
        if (offset as usize) % bytes != 0 || address % (bytes as u64) != 0 {
            return Err(MmregError::UnalignedWidthAccess {
                address: self.base_address,
                offset,
                width_bits: width.bits(),
            });
        }
        if (offset as usize)
            .checked_add(bytes)
            .map_or(true, |end| end > self.size)
        {
            return Err(MmregError::OutOfBoundsAccess {
                register_name: self.name.clone(),
                offset,
                access_size: bytes,
                region_size: self.size,
            });
        }
        Ok(address)
    }

    /// Reads `width` bits at byte `offset` from the start of the window, zero-extended to `u64`.
    pub fn read(&self, offset: u32, width: Width) -> Result<u64, MmregError> {
        let address = self.check_access(offset, width, false)?;
        let _guard = self.lock_at(address)?;
        Ok(self.backend.read(offset, width))
    }

    /// Writes the low `width` bits of `value` at byte `offset` from the start of the window.
    pub fn write(&self, offset: u32, width: Width, value: u64) -> Result<(), MmregError> {
        let address = self.check_access(offset, width, true)?;
        let _guard = self.lock_at(address)?;
        self.backend.write(offset, width, value);
        Ok(())
    }

    /// Reads an 8-bit register at byte `offset`.
    #[inline]
    pub fn read_u8(&self, offset: u32) -> Result<u8, MmregError> {
        self.read(offset, Width::U8).map(|v| v as u8)
    }

    /// Reads a 16-bit register at a 2-byte-aligned `offset`.
    #[inline]
    pub fn read_u16(&self, offset: u32) -> Result<u16, MmregError> {
        self.read(offset, Width::U16).map(|v| v as u16)
    }

    /// Reads a 32-bit register at a 4-byte-aligned `offset`.
    #[inline]
    pub fn read_u32(&self, offset: u32) -> Result<u32, MmregError> {
        self.read(offset, Width::U32).map(|v| v as u32)
    }

    /// Reads a 64-bit register at an 8-byte-aligned `offset`.
    ///
    /// On 32-bit CPUs (ARMv7, 32-bit RISC-V) the compiler may split a 64-bit access into two
    /// 32-bit bus accesses, so another bus master can see a half-written value. For a register
    /// that must be read or written in one access, use the hardware's own latching scheme or two
    /// `u32` accesses inside [`Device::lock`] in the order the IP specifies.
    #[inline]
    pub fn read_u64(&self, offset: u32) -> Result<u64, MmregError> {
        self.read(offset, Width::U64)
    }

    /// Writes an 8-bit register at byte `offset`.
    #[inline]
    pub fn write_u8(&self, offset: u32, value: u8) -> Result<(), MmregError> {
        self.write(offset, Width::U8, u64::from(value))
    }

    /// Writes a 16-bit register at a 2-byte-aligned `offset`.
    #[inline]
    pub fn write_u16(&self, offset: u32, value: u16) -> Result<(), MmregError> {
        self.write(offset, Width::U16, u64::from(value))
    }

    /// Writes a 32-bit register at a 4-byte-aligned `offset`.
    #[inline]
    pub fn write_u32(&self, offset: u32, value: u32) -> Result<(), MmregError> {
        self.write(offset, Width::U32, u64::from(value))
    }

    /// Writes a 64-bit register at an 8-byte-aligned `offset`.
    ///
    /// On 32-bit CPUs (ARMv7, 32-bit RISC-V) the compiler may split a 64-bit access into two
    /// 32-bit bus accesses, so another bus master can see a half-written value. For a register
    /// that must be read or written in one access, use the hardware's own latching scheme or two
    /// `u32` accesses inside [`Device::lock`] in the order the IP specifies.
    #[inline]
    pub fn write_u64(&self, offset: u32, value: u64) -> Result<(), MmregError> {
        self.write(offset, Width::U64, value)
    }

    /// Atomically reads the 32-bit register at `offset`, replaces the bits selected by `mask`
    /// with `value & mask` (`(old & !mask) | (value & mask)`), writes the result back under the
    /// same lock, and returns the new 32-bit register value.
    ///
    /// Pass `value` already shifted to the field's bit position (e.g. `(field_val << SHIFT)`).
    pub fn update_u32(&self, offset: u32, mask: u32, value: u32) -> Result<u32, MmregError> {
        let address = self.check_access(offset, Width::U32, true)?;
        let _guard = self.lock_at(address)?;
        let current = self.backend.read(offset, Width::U32) as u32;
        let updated = (current & !mask) | (value & mask);
        self.backend.write(offset, Width::U32, u64::from(updated));
        Ok(updated)
    }

    /// Unmasks (re-enables) the UIO interrupt line by writing `1u32` to the `/dev/uioN` descriptor.
    ///
    /// For level-triggered interrupt sources (such as `uio_pdrv_genirq`), always clear the
    /// interrupt cause in the device's registers **before** calling `irq_unmask`, or the interrupt
    /// will fire again immediately.
    pub fn irq_unmask(&self) -> Result<(), MmregError> {
        if self.read_only {
            return Err(MmregError::ReadOnly {
                name: self.name.clone(),
            });
        }
        let file = match &self.backend {
            Backend::Os(os) => os.uio_file(),
            Backend::Custom(_) => None,
        }
        .ok_or_else(|| MmregError::UioRequired {
            operation: "irq_unmask".to_string(),
        })?;
        irq_unmask(file).map_err(|e| MmregError::UioIrq {
            device_path: self
                .device_path
                .clone()
                .unwrap_or_else(|| self.name.clone()),
            details: e.to_string(),
        })
    }

    /// Waits up to `timeout` for a UIO interrupt (`poll(POLLIN)` + 4-byte `read`) and returns
    /// `Ok(Some(count))` with the kernel's cumulative interrupt counter, or `Ok(None)` if the
    /// timeout elapsed before an interrupt arrived.
    ///
    /// `irq_wait` does **not** unmask the interrupt line automatically: clear the interrupt source
    /// in the device registers first, then call [`Device::irq_unmask`].
    pub fn irq_wait(&self, timeout: Duration) -> Result<Option<u32>, MmregError> {
        let file = match &self.backend {
            Backend::Os(os) => os.uio_file(),
            Backend::Custom(_) => None,
        }
        .ok_or_else(|| MmregError::UioRequired {
            operation: "irq_wait".to_string(),
        })?;
        irq_wait(file, timeout).map_err(|e| MmregError::UioIrq {
            device_path: self
                .device_path
                .clone()
                .unwrap_or_else(|| self.name.clone()),
            details: e.to_string(),
        })
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.backend.unmap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct ByteMockMapper {
        bytes: Mutex<Vec<u8>>,
        mapped: bool,
    }

    impl ByteMockMapper {
        fn boxed(size: usize) -> Box<Self> {
            Box::new(Self {
                bytes: Mutex::new(vec![0; size]),
                mapped: false,
            })
        }
    }

    impl MemoryMapper for ByteMockMapper {
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

    #[test]
    fn test_device_all_widths_update_alignment_and_bounds() {
        let dev = Device::addr(0x4000_0000, 16)
            .with_name("test_ip")
            .with_force(true)
            .with_custom_mapper(ByteMockMapper::boxed(16))
            .open()
            .unwrap();

        assert_eq!(dev.name(), "test_ip");
        assert_eq!(dev.base_address(), 0x4000_0000);
        assert_eq!(dev.size(), 16);
        assert!(!dev.is_read_only());
        assert!(dev.device_path().is_none());
        assert!(dev.warnings().is_empty());

        dev.write_u8(1, 0xAB).unwrap();
        assert_eq!(dev.read_u8(1).unwrap(), 0xAB);

        dev.write_u16(2, 0xCAFE).unwrap();
        assert_eq!(dev.read_u16(2).unwrap(), 0xCAFE);

        dev.write_u32(4, 0x1234_5678).unwrap();
        assert_eq!(dev.read_u32(4).unwrap(), 0x1234_5678);
        assert_eq!(
            dev.update_u32(4, 0x0000_FF00, 0x0000_AA00).unwrap(),
            0x1234_AA78
        );
        assert_eq!(dev.read_u32(4).unwrap(), 0x1234_AA78);

        dev.write_u64(8, 0x0102_0304_0506_0708).unwrap();
        assert_eq!(dev.read_u64(8).unwrap(), 0x0102_0304_0506_0708);

        // Alignment checks
        assert_eq!(dev.read_u16(1).unwrap_err().code(), "E_UNALIGNED_ACCESS");
        assert_eq!(dev.read_u32(2).unwrap_err().code(), "E_UNALIGNED_ACCESS");
        let err = dev.read_u64(4).unwrap_err();
        assert_eq!(err.code(), "E_UNALIGNED_ACCESS");
        let msg = err.to_string();
        assert!(
            msg.contains("64-bit") && msg.contains("multiple of 8 bytes"),
            "{msg}"
        );
        assert!(dev.read_u16(1).unwrap_err().to_string().contains("16-bit"));
        assert_eq!(
            dev.update_u32(2, 0xFF, 1).unwrap_err().code(),
            "E_UNALIGNED_ACCESS"
        );

        // Bounds checks
        assert_eq!(dev.read_u8(16).unwrap_err().code(), "E_OUT_OF_BOUNDS");
        assert_eq!(dev.read_u16(16).unwrap_err().code(), "E_OUT_OF_BOUNDS");
        assert_eq!(dev.read_u32(16).unwrap_err().code(), "E_OUT_OF_BOUNDS");
        assert_eq!(dev.read_u64(16).unwrap_err().code(), "E_OUT_OF_BOUNDS");
    }

    #[test]
    fn test_device_read_only_rejects_writes_and_unmask() {
        let dev = Device::addr(0x4000_0000, 16)
            .with_name("ro_ip")
            .read_only()
            .with_force(true)
            .with_custom_mapper(ByteMockMapper::boxed(16))
            .open()
            .unwrap();

        assert!(dev.is_read_only());
        assert_eq!(dev.read_u32(0).unwrap(), 0);
        assert_eq!(dev.write_u8(0, 1).unwrap_err().code(), "E_READ_ONLY");
        assert_eq!(dev.write_u16(0, 1).unwrap_err().code(), "E_READ_ONLY");
        assert_eq!(dev.write_u32(0, 1).unwrap_err().code(), "E_READ_ONLY");
        assert_eq!(dev.write_u64(0, 1).unwrap_err().code(), "E_READ_ONLY");
        assert_eq!(dev.update_u32(0, 1, 1).unwrap_err().code(), "E_READ_ONLY");
        assert_eq!(dev.irq_unmask().unwrap_err().code(), "E_READ_ONLY");
    }

    #[test]
    fn test_device_send_sync_and_reentrant_sequence_lock() {
        let dev = Arc::new(
            Device::addr(0x4000_0000, 16)
                .with_force(true)
                .with_custom_mapper(ByteMockMapper::boxed(16))
                .open()
                .unwrap(),
        );

        // Reentrant sequence guard on main thread across multiple RMW operations.
        {
            let _g1 = dev.lock().unwrap();
            let _g2 = dev.lock().unwrap();
            dev.write_u32(0, 10).unwrap();
            dev.update_u32(0, 0xFF, 20).unwrap();
            assert_eq!(dev.read_u32(0).unwrap(), 20);
        }

        // Concurrent threads sharing Arc<Device> and using dev.lock() sequences.
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let d = Arc::clone(&dev);
                std::thread::spawn(move || {
                    for _ in 0..25 {
                        let _guard = d.lock().unwrap();
                        let cur = d.read_u32(0).unwrap();
                        d.write_u32(0, cur + 1).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(dev.read_u32(0).unwrap(), 120);
    }

    #[test]
    fn test_sequence_guard_holds_every_page_of_the_window() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let page = page_size();
        let base = 0x7A00_0000u64;
        let dev = Device::addr(base, 2 * page as usize)
            .with_force(true)
            .with_custom_mapper(ByteMockMapper::boxed(2 * page as usize))
            .open()
            .unwrap();

        let guard = dev.lock().unwrap();
        // Accesses to both pages inside the sequence reuse the held locks.
        dev.write_u32(0, 1).unwrap();
        dev.write_u32(page as u32, 2).unwrap();

        let step = Arc::new(AtomicUsize::new(0));
        let step_bg = Arc::clone(&step);
        let bg = std::thread::spawn(move || {
            step_bg.store(1, Ordering::SeqCst);
            // Another locker of the second page (another thread, or another process using the
            // page-lock protocol) waits for the whole sequence.
            let lock = DeviceLock::open_page(base + page).unwrap();
            let _g = lock.lock().unwrap();
            step_bg.store(2, Ordering::SeqCst);
        });
        while step.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(
            step.load(Ordering::SeqCst),
            1,
            "second page must stay locked"
        );

        drop(guard);
        bg.join().unwrap();
        assert_eq!(step.load(Ordering::SeqCst), 2);
        assert_eq!(dev.read_u32(page as u32).unwrap(), 2);

        // Boundary: exactly MAX_SEQUENCE_PAGES (64) pages succeeds; 65 pages is refused with E_LOCK_RANGE.
        let max_pages = Device::MAX_SEQUENCE_PAGES as usize;
        let boundary_base = 0x7A80_0000u64;
        let max_dev = Device::addr(boundary_base, max_pages * page as usize)
            .with_force(true)
            .with_custom_mapper(ByteMockMapper::boxed(max_pages * page as usize))
            .open()
            .unwrap();
        {
            let _g = max_dev.lock().unwrap();
            let last_page_off = ((max_pages - 1) * page as usize) as u32;
            max_dev.write_u32(last_page_off, 0xCAFE_1234).unwrap();
            assert_eq!(max_dev.read_u32(last_page_off).unwrap(), 0xCAFE_1234);
        }
        let over_dev = Device::addr(boundary_base, (max_pages + 1) * page as usize)
            .with_force(true)
            .with_custom_mapper(ByteMockMapper::boxed((max_pages + 1) * page as usize))
            .open()
            .unwrap();
        assert_eq!(over_dev.lock().unwrap_err().code(), "E_LOCK_RANGE");

        // Large windows: single accesses lock the exact physical page (so they exclude the CLI
        // and other programs locking that page), and a sequence guard over more than
        // MAX_SEQUENCE_PAGES pages is refused.
        let large_base = 0x7B00_0000u64;
        let large_dev = Arc::new(
            Device::addr(large_base, 100 * page as usize)
                .with_force(true)
                .with_custom_mapper(ByteMockMapper::boxed(100 * page as usize))
                .open()
                .unwrap(),
        );
        assert_eq!(large_dev.lock().unwrap_err().code(), "E_LOCK_RANGE");

        let page99 = DeviceLock::open_page(large_base + 99 * page).unwrap();
        let held = page99.lock().unwrap();
        let step = Arc::new(AtomicUsize::new(0));
        let (step_bg, dev_bg) = (Arc::clone(&step), Arc::clone(&large_dev));
        let bg = std::thread::spawn(move || {
            step_bg.store(1, Ordering::SeqCst);
            dev_bg.write_u32((99 * page) as u32, 0xABCD_1234).unwrap();
            step_bg.store(2, Ordering::SeqCst);
        });
        while step.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(
            step.load(Ordering::SeqCst),
            1,
            "page 99 access waits for its own page lock"
        );
        drop(held);
        bg.join().unwrap();
        assert_eq!(large_dev.read_u32((99 * page) as u32).unwrap(), 0xABCD_1234);
    }

    #[test]
    fn test_device_uio_required_and_lookup_errors() {
        let dev = Device::addr(0x4000_0000, 16)
            .with_force(true)
            .with_custom_mapper(ByteMockMapper::boxed(16))
            .open()
            .unwrap();
        assert_eq!(dev.irq_unmask().unwrap_err().code(), "E_UIO_REQUIRED");
        assert_eq!(
            dev.irq_wait(Duration::from_millis(1)).unwrap_err().code(),
            "E_UIO_REQUIRED"
        );
        assert_eq!(
            uio::find("nonexistent_uio_device_xyz").unwrap_err().code(),
            "E_UIO_NOT_FOUND"
        );
        assert_eq!(
            Device::open_uio("nonexistent_uio_device_xyz")
                .err()
                .unwrap()
                .code(),
            "E_UIO_NOT_FOUND"
        );
        assert_eq!(
            Device::open_uio_path("/dev/nonexistent_uio99")
                .err()
                .unwrap()
                .code(),
            "E_UIO_ADDRESS"
        );

        // Off-target unit tests with `.with_custom_mapper(...)` + `.with_force(true)` can open
        // `Device::uio` and `Device::uio_path` even when `/sys/class/uio` is absent.
        let mock_uio = Device::uio("mock_axi_gpio")
            .with_force(true)
            .with_custom_mapper(ByteMockMapper::boxed(page_size() as usize))
            .open()
            .unwrap();
        mock_uio.write_u32(4, 0x55AA_55AA).unwrap();
        assert_eq!(mock_uio.read_u32(4).unwrap(), 0x55AA_55AA);

        // Without `with_force(true)`, `Device::uio` still requires the UIO device to exist even
        // when a custom mapper is supplied.
        assert_eq!(
            Device::uio("nonexistent_uio_device_xyz")
                .with_custom_mapper(ByteMockMapper::boxed(page_size() as usize))
                .open()
                .err()
                .unwrap()
                .code(),
            "E_UIO_NOT_FOUND"
        );

        // `Device::uio_path` with `.with_custom_mapper(...)` + `.with_force(true)` and builder
        // getters (`name`, `base_address`, `size`, `device_path`, `warnings`, `read`/`write`).
        let mock_path = Device::uio_path("/dev/nonexistent_uio99")
            .region(1)
            .with_name("custom_uio_label")
            .with_read_only(false)
            .with_force(true)
            .with_custom_mapper(ByteMockMapper::boxed(page_size() as usize))
            .open()
            .unwrap();
        assert_eq!(mock_path.name(), "custom_uio_label");
        assert_eq!(mock_path.base_address(), 0);
        assert_eq!(mock_path.size(), page_size() as usize);
        assert!(!mock_path.is_read_only());
        assert!(mock_path.device_path().is_none());
        assert!(mock_path.warnings().is_empty());
        mock_path.write(2, Width::U16, 0xBEEF).unwrap();
        assert_eq!(mock_path.read(2, Width::U16).unwrap(), 0xBEEF);

        // Invalid `Device::addr` windows (`size == 0` or `base + size > u64::MAX`).
        assert_eq!(
            Device::addr(0x4000_0000, 0).open().err().unwrap().code(),
            "E_OUT_OF_BOUNDS"
        );
        assert_eq!(
            Device::addr(u64::MAX, 4).open().err().unwrap().code(),
            "E_OUT_OF_BOUNDS"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_device_irq_wait_and_unmask_with_pipe() {
        use std::io::{Read, Write};

        let (mut pipe_reader, pipe_writer) = crate::mmio::test_pipe();

        // Give OsMapper the writer end to test irq_unmask first.
        let mut os = OsMapper::new(
            MapperType::Uio {
                device_path: "/dev/uio_fake".into(),
                region_index: 0,
            },
            false,
        );
        os.set_uio_file_for_test(pipe_writer);
        let dev_unmask = Device {
            name: "uio_fake".into(),
            base_address: 0xA000_0000,
            size: 16,
            read_only: false,
            device_path: Some("/dev/uio_fake".into()),
            warnings: Vec::new(),
            backend: Backend::Os(os),
            uio_lock: None,
            base_page_lock: OnceLock::new(),
            _pm_guard: None,
        };
        dev_unmask.irq_unmask().unwrap();
        let mut buf = [0u8; 4];
        pipe_reader.read_exact(&mut buf).unwrap();
        assert_eq!(u32::from_ne_bytes(buf), 1);

        // Now attach the reader end of a second pipe to test irq_wait timeout and interrupt count.
        let (wait_reader, mut wait_writer) = crate::mmio::test_pipe();
        let mut os_wait = OsMapper::new(
            MapperType::Uio {
                device_path: "/dev/uio_fake".into(),
                region_index: 0,
            },
            true,
        );
        os_wait.set_uio_file_for_test(wait_reader);
        let dev_wait = Device {
            name: "uio_fake".into(),
            base_address: 0xA000_0000,
            size: 16,
            read_only: true,
            device_path: Some("/dev/uio_fake".into()),
            warnings: Vec::new(),
            backend: Backend::Os(os_wait),
            uio_lock: None,
            base_page_lock: OnceLock::new(),
            _pm_guard: None,
        };
        assert_eq!(dev_wait.irq_wait(Duration::from_millis(10)).unwrap(), None);
        wait_writer.write_all(&7u32.to_ne_bytes()).unwrap();
        assert_eq!(
            dev_wait.irq_wait(Duration::from_millis(50)).unwrap(),
            Some(7)
        );
    }
}

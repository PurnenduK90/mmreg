//! Memory mapper backends: the `MemoryMapper` trait and its `/dev/mem` / UIO implementation.

use crate::error::MmregError;
use crate::memcheck::UioMap;
use crate::mmio::{open_device, page_size, Mapping};
use std::fs::File;
use std::path::Path;

/// Width of one register access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    /// 8 bits
    U8,
    /// 16 bits
    U16,
    /// 32 bits
    U32,
    /// 64 bits (two bus accesses on 32-bit buses)
    U64,
}

impl Width {
    /// Size of the access in bytes.
    #[inline]
    pub fn bytes(self) -> usize {
        match self {
            Width::U8 => 1,
            Width::U16 => 2,
            Width::U32 => 4,
            Width::U64 => 8,
        }
    }

    /// Size of the access in bits (`8`, `16`, `32`, or `64`).
    #[inline]
    pub fn bits(self) -> u32 {
        (self.bytes() * 8) as u32
    }

    /// Parses a bit width (`8`, `16`, `32`, or `64`).
    #[inline]
    pub fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            8 => Some(Width::U8),
            16 => Some(Width::U16),
            32 => Some(Width::U32),
            64 => Some(Width::U64),
            _ => None,
        }
    }
}

/// A way to reach physical memory (`/dev/mem`, UIO, or a custom/mock backend).
///
/// Callers only read and write while mapped, at an `offset` aligned to the access width and inside
/// the mapped `size`. Values are zero-extended to `u64`; writes use the low `width` bits.
pub trait MemoryMapper: Send {
    /// Maps `size` bytes at physical `address`, replacing any previous mapping.
    fn map(&mut self, address: u64, size: usize) -> Result<(), MmregError>;

    /// Releases the mapping (no-op when not mapped).
    fn unmap(&mut self);

    /// Returns true while a mapping is held.
    fn is_mapped(&self) -> bool;

    /// Reads `width` bits at `offset` bytes past the mapped address.
    fn read(&self, offset: u32, width: Width) -> u64;

    /// Writes the low `width` bits of `value` at `offset` bytes past the mapped address.
    fn write(&self, offset: u32, width: Width, value: u64);
}

/// Backend used to reach a physical address.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum MapperType {
    /// Use the UIO region that covers the address when there is one, `/dev/mem` otherwise.
    #[default]
    Auto,
    /// Use `/dev/mem`; needs the `debug` feature (`[E_DEBUG_DISABLED]` without it).
    DevMem,
    /// Use region `region_index` of a UIO device; its physical window is read from
    /// `/sys/class/uio/<device>/maps/map<region_index>/{addr,size}`.
    Uio {
        /// Path to the UIO device (e.g. `"/dev/uio0"`)
        device_path: String,
        /// Memory region index (`mapN`)
        region_index: usize,
    },
}

const SYSFS_UIO: &str = "/sys/class/uio";

/// `/dev/mem` or UIO mapper for a resolved (non-`Auto`) `MapperType`.
pub(crate) struct OsMapper {
    kind: MapperType,
    read_only: bool,
    mapping: Option<Mapping>,
    uio_file: Option<File>,
}

impl OsMapper {
    pub(crate) fn new(kind: MapperType, read_only: bool) -> Self {
        OsMapper {
            kind,
            read_only,
            mapping: None,
            uio_file: None,
        }
    }

    /// Returns the open `/dev/uioN` descriptor when mapped through UIO.
    pub(crate) fn uio_file(&self) -> Option<&File> {
        self.uio_file.as_ref()
    }

    #[cfg(all(test, unix))]
    pub(crate) fn set_uio_file_for_test(&mut self, file: File) {
        self.uio_file = Some(file);
    }
}

impl MemoryMapper for OsMapper {
    fn map(&mut self, address: u64, size: usize) -> Result<(), MmregError> {
        self.mapping = None;
        self.uio_file = None;
        match &self.kind {
            MapperType::Auto | MapperType::DevMem => {
                self.mapping = Some(map_devmem(address, size, self.read_only)?);
            }
            MapperType::Uio {
                device_path,
                region_index,
            } => {
                let (file, mapping) = map_uio(
                    Path::new(SYSFS_UIO),
                    device_path,
                    *region_index,
                    address,
                    size,
                    self.read_only,
                )?;
                self.uio_file = Some(file);
                self.mapping = Some(mapping);
            }
        }
        Ok(())
    }

    fn unmap(&mut self) {
        self.mapping = None;
        self.uio_file = None;
    }

    fn is_mapped(&self) -> bool {
        self.mapping.is_some()
    }

    #[inline]
    fn read(&self, offset: u32, width: Width) -> u64 {
        self.mapping.as_ref().map_or(0, |m| m.read(offset, width))
    }

    #[inline]
    fn write(&self, offset: u32, width: Width, value: u64) {
        if let Some(m) = &self.mapping {
            m.write(offset, width, value);
        }
    }
}

/// Without the `debug` feature, `/dev/mem` is never opened.
#[cfg(not(feature = "debug"))]
fn map_devmem(address: u64, _size: usize, _read_only: bool) -> Result<Mapping, MmregError> {
    Err(MmregError::DebugDisabled { address })
}

#[cfg(feature = "debug")]
fn map_devmem(address: u64, size: usize, read_only: bool) -> Result<Mapping, MmregError> {
    let page_base = address & !(page_size() - 1);
    let file = open_device("/dev/mem", true, read_only).map_err(|e| MmregError::DevMemOpen {
        address,
        details: e.to_string(),
    })?;
    Mapping::new(
        &file,
        page_base,
        (address - page_base) as usize,
        size,
        read_only,
    )
    .map_err(|e| MmregError::DevMemMmap {
        address,
        page_base,
        size,
        details: e.to_string(),
    })
}

/// Maps `[address, address + size)` through UIO region `mapN`.
///
/// The kernel maps a region from the page holding its sysfs `addr`, and `size` counts from `addr`.
fn map_uio(
    sysfs_uio_root: &Path,
    device_path: &str,
    region_index: usize,
    address: u64,
    size: usize,
    read_only: bool,
) -> Result<(File, Mapping), MmregError> {
    let data_offset = uio_data_offset(sysfs_uio_root, device_path, region_index, address, size)?;
    let file = open_device(device_path, false, read_only).map_err(|e| MmregError::UioOpen {
        device_path: device_path.to_string(),
        details: e.to_string(),
    })?;
    let mapping = Mapping::new(
        &file,
        region_index as u64 * page_size(),
        data_offset,
        size,
        read_only,
    )
    .map_err(|e| MmregError::UioMmap {
        device_path: device_path.to_string(),
        region_index,
        size,
        details: e.to_string(),
    })?;
    Ok((file, mapping))
}

/// Bytes from the start of UIO region `mapN`'s mmap to `address`, after checking that
/// `[address, address + size)` lies inside the region.
fn uio_data_offset(
    sysfs_uio_root: &Path,
    device_path: &str,
    region_index: usize,
    address: u64,
    size: usize,
) -> Result<usize, MmregError> {
    let invalid = |details: &str| MmregError::UioAddressInvalid {
        device_path: device_path.to_string(),
        region_index,
        address,
        details: details.to_string(),
    };
    let map_dir = Path::new(device_path)
        .file_name()
        .map(|dev| {
            sysfs_uio_root
                .join(dev)
                .join("maps")
                .join(format!("map{region_index}"))
        })
        .ok_or_else(|| invalid("device path has no file name"))?;
    let region = UioMap::read(&map_dir)
        .ok_or_else(|| invalid("cannot read the region's sysfs addr/size"))?;
    if !region.contains(address, size) {
        return Err(invalid(&format!(
            "range is outside the region 0x{:X} + 0x{:X}",
            region.addr, region.size
        )));
    }
    usize::try_from(address - (region.addr & !(page_size() - 1)))
        .map_err(|_| invalid("offset overflows usize"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_unmapped_os_mapper_is_inert() {
        let mut mapper = OsMapper::new(MapperType::DevMem, false);
        assert!(!mapper.is_mapped());
        assert!(mapper.uio_file().is_none());
        assert_eq!(mapper.read(0, Width::U32), 0);
        mapper.write(0, Width::U32, 0xDEAD_BEEF);
        assert_eq!(Width::U64.bytes(), 8);
        assert_eq!(Width::U64.bits(), 64);
        assert_eq!(Width::from_bits(8), Some(Width::U8));
        assert_eq!(Width::from_bits(16), Some(Width::U16));
        assert_eq!(Width::from_bits(32), Some(Width::U32));
        assert_eq!(Width::from_bits(64), Some(Width::U64));
        assert_eq!(Width::from_bits(12), None);
        mapper.unmap();

        let mut uio = OsMapper::new(
            MapperType::Uio {
                device_path: "/dev/nonexistent_uio99".to_string(),
                region_index: 0,
            },
            true,
        );
        assert_eq!(uio.map(0xA000_0010, 4).unwrap_err().code(), "E_UIO_ADDRESS");
        assert!(!uio.is_mapped());
    }

    #[test]
    fn test_uio_data_offset_uses_sysfs_addr_and_size() {
        let root = crate::test_dir("uio_offset");
        let map0 = root.join("uio0/maps/map0");
        fs::create_dir_all(&map0).unwrap();
        // uio_pdrv_genirq: page-aligned addr, device at offs 0x100, size counted from addr.
        fs::write(map0.join("addr"), "0xa0000000\n").unwrap();
        fs::write(map0.join("offset"), "0x100\n").unwrap();
        fs::write(map0.join("size"), "0x1000\n").unwrap();

        let off = |addr, size| uio_data_offset(&root, "/dev/uio0", 0, addr, size);
        assert_eq!(off(0xA000_0120, 4).unwrap(), 0x120);
        assert_eq!(off(0xA000_0FFC, 4).unwrap(), 0xFFC);
        assert_eq!(off(0xA000_0FFC, 8).unwrap_err().code(), "E_UIO_ADDRESS");
        assert_eq!(off(0x9FFF_FFFC, 4).unwrap_err().code(), "E_UIO_ADDRESS");
        assert_eq!(
            uio_data_offset(&root, "/dev/uio0", 1, 0xA000_0000, 4)
                .unwrap_err()
                .code(),
            "E_UIO_ADDRESS"
        );

        let _ = fs::remove_dir_all(&root);
    }
}

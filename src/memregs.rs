//! Memory mapper backends: the `MemoryMapper` trait and its `/dev/mem` / UIO implementation.

use crate::error::MmregError;
use crate::memcheck::UioMap;
use crate::mmio::{Mapping, open_device, page_size};
use std::path::Path;

/// A way to reach physical memory (`/dev/mem`, UIO, or a custom/mock backend).
///
/// `Interface` only calls `read_u32`/`write_u32` while mapped, with a 4-byte-aligned `offset`
/// that lies inside the mapped `size`.
pub trait MemoryMapper: Send {
    /// Maps `size` bytes at physical `address`, replacing any previous mapping.
    fn map(&mut self, address: u64, size: usize) -> Result<(), MmregError>;

    /// Releases the mapping (no-op when not mapped).
    fn unmap(&mut self);

    /// Returns true while a mapping is held.
    fn is_mapped(&self) -> bool;

    /// Reads 32 bits at `offset` bytes past the mapped address.
    fn read_u32(&self, offset: u32) -> u32;

    /// Writes 32 bits at `offset` bytes past the mapped address.
    fn write_u32(&self, offset: u32, value: u32);
}

/// Backend used to reach a physical address.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MapperType {
    /// Use the UIO region that covers the address when there is one, `/dev/mem` otherwise.
    #[default]
    Auto,
    /// Use `/dev/mem`.
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
    mapping: Option<Mapping>,
}

impl OsMapper {
    pub(crate) fn new(kind: MapperType) -> Self {
        OsMapper {
            kind,
            mapping: None,
        }
    }
}

impl MemoryMapper for OsMapper {
    fn map(&mut self, address: u64, size: usize) -> Result<(), MmregError> {
        self.mapping = None;
        self.mapping = Some(match &self.kind {
            MapperType::Auto | MapperType::DevMem => map_devmem(address, size)?,
            MapperType::Uio {
                device_path,
                region_index,
            } => map_uio(
                Path::new(SYSFS_UIO),
                device_path,
                *region_index,
                address,
                size,
            )?,
        });
        Ok(())
    }

    fn unmap(&mut self) {
        self.mapping = None;
    }

    fn is_mapped(&self) -> bool {
        self.mapping.is_some()
    }

    #[inline]
    fn read_u32(&self, offset: u32) -> u32 {
        self.mapping.as_ref().map_or(0, |m| m.read_u32(offset))
    }

    #[inline]
    fn write_u32(&self, offset: u32, value: u32) {
        if let Some(m) = &self.mapping {
            m.write_u32(offset, value);
        }
    }
}

fn map_devmem(address: u64, size: usize) -> Result<Mapping, MmregError> {
    let page_base = address & !(page_size() - 1);
    let file = open_device("/dev/mem", true).map_err(|e| MmregError::DevMemOpen {
        address,
        details: e.to_string(),
    })?;
    Mapping::new(&file, page_base, (address - page_base) as usize, size).map_err(|e| {
        MmregError::DevMemMmap {
            address,
            page_base,
            size,
            details: e.to_string(),
        }
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
) -> Result<Mapping, MmregError> {
    let data_offset = uio_data_offset(sysfs_uio_root, device_path, region_index, address, size)?;
    let file = open_device(device_path, false).map_err(|e| MmregError::UioOpen {
        device_path: device_path.to_string(),
        details: e.to_string(),
    })?;
    Mapping::new(&file, region_index as u64 * page_size(), data_offset, size).map_err(|e| {
        MmregError::UioMmap {
            device_path: device_path.to_string(),
            region_index,
            size,
            details: e.to_string(),
        }
    })
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
        let mut mapper = OsMapper::new(MapperType::DevMem);
        assert!(!mapper.is_mapped());
        assert_eq!(mapper.read_u32(0), 0);
        mapper.write_u32(0, 0xDEAD_BEEF);
        mapper.unmap();

        let mut uio = OsMapper::new(MapperType::Uio {
            device_path: "/dev/nonexistent_uio99".to_string(),
            region_index: 0,
        });
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

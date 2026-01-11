use std::fs::{File, OpenOptions};
use std::io::{self};

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

use std::ptr;

// --- Memory Mapper Trait ---

/// Trait for different memory mapping backends (/dev/mem, UIO, custom, etc.)
///
/// Implementations of this trait provide a unified interface for mapping and accessing
/// physical memory through different mechanisms. This allows the caller to switch between
/// backends without changing application code.
pub(crate) trait MemoryMapper: Send {
    /// Maps a physical memory region and returns the page offset.
    ///
    /// # Arguments
    /// * `address` - Physical address to map
    /// * `size` - Size of region to map in bytes
    ///
    /// # Returns
    /// * `Ok(offset)` - Offset from mapped page start to requested address
    /// * `Err(String)` - Error message if mapping fails
    fn map(&mut self, address: u64, size: usize) -> Result<isize, String>;

    /// Unmaps the memory region and releases resources.
    ///
    /// # Returns
    /// * `Ok(())` - Success
    /// * `Err(String)` - Error message if unmapping fails
    fn unmap(&mut self) -> Result<(), String>;

    /// Reads a 32-bit value from a mapped memory location.
    ///
    /// # Arguments
    /// * `offset` - Byte offset from the mapped region start
    ///
    /// # Returns
    /// The 32-bit value read from memory
    fn read_u32(&self, offset: u32) -> u32;

    /// Writes a 32-bit value to a mapped memory location.
    ///
    /// # Arguments
    /// * `offset` - Byte offset from the mapped region start
    /// * `value` - The 32-bit value to write
    fn write_u32(&self, offset: u32, value: u32);

    /// Returns the mapped memory pointer if currently mapped.
    ///
    /// # Returns
    /// * `Some(ptr)` - Pointer to mapped memory
    /// * `None` - Not currently mapped
    fn get_ptr(&self) -> Option<*mut u8>;
}

// --- /dev/mem Mapper Implementation ---

/// Memory mapper using /dev/mem for direct physical memory access.
///
/// This is the standard Linux method for user-space access to physical memory.
/// Requires root/elevated privileges.
pub(crate) struct DevMemMapper {
    /// File handle to /dev/mem
    file: Option<File>,
    /// Pointer to mapped memory
    map_ptr: Option<*mut u8>,
    /// Size of the mapped region
    map_size: usize,
}

impl DevMemMapper {
    /// Creates a new DevMemMapper instance.
    pub(crate) fn new() -> Self {
        DevMemMapper {
            file: None,
            map_ptr: None,
            map_size: 0,
        }
    }
}

impl MemoryMapper for DevMemMapper {
    fn map(&mut self, address: u64, size: usize) -> Result<isize, String> {
        // Use the low-level devmem mapping function
        let (file, map_ptr, offset) = crate::devmem::mmap_register(address, size)?;
        
        self.file = Some(file);
        self.map_ptr = Some(map_ptr);
        self.map_size = size;

        Ok(offset)
    }

    fn unmap(&mut self) -> Result<(), String> {
        if let Some(ptr) = self.map_ptr {
            crate::devmem::munmap_register(ptr, self.map_size);
            self.map_ptr = None;
            self.file = None;
            Ok(())
        } else {
            Err("Memory not mapped".to_string())
        }
    }

    fn read_u32(&self, offset: u32) -> u32 {
        if let Some(ptr) = self.map_ptr {
            crate::devmem::read_u32_mapped(ptr, 0, offset)
        } else {
            0
        }
    }

    fn write_u32(&self, offset: u32, value: u32) {
        if let Some(ptr) = self.map_ptr {
            crate::devmem::write_u32_mapped(ptr, 0, offset, value);
        }
    }

    fn get_ptr(&self) -> Option<*mut u8> {
        self.map_ptr
    }
}

// --- UIO Mapper Implementation ---

/// Memory mapper using UIO (Userspace I/O) devices.
///
/// UIO devices provide safer, kernel-driver-managed access to specific hardware resources.
/// No special privileges required if the device is properly accessible.
pub(crate) struct UioMemMapper {
    /// Path to the UIO device (e.g., "/dev/uio0")
    device_path: String,
    /// File handle to the UIO device
    file: Option<File>,
    /// Pointer to mapped memory
    map_ptr: Option<*mut u8>,
    /// Size of the mapped region
    map_size: usize,
    /// Memory region index within the UIO device
    region_index: usize,
}

impl UioMemMapper {
    /// Creates a new UioMemMapper instance.
    ///
    /// # Arguments
    /// * `device_path` - Path to the UIO device (e.g., "/dev/uio0")
    /// * `region_index` - Memory region index (typically 0)
    pub(crate) fn new(device_path: String, region_index: usize) -> Self {
        UioMemMapper {
            device_path,
            file: None,
            map_ptr: None,
            map_size: 0,
            region_index,
        }
    }
}

impl MemoryMapper for UioMemMapper {
    fn map(&mut self, _address: u64, size: usize) -> Result<isize, String> {
        // Use the low-level uiomem mapping function
        let (file, map_ptr, _offset) = crate::uiomem::mmap_uio(&self.device_path, self.region_index, size)?;
        
        self.file = Some(file);
        self.map_ptr = Some(map_ptr);
        self.map_size = size;

        // UIO mapping starts at region base, no additional offset
        Ok(0)
    }

    fn unmap(&mut self) -> Result<(), String> {
        if let Some(ptr) = self.map_ptr {
            crate::uiomem::munmap_uio(ptr, self.map_size);
            self.map_ptr = None;
            self.file = None;
            Ok(())
        } else {
            Err("Memory not mapped".to_string())
        }
    }

    fn read_u32(&self, offset: u32) -> u32 {
        if let Some(ptr) = self.map_ptr {
            crate::uiomem::read_u32_uio(ptr, offset)
        } else {
            0
        }
    }

    fn write_u32(&self, offset: u32, value: u32) {
        if let Some(ptr) = self.map_ptr {
            crate::uiomem::write_u32_uio(ptr, offset, value);
        }
    }

    fn get_ptr(&self) -> Option<*mut u8> {
        self.map_ptr
    }
}

// --- Mapper Factory ---

/// Enumeration of available memory mapping backends.
pub(crate) enum MapperType {
    /// Use /dev/mem for direct physical memory access (default)
    DevMem,
    /// Use a UIO device with specified path and optional region index
    Uio {
        /// Path to the UIO device (e.g., "/dev/uio0")
        device_path: String,
        /// Memory region index (default: 0)
        region_index: usize,
    },
}

/// Creates a new MemoryMapper instance of the specified type.
///
/// # Arguments
/// * `mapper_type` - Type of mapper to create
///
/// # Returns
/// A boxed MemoryMapper trait object
///
/// # Example
/// ```ignore
/// let mapper = create_mapper(MapperType::DevMem);
/// // or
/// let mapper = create_mapper(MapperType::Uio {
///     device_path: "/dev/uio0".to_string(),
///     region_index: 0,
/// });
/// ```
pub(crate) fn create_mapper(mapper_type: MapperType) -> Box<dyn MemoryMapper> {
    match mapper_type {
        MapperType::DevMem => Box::new(DevMemMapper::new()),
        MapperType::Uio {
            device_path,
            region_index,
        } => Box::new(UioMemMapper::new(device_path, region_index)),
    }
}

// --- Legacy Functions (for backward compatibility) ---

/// Maps the physical memory page containing the register address into the process's virtual space.
/// Returns the file descriptor and the virtual memory pointer.
///
/// **Note:** This is a legacy function. Prefer using the `MemoryMapper` trait for new code.
pub(crate) fn mmap_register(
    address: u64,
    map_size: usize,
) -> Result<(File, *mut u8, isize), String> {
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    if page_size == 0 {
        return Err("Could not determine system page size.".to_string());
    }
    let page_mask = !(page_size - 1);

    let map_base = address & page_mask;
    let register_offset = (address - map_base) as isize;

    let file = match OpenOptions::new().read(true).write(true).open("/dev/mem") {
        Ok(f) => f,
        Err(_) => return Err("Failed to open /dev/mem. Check permissions.".to_string()),
    };
    let fd = file.as_raw_fd();

    let map_ptr = unsafe {
        libc::mmap(
            ptr::null_mut(),
            map_size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            map_base as libc::off_t,
        )
    };

    if map_ptr == libc::MAP_FAILED {
        let err = io::Error::last_os_error();
        return Err(format!("mmap failed: {}", err));
    }

    Ok((file, map_ptr as *mut u8, register_offset))
}

/// Unmaps the memory region.
///
/// **Note:** This is a legacy function. Prefer using the `MemoryMapper` trait for new code.
pub(crate) fn munmap_register(map_ptr: *mut u8, map_size: usize) {
    unsafe {
        libc::munmap(map_ptr as *mut libc::c_void, map_size);
    }
}

/// Safely read a 32-bit value from mapped memory
///
/// **Note:** This is a legacy function. Prefer using the `MemoryMapper` trait for new code.
pub(crate) fn read_u32_mapped(ptr: *mut u8, iface_offset: isize, reg_offset: u32) -> u32 {
    let reg_ptr = unsafe { ptr.offset(iface_offset + reg_offset as isize) } as *const u32;
    unsafe { std::ptr::read_volatile(reg_ptr) }
}

/// Safely write a 32-bit value to mapped memory
///
/// **Note:** This is a legacy function. Prefer using the `MemoryMapper` trait for new code.
pub(crate) fn write_u32_mapped(ptr: *mut u8, iface_offset: isize, reg_offset: u32, value: u32) {
    let reg_ptr = unsafe { ptr.offset(iface_offset + reg_offset as isize) } as *mut u32;
    unsafe {
        std::ptr::write_volatile(reg_ptr, value);
    }
}
use std::fs::{File, OpenOptions};
use std::io::{self};

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

use std::ptr;

/// Maps a UIO device's memory region into the process's virtual address space.
///
/// UIO (Userspace I/O) devices provide safer memory-mapped access to hardware compared to `/dev/mem`
/// because they limit access to specific hardware resources defined by kernel drivers.
///
/// # Arguments
/// * `uio_device_path` - Path to the UIO device file (e.g., "/dev/uio0")
/// * `region_index` - Memory region index (most devices have region 0)
/// * `map_size` - Size of the region to map in bytes
///
/// # Returns
/// * `Ok((file, ptr, offset))` - Contains:
///   - `file`: Open file handle to the UIO device (must be kept open while memory is mapped)
///   - `ptr`: Pointer to the mapped virtual memory
///   - `offset`: Always 0 for UIO (memory is directly mapped from region start)
/// * `Err(String)` - Error message if mapping fails
///
/// # Safety Considerations
/// - Requires permission to access the UIO device
/// - The returned pointer must be treated as volatile memory
/// - The pointer is only valid while the returned file handle is open
/// - Improper use can corrupt hardware state or crash the system
///
/// # Technical Details
/// Unlike `/dev/mem`, UIO device mapping:
/// 1. Opens the specific UIO device file
/// 2. Uses mmap(2) with the device file descriptor
/// 3. The offset parameter corresponds to the memory region index
/// 4. Provides kernel driver-managed access to hardware resources
pub(crate) fn mmap_uio(
    uio_device_path: &str,
    region_index: usize,
    map_size: usize,
) -> Result<(File, *mut u8, isize), String> {
    // Step 1: Open the UIO device file
    // UIO devices are character devices that support mmap operations
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(uio_device_path)
    {
        Ok(f) => f,
        Err(e) => {
            return Err(format!(
                "Failed to open UIO device '{}': {}. Check permissions and device existence.",
                uio_device_path, e
            ));
        }
    };
    let fd = file.as_raw_fd();

    // Step 2: Calculate the memory offset for the region
    // Each memory region in a UIO device is typically mapped at a multiple of the page size
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    if page_size == 0 {
        return Err("Could not determine system page size.".to_string());
    }
    let region_offset = (region_index * page_size) as libc::off_t;

    // Step 3: Perform memory mapping using mmap(2)
    // For UIO devices, the offset indicates which memory region to map
    let map_ptr = unsafe {
        libc::mmap(
            ptr::null_mut(),                            // Let kernel choose virtual address
            map_size,                                   // Map this many bytes
            libc::PROT_READ | libc::PROT_WRITE,         // Read and write permissions
            libc::MAP_SHARED,                           // Shared mapping (hardware registers)
            fd,                                         // File descriptor of UIO device
            region_offset,                              // Region index as offset
        )
    };

    // Step 4: Check if mmap succeeded
    if map_ptr == libc::MAP_FAILED {
        let err = io::Error::last_os_error();
        return Err(format!("mmap failed for UIO device '{}': {}", uio_device_path, err));
    }

    // Return the file handle, virtual pointer, and offset (0 for UIO as mapping starts at region base)
    Ok((file, map_ptr as *mut u8, 0))
}

/// Unmaps a previously mapped UIO device memory region.
///
/// This must be called to properly release memory resources and prevent resource leaks.
///
/// # Arguments
/// * `map_ptr` - The pointer returned from `mmap_uio`
/// * `map_size` - The size of the mapped region (must match the size from mmap_uio)
///
/// # Safety Considerations
/// - Must not be called with invalid pointers or mismatched sizes
/// - Any attempts to dereference the pointer after unmapping will cause undefined behavior
/// - The file handle from `mmap_uio` should be closed before or after calling this
pub(crate) fn munmap_uio(map_ptr: *mut u8, map_size: usize) {
    unsafe {
        // munmap(2) removes the mapping and frees kernel resources
        libc::munmap(map_ptr as *mut libc::c_void, map_size);
    }
}

/// Safely reads a 32-bit value from a UIO-mapped register.
///
/// Uses volatile access to ensure the compiler doesn't optimize away the memory access.
/// This is essential for memory-mapped hardware registers where reads can have side effects.
///
/// # Arguments
/// * `ptr` - The mapped virtual memory pointer from `mmap_uio`
/// * `offset` - Byte offset from the base of the mapped region
///
/// # Returns
/// The 32-bit value read from the register
///
/// # Safety
/// - The offset must point to a valid, readable 32-bit aligned location
/// - The pointer must remain valid during the read operation
pub(crate) fn read_u32_uio(ptr: *mut u8, offset: u32) -> u32 {
    // Calculate the final virtual address: mapped_ptr + offset
    let reg_ptr = unsafe { ptr.offset(offset as isize) } as *const u32;
    // Use volatile read to prevent compiler optimization
    unsafe { std::ptr::read_volatile(reg_ptr) }
}

/// Safely writes a 32-bit value to a UIO-mapped register.
///
/// Uses volatile access to ensure the compiler doesn't optimize away the memory write.
/// Writing to hardware registers often has side effects (e.g., triggering actions, status changes).
///
/// # Arguments
/// * `ptr` - The mapped virtual memory pointer from `mmap_uio`
/// * `offset` - Byte offset from the base of the mapped region
/// * `value` - The 32-bit value to write
///
/// # Safety
/// - The offset must point to a valid, writable 32-bit aligned location
/// - The pointer must remain valid during the write operation
/// - Hardware may respond immediately to the write
pub(crate) fn write_u32_uio(ptr: *mut u8, offset: u32, value: u32) {
    // Calculate the final virtual address: mapped_ptr + offset
    let reg_ptr = unsafe { ptr.offset(offset as isize) } as *mut u32;
    // Use volatile write to prevent compiler optimization and ensure immediate hardware update
    unsafe {
        std::ptr::write_volatile(reg_ptr, value);
    }
}

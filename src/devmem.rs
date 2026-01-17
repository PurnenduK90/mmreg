use std::fs::{File, OpenOptions};
use std::io::{self};

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

use std::ptr;

// --- Common MMAP Helper (Read/Write) ---

/// Maps a physical memory region into the process's virtual address space.
///
/// This function handles the low-level memory mapping required to access physical memory
/// through `/dev/mem`. It performs page-aligned mapping which is required by the kernel.
///
/// # Arguments
/// * `address` - The physical address to map
/// * `map_size` - The size of the region to map in bytes
///
/// # Returns
/// * `Ok((file, ptr, offset))` - Contains:
///   - `file`: Open file handle to `/dev/mem` (must be kept open while memory is mapped)
///   - `ptr`: Pointer to the mapped virtual memory
///   - `offset`: Byte offset from the mapped page start to the requested physical address
/// * `Err(String)` - Error message if mapping fails
///
/// # Safety Considerations
/// - Requires root/elevated privileges to access `/dev/mem`
/// - The returned pointer must be treated as volatile memory (use `read_volatile`/`write_volatile`)
/// - The pointer is only valid while the returned file handle is open
/// - Improper use can crash the system or corrupt kernel state
///
/// # Technical Details
/// The kernel enforces page-aligned memory mapping. This function:
/// 1. Calculates the page-aligned base address
/// 2. Computes the offset from the page start to the requested address
/// 3. Maps the page-aligned region using mmap(2)
/// 4. Returns all necessary information to access the target address
pub(crate) fn mmap_register(
    address: u64,
    map_size: usize,
) -> Result<(File, *mut u8, isize), String> {
    // Step 1: Determine system page size (typically 4096 bytes on most Linux systems)
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    if page_size == 0 {
        return Err("Could not determine system page size.".to_string());
    }
    // Create a bitmask to align addresses to page boundaries
    // Example: if page_size = 4096 (0x1000), page_mask = 0xFFFFF000
    let page_mask = !(page_size - 1);

    // Step 2: Calculate page-aligned base address and offset within that page
    let map_base = address & page_mask;
    let register_offset = (address - map_base) as isize;

    // Step 3: Open /dev/mem for read/write access
    // This file represents the entire physical memory of the system
    let file = match OpenOptions::new().read(true).write(true).open("/dev/mem") {
        Ok(f) => f,
        Err(_) => return Err("Failed to open /dev/mem. Check permissions.".to_string()),
    };
    let fd = file.as_raw_fd();

    // Step 4: Perform memory mapping using mmap(2) system call
    // - MAP_SHARED: Changes are visible to other processes (physical memory behavior)
    // - PROT_READ | PROT_WRITE: Allow both read and write access
    // - map_base as offset: Tells kernel which physical page to map
    let map_ptr = unsafe {
        libc::mmap(
            ptr::null_mut(),                    // Let kernel choose virtual address
            map_size,                           // Map this many bytes
            libc::PROT_READ | libc::PROT_WRITE, // Read and write permissions
            libc::MAP_SHARED,                   // Shared mapping (physical memory)
            fd,                                 // File descriptor of /dev/mem
            map_base as libc::off_t,            // Physical page offset
        )
    };

    // Step 5: Check if mmap succeeded (returns MAP_FAILED on error)
    if map_ptr == libc::MAP_FAILED {
        let err = io::Error::last_os_error();
        return Err(format!("mmap failed: {}", err));
    }

    // Return the file handle, virtual pointer, and byte offset to actual address
    Ok((file, map_ptr as *mut u8, register_offset))
}

/// Unmaps a previously mapped memory region.
///
/// This must be called to properly release memory resources and close the mapping.
/// Failing to unmap can cause resource leaks and prevent remapping the same region.
///
/// # Arguments
/// * `map_ptr` - The pointer returned from `mmap_register`
/// * `map_size` - The size of the mapped region (must match the size from mmap_register)
///
/// # Safety Considerations
/// - Must not be called with invalid pointers or mismatched sizes
/// - Any attempts to dereference the pointer after unmapping will cause undefined behavior
/// - This should be called before the file handle from `mmap_register` is closed
pub(crate) fn munmap_register(map_ptr: *mut u8, map_size: usize) {
    unsafe {
        // munmap(2) removes the mapping and frees kernel resources
        libc::munmap(map_ptr as *mut libc::c_void, map_size);
    }
}

/// Safely reads a 32-bit value from a memory-mapped register.
///
/// Uses volatile access to ensure the compiler doesn't optimize away the memory access.
/// This is critical for memory-mapped I/O where the value can change asynchronously
/// or where reading has side effects.
///
/// # Arguments
/// * `ptr` - The mapped virtual memory pointer from `mmap_register`
/// * `iface_offset` - The byte offset within the mapped region (from `mmap_register`)
/// * `reg_offset` - The register offset from the base address
///
/// # Returns
/// The 32-bit value read from the register
///
/// # Safety
/// - All pointers must be valid and point to readable mapped memory
/// - The calculated address must be properly aligned for a 32-bit read
pub(crate) fn read_u32_mapped(ptr: *mut u8, iface_offset: isize, reg_offset: u32) -> u32 {
    // Calculate the final virtual address: mapped_ptr + page_offset + register_offset
    let reg_ptr = unsafe { ptr.offset(iface_offset + reg_offset as isize) } as *const u32;
    // Use volatile read to prevent compiler optimization
    unsafe { std::ptr::read_volatile(reg_ptr) }
}

/// Safely writes a 32-bit value to a memory-mapped register.
///
/// Uses volatile access to ensure the compiler doesn't optimize away the memory write.
/// This is critical for memory-mapped I/O where writing has side effects (e.g., triggering hardware actions).
///
/// # Arguments
/// * `ptr` - The mapped virtual memory pointer from `mmap_register`
/// * `iface_offset` - The byte offset within the mapped region (from `mmap_register`)
/// * `reg_offset` - The register offset from the base address
/// * `value` - The 32-bit value to write
///
/// # Safety
/// - All pointers must be valid and point to writable mapped memory
/// - The calculated address must be properly aligned for a 32-bit write
/// - Hardware may respond to the write immediately, causing observable side effects
pub(crate) fn write_u32_mapped(ptr: *mut u8, iface_offset: isize, reg_offset: u32, value: u32) {
    // Calculate the final virtual address: mapped_ptr + page_offset + register_offset
    let reg_ptr = unsafe { ptr.offset(iface_offset + reg_offset as isize) } as *mut u32;
    // Use volatile write to prevent compiler optimization and ensure immediate hardware update
    unsafe {
        std::ptr::write_volatile(reg_ptr, value);
    }
}

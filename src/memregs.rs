use std::fs::{File, OpenOptions};
use std::io::{self};
use std::os::unix::io::AsRawFd;
use std::ptr;

// --- Common MMAP Helper (Read/Write) ---

/// Maps the physical memory page containing the register address into the process's virtual space.
/// Returns the file descriptor and the virtual memory pointer.
pub(crate) fn mmap_register(
    address: u64,
    map_size: usize,
) -> Result<(File, *mut u8, isize), String> {
    // 1. Calculate page alignment
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    if page_size <= 0 {
        return Err("Could not determine system page size.".to_string());
    }
    let page_mask = !(page_size - 1);

    let map_base = address & page_mask;
    let register_offset = (address - map_base) as isize;

    // 2. Open /dev/mem
    let file = match OpenOptions::new().read(true).write(true).open("/dev/mem") {
        Ok(f) => f,
        Err(_) => return Err("Failed to open /dev/mem. Check permissions.".to_string()),
    };
    let fd = file.as_raw_fd();

    // 3. Perform mmap (Memory Map)
    let map_ptr = unsafe {
        libc::mmap(
            ptr::null_mut(),
            map_size,
            libc::PROT_READ | libc::PROT_WRITE, // Required for both volatile R and W
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
pub(crate) fn munmap_register(map_ptr: *mut u8, map_size: usize) {
    unsafe {
        libc::munmap(map_ptr as *mut libc::c_void, map_size);
    }
}

/// Safely read a 32-bit value from mapped memory
pub(crate) fn read_u32_mapped(ptr: *mut u8, iface_offset: isize, reg_offset: u32) -> u32 {
    let reg_ptr = unsafe { ptr.offset(iface_offset + reg_offset as isize) } as *const u32;
    unsafe { std::ptr::read_volatile(reg_ptr) }
}

/// Safely write a 32-bit value to mapped memory
pub(crate) fn write_u32_mapped(ptr: *mut u8, iface_offset: isize, reg_offset: u32, value: u32) {
    let reg_ptr = unsafe { ptr.offset(iface_offset + reg_offset as isize) } as *mut u32;
    unsafe {
        std::ptr::write_volatile(reg_ptr, value);
    }
}

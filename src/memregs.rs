use std::fs::{File, OpenOptions};
use std::io::{self};

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

use std::ptr;

// Signal handler for SIGBUS and SIGSEGV
extern "C" fn bus_error_handler(_signal: libc::c_int) {
    // Exit the process immediately to prevent kernel hang
    unsafe {
        libc::_exit(1);
    }
}

// Set up signal handlers for SIGBUS and SIGSEGV
fn setup_bus_error_handlers() -> Result<(), String> {
    unsafe {
        let mut new_action: libc::sigaction = std::mem::zeroed();
        // Use sa_handler (not sa_sigaction) since our handler has the simple signature
        new_action.sa_sigaction = bus_error_handler as libc::sighandler_t;
        libc::sigemptyset(&mut new_action.sa_mask as *mut libc::sigset_t);
        new_action.sa_flags = 0;

        if libc::sigaction(libc::SIGBUS, &new_action, ptr::null_mut()) != 0 {
            return Err("Failed to install SIGBUS handler".to_string());
        }

        if libc::sigaction(libc::SIGSEGV, &new_action, ptr::null_mut()) != 0 {
            return Err("Failed to install SIGSEGV handler".to_string());
        }

        Ok(())
    }
}

// Check memory accessibility by forking and attempting access in child process
fn check_memory_accessible_safe(ptr: *mut u8) -> Result<(), String> {
    unsafe {
        let pid = libc::fork();

        if pid < 0 {
            return Err("Failed to fork process for memory check".to_string());
        }

        if pid == 0 {
            // Child process: try to read the memory
            // Set up alarm to prevent hanging (2 second timeout)
            libc::alarm(2);

            // Set up signal handlers to catch bus errors
            if setup_bus_error_handlers().is_err() {
                libc::_exit(1);
            }

            // Try to read the memory (testing accessibility)
            std::ptr::read_volatile(ptr as *const u32);

            // If we got here, the access succeeded
            libc::_exit(0);
        } else {
            // Parent process: wait for child
            let mut status: libc::c_int = 0;
            let wait_result = libc::waitpid(pid, &mut status, 0);

            if wait_result < 0 {
                return Err("Failed to wait for child process".to_string());
            }

            // Check if child exited normally with status 0
            if libc::WIFEXITED(status) {
                let exit_code = libc::WEXITSTATUS(status);
                if exit_code == 0 {
                    Ok(())
                } else {
                    Err(format!(
                        "Memory address 0x{:X} is not accessible. The register may not be defined or available in the device tree.",
                        ptr as u64
                    ))
                }
            } else if libc::WIFSIGNALED(status) {
                let signal = libc::WTERMSIG(status);
                Err(format!(
                    "Memory address 0x{:X} caused signal {} (bus error). The register may not be defined or available in the device tree.",
                    ptr as u64, signal
                ))
            } else {
                Err(format!(
                    "Memory address 0x{:X} check failed unexpectedly.",
                    ptr as u64
                ))
            }
        }
    }
}

// --- Common MMAP Helper (Read/Write) ---

/// Maps the physical memory page containing the register address into the process's virtual space.
/// Returns the file descriptor and the virtual memory pointer.
pub(crate) fn mmap_register(
    address: u64,
    map_size: usize,
) -> Result<(File, *mut u8, isize), String> {
    // 1. Calculate page alignment
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    if page_size == 0 {
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

/// Safely read a 32-bit value from mapped memory with bus error protection
///
/// This function uses fork-based safety checking to prevent system hangs when accessing
/// invalid memory addresses. The performance overhead is acceptable given the safety benefit
/// of preventing kernel-level hangs from bus errors on unavailable register addresses.
pub(crate) fn read_u32_mapped(
    ptr: *mut u8,
    iface_offset: isize,
    reg_offset: u32,
) -> Result<u32, String> {
    let reg_ptr = unsafe { ptr.offset(iface_offset + reg_offset as isize) };

    // Check if memory is accessible before reading (using fork)
    check_memory_accessible_safe(reg_ptr)?;

    // If check passed, perform the actual read
    let result = unsafe { std::ptr::read_volatile(reg_ptr as *const u32) };
    Ok(result)
}

/// Safely write a 32-bit value to mapped memory with bus error protection
///
/// This function uses fork-based safety checking to prevent system hangs when accessing
/// invalid memory addresses. The performance overhead is acceptable given the safety benefit
/// of preventing kernel-level hangs from bus errors on unavailable register addresses.
pub(crate) fn write_u32_mapped(
    ptr: *mut u8,
    iface_offset: isize,
    reg_offset: u32,
    value: u32,
) -> Result<(), String> {
    let reg_ptr = unsafe { ptr.offset(iface_offset + reg_offset as isize) };

    // Check if memory is accessible before writing (using fork)
    check_memory_accessible_safe(reg_ptr)?;

    // If check passed, perform the actual write
    unsafe {
        std::ptr::write_volatile(reg_ptr as *mut u32, value);
    }
    Ok(())
}

use std::fs;
use std::io::Read;
use std::path::Path;

/// Scans the sysfs UIO device directory for any UIO device that can access the given address.
///
/// UIO (Userspace I/O) devices expose hardware memory regions through `/dev/uioN` interfaces.
/// This function searches for any UIO device that has mapped the target address, which indicates
/// the address is serviceable by a UIO driver.
///
/// # Arguments
/// * `target` - The physical address to check
///
/// # Returns
/// * `Some((uio_path, device_name))` if a UIO device mapping this address is found
///   - `uio_path`: Path to the UIO device file (e.g., "/dev/uio0")
///   - `device_name`: Name of the UIO device (from sysfs)
/// * `None` if no UIO device maps this address
///
/// # Technical Details
/// For each UIO device in `/sys/class/uio/`, this function:
/// 1. Reads the device name from `name` file
/// 2. Checks the `maps/mapN/addr` and `maps/mapN/size` files for memory regions
/// 3. Verifies if the target address falls within any mapped region
pub(crate) fn check_uio_device_for_address(target: u64) -> Option<(String, String)> {
    let sysfs_uio_base = "/sys/class/uio";

    let entries = fs::read_dir(sysfs_uio_base).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();
        let uio_name = path.file_name()?.to_string_lossy().into_owned();

        // Read the device name
        let name_path = path.join("name");
        let device_name = match fs::read_to_string(&name_path) {
            Ok(content) => content.trim().to_string(),
            Err(_) => continue,
        };

        // Check each memory map in the UIO device
        let maps_path = path.join("maps");
        if let Ok(maps_entries) = fs::read_dir(&maps_path) {
            for map_entry in maps_entries.flatten() {
                let map_path = map_entry.path();

                // Check if this map contains the target address
                let addr_path = map_path.join("addr");
                let size_path = map_path.join("size");

                if let (Ok(addr_str), Ok(size_str)) =
                    (fs::read_to_string(&addr_path), fs::read_to_string(&size_path))
                {
                    if let (Ok(addr), Ok(size)) = (
                        u64::from_str_radix(addr_str.trim(), 16),
                        u64::from_str_radix(size_str.trim(), 16),
                    ) {
                        // Check if target address falls within this region
                        if target >= addr && target < (addr + size) {
                            let uio_dev_path = format!("/dev/{}", uio_name);
                            return Some((uio_dev_path, device_name));
                        }
                    }
                }
            }
        }
    }

    None
}

pub(crate) fn check_iomem(target: u64) -> bool {
    let Ok(content) = fs::read_to_string("/proc/iomem") else {
        return false;
    };

    for line in content.lines() {
        // Line format: "a0000000-a0000fff : some_device"
        if let Some((range_part, _)) = line.split_once(" : ") {
            if let Some((start_str, end_str)) = range_part.split_once('-') {
                let start = u64::from_str_radix(start_str.trim(), 16).unwrap_or(0);
                let end = u64::from_str_radix(end_str.trim(), 16).unwrap_or(0);

                if target >= start && target <= end {
                    return true;
                }
            }
        }
    }
    false
}

/// Checks a single device tree 'reg' file for a matching physical address.
///
/// Device tree 'reg' properties are stored as binary data in Big-Endian format:
/// - Bytes 0-7: Start address (u64, Big-Endian)
/// - Bytes 8-15: Region size (u64, Big-Endian)
///
/// # Arguments
/// * `reg_path` - Path to the 'reg' file (typically found at `of_node/reg` in sysfs)
/// * `target` - The physical address to check
///
/// # Returns
/// * `Some((start, size))` if the address falls within the region, `None` otherwise
pub(crate) fn check_reg_file(reg_path: &Path, target: u64) -> Option<(u64, u64)> {
    if let Ok(mut file) = fs::File::open(reg_path) {
        let mut buf = [0u8; 16];
        if file.read_exact(&mut buf).is_ok() {
            let start = u64::from_be_bytes(buf[0..8].try_into().unwrap());
            let size = u64::from_be_bytes(buf[8..16].try_into().unwrap());

            if target >= start && target < (start + size) {
                return Some((start, size));
            }
        }
    }
    None
}

/// Scans the sysfs platform devices directory for any device that owns the given address.
///
/// This function iterates through platform devices registered in `/sys/bus/platform/devices/`
/// and checks each device's device tree `reg` property to see if the target address is within
/// that device's memory region. This is particularly useful for finding custom or embedded devices
/// not listed in `/proc/iomem`.
///
/// # Arguments
/// * `target` - The physical address to locate
///
/// # Returns
/// * `Some(device_name)` if a device owning the address is found, `None` otherwise
pub(crate) fn find_owning_device_in_sysfs(target: u64) -> Option<String> {
    let devices_path = "/sys/bus/platform/devices/";
    let entries = fs::read_dir(devices_path).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();
        let reg_path = path.join("of_node/reg");

        if check_reg_file(&reg_path, target).is_some() {
            return Some(entry.file_name().to_string_lossy().into_owned());
        }
    }
    None
}

/// Verifies whether a physical address is recognized by the system.
///
/// This function performs a comprehensive check to determine if a given physical address
/// is valid and recognized by the Linux kernel. It uses two methods:
/// 1. Checks `/proc/iomem` for standard registered I/O memory resources
/// 2. Scans `/sys/bus/platform/devices/` for device-specific memory regions
///
/// # Arguments
/// * `target` - The physical address to validate
///
/// # Returns
/// * `true` if the address is recognized, `false` otherwise
///
/// # Example
/// ```ignore
/// if is_address_legit(0xdeadbeef) {
///     println!("Address is valid!");
/// } else {
///     println!("Address is not recognized by the system");
/// }
/// ```
pub(crate) fn is_address_legit(target: u64) -> bool {
    // Check standard registered resources first
    if check_iomem(target) {
        println!("Match found in /proc/iomem");
        return true;
    }

    // Check sysfs/Device Tree nodes (covers custom devices)
    if let Some(dev_name) = find_owning_device_in_sysfs(target) {
        println!("Match found in sysfs: {}", dev_name);
        return true;
    }

    false
}

/// Verifies whether a physical address is serviceable by a UIO device.
///
/// This function checks if the target address is exposed through any UIO device,
/// which indicates it can be accessed safely via the UIO interface rather than raw `/dev/mem`.
///
/// # Arguments
/// * `target` - The physical address to check
///
/// # Returns
/// * `Some((uio_path, device_name))` if the address is serviceable by a UIO device
/// * `None` if no UIO device services this address
///
/// # Example
/// ```ignore
/// if let Some((path, name)) = is_address_serviceable_by_uio(0xa0000000) {
///     println!("Address {} is serviceable by UIO device {}", path, name);
/// }
/// ```
pub(crate) fn is_address_serviceable_by_uio(target: u64) -> Option<(String, String)> {
    check_uio_device_for_address(target)
}

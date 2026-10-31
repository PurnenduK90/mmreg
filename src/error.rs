use std::fmt;
use std::path::PathBuf;

/// Structured error codes and diagnostics for `mmreg`.
///
/// Every variant produces a deterministic `[E_*]` prefix, context, and actionable `Remediation:`
/// guidance when formatted via `Display` or converted into a `String`. New variants may be added in
/// minor releases, so matches need a wildcard arm; match on [`MmregError::code`] for stable handling.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MmregError {
    /// Target physical address range is absent from `/sys/class/uio`, `/proc/iomem`, and Device Tree `reg`.
    BlueprintUnmapped { address: u64, size: usize },
    /// Target physical address range falls inside System RAM, kernel code/data, or reserved memory in `/proc/iomem`.
    BlueprintSystemRam {
        address: u64,
        size: usize,
        resource: String,
    },
    /// Owning device has a non-okay Device Tree `of_node/status` (e.g., `"disabled"` or `"fail"`).
    ZombieDtStatus {
        device_name: String,
        status: String,
        status_path: PathBuf,
    },
    /// Owning device's runtime power management (`power/runtime_status`) is `"suspended"`, `"suspending"`, or `"error"`.
    ZombieRuntimePm {
        device_name: String,
        status: String,
        pm_path: PathBuf,
        device_path: PathBuf,
    },
    /// Upstream clock/power/reset supplier (`supplier:*`) is `"suspended"`, `"suspending"`, or `"error"`.
    ZombieSupplierPm {
        device_name: String,
        supplier_name: String,
        status: String,
        supplier_pm_path: PathBuf,
    },
    /// Target device resides on an FPGA fabric whose FPGA manager `state` is not `"operating"`.
    ZombieFpgaState {
        device_name: String,
        manager_name: String,
        state: String,
        state_path: PathBuf,
    },
    /// Target device's clock in Linux Common Clock Framework (`/sys/kernel/debug/clk/clk_summary`) is disabled in hardware.
    ZombieClockGated {
        device_name: String,
        clock_name: String,
        clk_path: PathBuf,
    },
    /// A declarative pre-flight validation hook check (`--validate <file>`) failed.
    ValidationHookFailed {
        profile_name: String,
        check_name: String,
        address: u64,
        mask: u32,
        expected: u32,
        actual: u32,
    },
    /// Failed to read or parse a declarative validation profile JSON file.
    ValidationProfileParse { details: String },
    /// Failed to open `/dev/mem`.
    DevMemOpen { address: u64, details: String },
    /// `mmap(2)` failed on `/dev/mem`.
    DevMemMmap {
        address: u64,
        page_base: u64,
        size: usize,
        details: String,
    },
    /// Failed to open `/dev/uioN`.
    UioOpen {
        device_path: String,
        details: String,
    },
    /// `mmap(2)` failed on `/dev/uioN`.
    UioMmap {
        device_path: String,
        region_index: usize,
        size: usize,
        details: String,
    },
    /// Address range is not inside the UIO region, or the region's sysfs `addr`/`size` cannot be read.
    UioAddressInvalid {
        device_path: String,
        region_index: usize,
        address: u64,
        details: String,
    },
    /// Base address or register offset is not 4-byte aligned.
    UnalignedAccess { address: u64, offset: u32 },
    /// Register access exceeds the mapped `Interface` size window.
    OutOfBoundsAccess {
        register_name: String,
        offset: u32,
        access_size: usize,
        region_size: usize,
    },
    /// Failed to open inter-process lock file.
    LockOpen { lock_path: PathBuf, details: String },
    /// Failed to acquire exclusive lock on lock file.
    LockAcquire { details: String },
    /// Requested register name was not found on the `Interface`.
    RegisterNotFound {
        interface_name: String,
        register_name: String,
        available: Vec<String>,
    },
    /// Requested subregister (bitfield) name was not found on the `Register`.
    SubRegisterNotFound {
        interface_name: String,
        register_name: String,
        subregister_name: String,
        available: Vec<String>,
    },
    /// Subregister bit range is invalid (`msb < lsb` or `msb > 31`).
    InvalidSubRegister {
        register_name: String,
        subregister_name: String,
        msb: u8,
        lsb: u8,
    },
    /// The address is in no UIO region and this build has no `/dev/mem` access (`debug` feature).
    DebugDisabled { address: u64 },
}

impl MmregError {
    /// Returns the stable machine-readable diagnostic error code for this error.
    pub fn code(&self) -> &'static str {
        match self {
            MmregError::BlueprintUnmapped { .. } => "E_BLUEPRINT_UNMAPPED",
            MmregError::BlueprintSystemRam { .. } => "E_BLUEPRINT_RAM",
            MmregError::ZombieDtStatus { .. } => "E_ZOMBIE_DT_STATUS",
            MmregError::ZombieRuntimePm { .. } => "E_ZOMBIE_RUNTIME_PM",
            MmregError::ZombieSupplierPm { .. } => "E_ZOMBIE_SUPPLIER_PM",
            MmregError::ZombieFpgaState { .. } => "E_ZOMBIE_FPGA_STATE",
            MmregError::ZombieClockGated { .. } => "E_ZOMBIE_CLOCK_GATED",
            MmregError::ValidationHookFailed { .. } => "E_VALIDATION_HOOK_FAILED",
            MmregError::ValidationProfileParse { .. } => "E_VALIDATION_PROFILE_PARSE",
            MmregError::DevMemOpen { .. } => "E_DEVMEM_OPEN",
            MmregError::DevMemMmap { .. } => "E_DEVMEM_MMAP",
            MmregError::UioOpen { .. } => "E_UIO_OPEN",
            MmregError::UioMmap { .. } => "E_UIO_MMAP",
            MmregError::UioAddressInvalid { .. } => "E_UIO_ADDRESS",
            MmregError::UnalignedAccess { .. } => "E_UNALIGNED_ACCESS",
            MmregError::OutOfBoundsAccess { .. } => "E_OUT_OF_BOUNDS",
            MmregError::LockOpen { .. } => "E_LOCK_OPEN",
            MmregError::LockAcquire { .. } => "E_LOCK_ACQUIRE",
            MmregError::RegisterNotFound { .. } => "E_REG_NOT_FOUND",
            MmregError::SubRegisterNotFound { .. } => "E_SUBREG_NOT_FOUND",
            MmregError::InvalidSubRegister { .. } => "E_SUBREG_RANGE",
            MmregError::DebugDisabled { .. } => "E_DEBUG_DISABLED",
        }
    }
}

impl fmt::Display for MmregError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self.code();
        match self {
            MmregError::BlueprintUnmapped { address, size } => write!(
                f,
                "[{}] Address 0x{:X} (size {} bytes) is not recognized in any system blueprint \
                 (/sys/class/uio, /sys/bus/platform/devices/*/of_node/reg, or /proc/iomem). \
                 Remediation: 1) Run with `sudo` if /proc/iomem addresses are hidden (00000000-00000000) for non-root users; \
                 2) Verify the peripheral's Device Tree node or UIO driver is loaded; \
                 3) Pass --force (`with_force(true)`) only if you are certain the hardware bus will respond.",
                code, address, size
            ),
            MmregError::BlueprintSystemRam {
                address,
                size,
                resource,
            } => write!(
                f,
                "[{}] Address 0x{:X} (size {} bytes) matches restricted memory resource '{}' in /proc/iomem. \
                 Refusing to map system RAM, kernel memory, or reserved ranges as MMIO. \
                 Remediation: Verify the peripheral's physical address, or pass --force (`with_force(true)`) only if intentionally accessing this range.",
                code, address, size, resource
            ),
            MmregError::ZombieDtStatus {
                device_name,
                status,
                status_path,
            } => write!(
                f,
                "[{}] Hardware Pre-Flight failed (Zombie Hardware): Device '{}' has Device Tree status '{}' at '{}' (expected 'okay'). \
                 Remediation: Enable the peripheral in the Device Tree (`status = \"okay\";`) or pass --force (`with_force(true)`) to override.",
                code,
                device_name,
                status,
                status_path.display()
            ),
            MmregError::ZombieRuntimePm {
                device_name,
                status,
                pm_path,
                device_path,
            } => write!(
                f,
                "[{}] Hardware Pre-Flight failed (Zombie Hardware): Device '{}' power/clock domain is '{}' at '{}'. \
                 Accessing a suspended peripheral can hang the AXI/system bus. \
                 Remediation: Wake the device (`echo on | sudo tee {}/power/control`), ensure its driver has active clocks, or pass --force to override.",
                code,
                device_name,
                status,
                pm_path.display(),
                device_path.display()
            ),
            MmregError::ZombieSupplierPm {
                device_name,
                supplier_name,
                status,
                supplier_pm_path,
            } => write!(
                f,
                "[{}] Hardware Pre-Flight failed (Zombie Hardware): Upstream clock/power supplier '{}' for device '{}' is '{}' at '{}'. \
                 Remediation: Ensure upstream clock/power controller is active before accessing '{}', or pass --force to override.",
                code,
                supplier_name,
                device_name,
                status,
                supplier_pm_path.display(),
                device_name
            ),
            MmregError::ZombieFpgaState {
                device_name,
                manager_name,
                state,
                state_path,
            } => write!(
                f,
                "[{}] Hardware Pre-Flight failed (Zombie Hardware): Device '{}' resides on the FPGA fabric, \
                 but FPGA manager '{}' is in state '{}' at '{}' (expected 'operating'). \
                 Remediation: Load the FPGA bitstream first (e.g., via `fpgautil -b <bitstream>`), or pass --force to override.",
                code,
                device_name,
                manager_name,
                state,
                state_path.display()
            ),
            MmregError::ZombieClockGated {
                device_name,
                clock_name,
                clk_path,
            } => write!(
                f,
                "[{}] Hardware Pre-Flight failed (Zombie Hardware): Device '{}' clock '{}' is gated in hardware at '{}'. \
                 Accessing a peripheral with a gated clock can cause an AXI bus hang or kernel RCU stall. \
                 Remediation: Enable the peripheral clock via its driver/device tree before accessing registers, or pass --force to override.",
                code,
                device_name,
                clock_name,
                clk_path.display()
            ),
            MmregError::ValidationHookFailed {
                profile_name,
                check_name,
                address,
                mask,
                expected,
                actual,
            } => write!(
                f,
                "[{}] Pre-Flight Safety Profile '{}' failed on check '{}' at address 0x{:X}: \
                 (actual 0x{:08X} & mask 0x{:08X}) == 0x{:08X}, expected 0x{:08X}. \
                 Remediation: Ensure hardware prerequisite '{}' is satisfied before accessing target bus.",
                code,
                profile_name,
                check_name,
                address,
                actual,
                mask,
                actual & mask,
                expected & mask,
                check_name
            ),
            MmregError::ValidationProfileParse { details } => write!(
                f,
                "[{}] Invalid Safety Profile: {}. \
                 Remediation: Provide a valid JSON profile with `\"name\": \"...\"` and `\"checks\": [...]` objects (`name`, `addr`, `mask`, `expected`).",
                code, details
            ),
            MmregError::DevMemOpen { address, details } => write!(
                f,
                "[{}] Failed to open /dev/mem for address 0x{:X}: {}. \
                 Remediation: Run with sudo or grant CAP_SYS_RAWIO (`sudo setcap cap_sys_rawio+ep $(which mmreg)`), \
                 or bind the peripheral to a UIO driver (/dev/uioN).",
                code, address, details
            ),
            MmregError::DevMemMmap {
                address,
                page_base,
                size,
                details,
            } => write!(
                f,
                "[{}] mmap(/dev/mem) failed for address 0x{:X} (page base 0x{:X}, size {} bytes): {}. \
                 Remediation: If EPERM/Operation not permitted, the kernel has CONFIG_STRICT_DEVMEM enabled — expose the device via UIO (/dev/uioN) instead.",
                code, address, page_base, size, details
            ),
            MmregError::UioOpen {
                device_path,
                details,
            } => write!(
                f,
                "[{}] Failed to open UIO device '{}': {}. \
                 Remediation: Verify the UIO device exists (`ls -l {}`) and current user has read/write permissions (or udev rule).",
                code, device_path, details, device_path
            ),
            MmregError::UioMmap {
                device_path,
                region_index,
                size,
                details,
            } => write!(
                f,
                "[{}] mmap failed for UIO device '{}' (region map{}, requested size {} bytes): {}. \
                 Remediation: Check `/sys/class/uio/*/maps/map{}/size` to ensure requested size does not exceed region size.",
                code, device_path, region_index, size, details, region_index
            ),
            MmregError::UioAddressInvalid {
                device_path,
                region_index,
                address,
                details,
            } => write!(
                f,
                "[{}] Invalid UIO address 0x{:X} for device '{}' (region map{}): {}. \
                 Remediation: Keep the whole Interface range inside the region given by `/sys/class/uio/<uioN>/maps/map{}/addr` and `size`, or use MapperType::Auto.",
                code, address, device_path, region_index, details, region_index
            ),
            MmregError::UnalignedAccess { address, offset } => write!(
                f,
                "[{}] Unaligned 32-bit register access at base 0x{:X} + offset 0x{:X} (effective address 0x{:X}). \
                 Remediation: Ensure both base address and register offsets are multiples of 4 bytes.",
                code,
                address,
                offset,
                address.wrapping_add(*offset as u64)
            ),
            MmregError::OutOfBoundsAccess {
                register_name,
                offset,
                access_size,
                region_size,
            } => write!(
                f,
                "[{}] Out-of-bounds access for register '{}' at offset 0x{:X} ({}-byte access exceeds mapped region size of {} bytes). \
                 Remediation: Increase the Interface `size` so `offset + {} <= size`.",
                code, register_name, offset, access_size, region_size, access_size
            ),
            MmregError::LockOpen { lock_path, details } => write!(
                f,
                "[{}] Failed to open inter-process lock file '{}': {}. Remediation: {}",
                code,
                lock_path.display(),
                details,
                // UIO locks the device file itself; /dev/mem and custom mappers use page lock files.
                if lock_path.starts_with("/dev") {
                    "Give the user read access to the UIO device (udev rule or group membership)."
                } else {
                    "Check permissions on the lock file or pre-create the lock directory \
                     (`sudo mkdir -m 1777 -p /run/lock/mmreg`)."
                }
            ),
            MmregError::LockAcquire { details } => write!(
                f,
                "[{}] Failed to acquire exclusive inter-process lock: {}.",
                code, details
            ),
            MmregError::RegisterNotFound {
                interface_name,
                register_name,
                available,
            } => write!(
                f,
                "[{}] Register '{}' not found in interface '{}'. Available registers: [{}]",
                code,
                register_name,
                interface_name,
                available.join(", ")
            ),
            MmregError::SubRegisterNotFound {
                interface_name,
                register_name,
                subregister_name,
                available,
            } => write!(
                f,
                "[{}] SubRegister '{}' not found in Register '{}' (interface '{}'). Available subregisters: [{}]",
                code,
                subregister_name,
                register_name,
                interface_name,
                available.join(", ")
            ),
            MmregError::InvalidSubRegister {
                register_name,
                subregister_name,
                msb,
                lsb,
            } => write!(
                f,
                "[{}] SubRegister '{}' of Register '{}' has invalid bit range [{}:{}]. \
                 Remediation: Use 31 >= msb >= lsb.",
                code, subregister_name, register_name, msb, lsb
            ),
            MmregError::DebugDisabled { address } => write!(
                f,
                "[{}] Address 0x{:X} is not inside any UIO region, and this mmreg build has no /dev/mem access. \
                 Remediation: Bind the peripheral to UIO (`compatible = \"generic-uio\"`), or use an mmreg built with the `debug` feature (development images only).",
                code, address
            ),
        }
    }
}

impl std::error::Error for MmregError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_error_codes_and_display_formatting() {
        let cases: Vec<(MmregError, &str)> = vec![
            (
                MmregError::BlueprintUnmapped {
                    address: 0x4000_0000,
                    size: 4,
                },
                "E_BLUEPRINT_UNMAPPED",
            ),
            (
                MmregError::BlueprintSystemRam {
                    address: 0x0010_0000,
                    size: 4,
                    resource: "System RAM".into(),
                },
                "E_BLUEPRINT_RAM",
            ),
            (
                MmregError::ZombieDtStatus {
                    device_name: "uart0".into(),
                    status: "disabled".into(),
                    status_path: PathBuf::from("/sys/devices/uart0/of_node/status"),
                },
                "E_ZOMBIE_DT_STATUS",
            ),
            (
                MmregError::ZombieRuntimePm {
                    device_name: "i2c0".into(),
                    status: "suspended".into(),
                    pm_path: PathBuf::from("/sys/devices/i2c0/power/runtime_status"),
                    device_path: PathBuf::from("/sys/devices/i2c0"),
                },
                "E_ZOMBIE_RUNTIME_PM",
            ),
            (
                MmregError::ZombieSupplierPm {
                    device_name: "spi0".into(),
                    supplier_name: "supplier:clk0".into(),
                    status: "suspended".into(),
                    supplier_pm_path: PathBuf::from("/sys/devices/clk0/power/runtime_status"),
                },
                "E_ZOMBIE_SUPPLIER_PM",
            ),
            (
                MmregError::ZombieFpgaState {
                    device_name: "axi_gpio".into(),
                    manager_name: "fpga0".into(),
                    state: "power off".into(),
                    state_path: PathBuf::from("/sys/class/fpga_manager/fpga0/state"),
                },
                "E_ZOMBIE_FPGA_STATE",
            ),
            (
                MmregError::ZombieClockGated {
                    device_name: "axi_gpio".into(),
                    clock_name: "pl0_ref".into(),
                    clk_path: PathBuf::from("/sys/kernel/debug/clk/clk_summary"),
                },
                "E_ZOMBIE_CLOCK_GATED",
            ),
            (
                MmregError::ValidationHookFailed {
                    profile_name: "ZynqMP_AXI_Safety".into(),
                    check_name: "PCAP_DONE".into(),
                    address: 0xFFCA_3010,
                    mask: 0x4,
                    expected: 0x4,
                    actual: 0x0,
                },
                "E_VALIDATION_HOOK_FAILED",
            ),
            (
                MmregError::ValidationProfileParse {
                    details: "missing checks array".into(),
                },
                "E_VALIDATION_PROFILE_PARSE",
            ),
            (
                MmregError::DevMemOpen {
                    address: 0x4000_0000,
                    details: "Permission denied".into(),
                },
                "E_DEVMEM_OPEN",
            ),
            (
                MmregError::DevMemMmap {
                    address: 0x4000_0000,
                    page_base: 0x4000_0000,
                    size: 4096,
                    details: "Operation not permitted".into(),
                },
                "E_DEVMEM_MMAP",
            ),
            (
                MmregError::UioOpen {
                    device_path: "/dev/uio0".into(),
                    details: "No such file".into(),
                },
                "E_UIO_OPEN",
            ),
            (
                MmregError::UioMmap {
                    device_path: "/dev/uio0".into(),
                    region_index: 0,
                    size: 4096,
                    details: "Invalid argument".into(),
                },
                "E_UIO_MMAP",
            ),
            (
                MmregError::UioAddressInvalid {
                    device_path: "/dev/uio0".into(),
                    region_index: 0,
                    address: 0xA000_0000,
                    details: "outside region".into(),
                },
                "E_UIO_ADDRESS",
            ),
            (
                MmregError::UnalignedAccess {
                    address: 0x4000_0002,
                    offset: 0,
                },
                "E_UNALIGNED_ACCESS",
            ),
            (
                MmregError::OutOfBoundsAccess {
                    register_name: "ctrl".into(),
                    offset: 16,
                    access_size: 4,
                    region_size: 16,
                },
                "E_OUT_OF_BOUNDS",
            ),
            (
                MmregError::LockOpen {
                    lock_path: PathBuf::from(
                        "/run/lock/mmreg/0000000040000000-0000000040001000.lock",
                    ),
                    details: "Read-only filesystem".into(),
                },
                "E_LOCK_OPEN",
            ),
            (
                MmregError::LockAcquire {
                    details: "Resource deadlock".into(),
                },
                "E_LOCK_ACQUIRE",
            ),
            (
                MmregError::RegisterNotFound {
                    interface_name: "axi".into(),
                    register_name: "bad".into(),
                    available: vec!["ctrl".into(), "status".into()],
                },
                "E_REG_NOT_FOUND",
            ),
            (
                MmregError::SubRegisterNotFound {
                    interface_name: "axi".into(),
                    register_name: "ctrl".into(),
                    subregister_name: "bad_sub".into(),
                    available: vec!["enable".into()],
                },
                "E_SUBREG_NOT_FOUND",
            ),
            (
                MmregError::InvalidSubRegister {
                    register_name: "ctrl".into(),
                    subregister_name: "bad".into(),
                    msb: 2,
                    lsb: 5,
                },
                "E_SUBREG_RANGE",
            ),
            (
                MmregError::DebugDisabled {
                    address: 0x4000_0000,
                },
                "E_DEBUG_DISABLED",
            ),
        ];

        for (err, expected_code) in cases {
            assert_eq!(err.code(), expected_code);
            assert!(err.to_string().starts_with(&format!("[{}]", expected_code)));
        }
    }

    #[test]
    fn test_lock_open_remediation_matches_lock_kind() {
        let msg = |path: &str| {
            MmregError::LockOpen {
                lock_path: PathBuf::from(path),
                details: "Permission denied".into(),
            }
            .to_string()
        };
        assert!(msg("/dev/uio0").contains("read access to the UIO device"));
        assert!(msg("/run/lock/mmreg/0-1000.lock").contains("mkdir -m 1777"));
    }
}

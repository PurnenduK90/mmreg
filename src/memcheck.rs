//! Blueprint validation (is the range a known device, not RAM?) and Zombie Hardware Pre-Flight
//! (is the owning device enabled, powered, clocked and, on an FPGA, programmed?).

use crate::error::MmregError;
use crate::memregs::MapperType;
use crate::parse::parse_u64_radix;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// How to reach an address range and which sysfs device owns it.
pub(crate) struct Target {
    /// `MapperType::Uio` when a UIO region covers the range, `MapperType::DevMem` otherwise.
    pub(crate) mapper: MapperType,
    /// Sysfs directory of the owning device, when one is found.
    pub(crate) owner: Option<PathBuf>,
}

/// Finds the UIO region or platform device owning `[target, target + size)`. With `check`, also
/// refuses RAM and unknown ranges (Blueprint) and runs the Hardware Pre-Flight on the owner.
pub(crate) fn resolve(target: u64, size: usize, check: bool) -> Result<Target, MmregError> {
    let fpga_mgr_root = Path::new("/sys/class/fpga_manager");

    if let Some(uio) = find_uio_region(Path::new("/sys/class/uio"), Path::new("/dev"), target, size)
    {
        if check {
            preflight_check_device(&uio.device_name, &uio.device_path, fpga_mgr_root)?;
        }
        return Ok(Target {
            mapper: MapperType::Uio {
                device_path: uio.dev_node,
                region_index: uio.region_index,
            },
            owner: Some(uio.device_path),
        });
    }

    let owner = find_owning_device(Path::new("/sys/bus/platform/devices"), target, size);
    if check {
        let in_iomem = check_iomem(target, size)?;
        match &owner {
            Some((name, path)) => preflight_check_device(name, path, fpga_mgr_root)?,
            None if !in_iomem => {
                return Err(MmregError::BlueprintUnmapped {
                    address: target,
                    size,
                });
            }
            None => {}
        }
    }
    Ok(Target {
        mapper: MapperType::DevMem,
        owner: owner.map(|(_, path)| path),
    })
}

fn starts_with_ignore_case(s: &str, prefix: &str) -> bool {
    s.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// Sysfs device-link entries naming a supplier (`supplier:<bus>:<dev>`; `supplier_` in tests,
/// since `:` is not allowed in Windows file names).
fn is_supplier_link(name: &str) -> bool {
    name.starts_with("supplier:") || name.starts_with("supplier_")
}

// --- UIO ---

/// One `/sys/class/uio/uioN/maps/mapM` region: physical `addr`, where the region's mmap starts,
/// and `size`, counted from `addr`.
pub(crate) struct UioMap {
    pub(crate) addr: u64,
    pub(crate) size: u64,
}

impl UioMap {
    pub(crate) fn read(map_dir: &Path) -> Option<Self> {
        let attr = |name: &str| {
            fs::read_to_string(map_dir.join(name))
                .ok()
                .and_then(|s| parse_u64_radix(&s, 16).ok())
        };
        Some(UioMap {
            addr: attr("addr")?,
            size: attr("size")?,
        })
    }

    /// True when `[address, address + len)` lies inside the region.
    pub(crate) fn contains(&self, address: u64, len: usize) -> bool {
        address >= self.addr
            && (address - self.addr)
                .checked_add(len as u64)
                .is_some_and(|end| end <= self.size)
    }
}

/// A UIO region covering an address range.
struct UioRegion {
    /// Device node, e.g. `/dev/uio0`
    dev_node: String,
    /// `M` of `maps/mapM`
    region_index: usize,
    /// Name of the owning device's sysfs directory (as used by `clk_summary`)
    device_name: String,
    /// Owning device's sysfs directory (`uioN/device`, resolved)
    device_path: PathBuf,
}

fn find_uio_region(
    sysfs_uio: &Path,
    dev_root: &Path,
    target: u64,
    size: usize,
) -> Option<UioRegion> {
    for uio in fs::read_dir(sysfs_uio).ok()?.flatten() {
        let Ok(maps) = fs::read_dir(uio.path().join("maps")) else {
            continue;
        };
        for map in maps.flatten() {
            let Some(region_index) = map
                .file_name()
                .to_str()
                .and_then(|n| n.strip_prefix("map"))
                .and_then(|i| i.parse().ok())
            else {
                continue;
            };
            if !UioMap::read(&map.path()).is_some_and(|r| r.contains(target, size)) {
                continue;
            }
            let device_path =
                fs::canonicalize(uio.path().join("device")).unwrap_or_else(|_| uio.path());
            let device_name = device_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            return Some(UioRegion {
                dev_node: dev_root
                    .join(uio.file_name())
                    .to_string_lossy()
                    .into_owned(),
                region_index,
                device_name,
                device_path,
            });
        }
    }
    None
}

// --- /proc/iomem ---

/// `/proc/iomem` resources that are memory, not MMIO (case-insensitive prefixes).
const RESTRICTED_IOMEM: [&str; 5] = ["system ram", "kernel ", "crash kernel", "reserved", "acpi "];

/// Returns whether `/proc/iomem` lists `[target, target + size)`, or an error when the range is
/// inside RAM, kernel memory or a reserved range. An unreadable `/proc/iomem` lists nothing.
fn check_iomem(target: u64, size: usize) -> Result<bool, MmregError> {
    match File::open("/proc/iomem") {
        Ok(file) => parse_iomem(BufReader::new(file), target, size),
        Err(_) => Ok(false),
    }
}

fn parse_iomem<R: BufRead>(mut reader: R, target: u64, size: usize) -> Result<bool, MmregError> {
    let last = target.saturating_add(size.max(1) as u64 - 1);
    let mut listed = false;
    let mut line = String::with_capacity(128);

    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        if let Some((range, resource)) = line.split_once(" : ")
            && let Some((start, end)) = range.trim().split_once('-')
            && let (Ok(start), Ok(end)) = (
                u64::from_str_radix(start, 16),
                u64::from_str_radix(end, 16),
            )
            // Non-root readers see every range as 00000000-00000000.
            && end != 0
            && target >= start
            && last <= end
        {
            let resource = resource.trim();
            if RESTRICTED_IOMEM
                .iter()
                .any(|p| starts_with_ignore_case(resource, p))
            {
                return Err(MmregError::BlueprintSystemRam {
                    address: target,
                    size,
                    resource: resource.to_string(),
                });
            }
            listed = true;
        }
        line.clear();
    }
    Ok(listed)
}

// --- Device Tree `reg` ---

/// Reads a `#address-cells` / `#size-cells` property (big-endian `u32`, 1..=4).
fn read_dt_cells(path: &Path) -> Option<usize> {
    let bytes = fs::read(path).ok()?;
    let cells = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
    (1..=4).contains(&cells).then_some(cells)
}

fn read_be_cells(bytes: &[u8]) -> u64 {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .fold(0, |acc, &w| (acc << 32) | u64::from(u32::from_be_bytes(w)))
}

/// True when a raw Device Tree `reg` property has an `(address, size)` entry covering
/// `[target, target + size)`. Without `cells`, every layout dividing the length is tried.
fn reg_covers(buf: &[u8], target: u64, size: usize, cells: Option<(usize, usize)>) -> bool {
    let end = target.saturating_add(size.max(1) as u64);
    let guesses = [(2, 2), (1, 1), (2, 1)];
    let layouts = match &cells {
        Some(c) => std::slice::from_ref(c),
        None => &guesses[..],
    };
    layouts.iter().any(|&(addr_cells, size_cells)| {
        let entry = (addr_cells + size_cells) * 4;
        buf.len().is_multiple_of(entry)
            && buf.chunks_exact(entry).any(|e| {
                let start = read_be_cells(&e[..addr_cells * 4]);
                let len = read_be_cells(&e[addr_cells * 4..]);
                len > 0 && target >= start && end <= start.saturating_add(len)
            })
    })
}

/// Checks a device's `of_node/reg`, using its parent node's cell sizes when readable.
fn reg_file_covers(reg_path: &Path, target: u64, size: usize) -> bool {
    let Ok(buf) = fs::read(reg_path) else {
        return false;
    };
    let cells = reg_path
        .parent()
        .and_then(|of_node| fs::canonicalize(of_node).ok())
        .and_then(|node| {
            let parent = node.parent()?;
            Some((
                read_dt_cells(&parent.join("#address-cells"))?,
                read_dt_cells(&parent.join("#size-cells"))?,
            ))
        });
    reg_covers(&buf, target, size, cells)
}

/// Finds the platform device whose Device Tree `reg` covers the range: `(name, sysfs path)`.
fn find_owning_device(devices: &Path, target: u64, size: usize) -> Option<(String, PathBuf)> {
    fs::read_dir(devices)
        .ok()?
        .flatten()
        .find(|dev| reg_file_covers(&dev.path().join("of_node/reg"), target, size))
        .map(|dev| (dev.file_name().to_string_lossy().into_owned(), dev.path()))
}

// --- Hardware Pre-Flight ---

/// Path components naming an FPGA (programmable logic) bus (case-insensitive prefixes).
const FPGA_BUS_PREFIXES: [&str; 7] = [
    "amba_pl",
    "axi_pl",
    "pl-bus",
    "fpga-region",
    "fpga_region",
    "fpga-full",
    "fpga-bus",
];

fn path_has_fpga_component(p: &Path) -> bool {
    p.components()
        .filter_map(|c| c.as_os_str().to_str())
        .any(|s| {
            FPGA_BUS_PREFIXES
                .iter()
                .any(|f| starts_with_ignore_case(s, f))
        })
}

/// True when a sysfs device sits on, or is supplied by, an FPGA fabric.
fn is_fpga_backed_device(dev_path: &Path) -> bool {
    path_has_fpga_component(dev_path)
        || fs::canonicalize(dev_path).is_ok_and(|p| path_has_fpga_component(&p))
        || fs::canonicalize(dev_path.join("of_node")).is_ok_and(|p| path_has_fpga_component(&p))
        || fs::read_dir(dev_path).is_ok_and(|entries| {
            entries.flatten().any(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|s| is_supplier_link(s) && s.to_ascii_lowercase().contains("fpga"))
            })
        })
}

fn is_runtime_pm_bad(pm: &str) -> bool {
    ["suspended", "suspending", "error"]
        .iter()
        .any(|bad| pm.eq_ignore_ascii_case(bad))
}

/// Runs the Zombie Hardware Pre-Flight checks on a sysfs device.
fn preflight_check_device(
    dev_name: &str,
    dev_path: &Path,
    fpga_mgr_root: &Path,
) -> Result<(), MmregError> {
    // 1. Device Tree status
    let status_path = dev_path.join("of_node/status");
    if let Ok(raw) = fs::read_to_string(&status_path) {
        let status = raw.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        if !status.is_empty()
            && !status.eq_ignore_ascii_case("okay")
            && !status.eq_ignore_ascii_case("ok")
        {
            return Err(MmregError::ZombieDtStatus {
                device_name: dev_name.to_string(),
                status: status.to_string(),
                status_path,
            });
        }
    }

    // 2. Runtime power management
    let pm_path = dev_path.join("power/runtime_status");
    if let Ok(raw) = fs::read_to_string(&pm_path)
        && is_runtime_pm_bad(raw.trim())
    {
        return Err(MmregError::ZombieRuntimePm {
            device_name: dev_name.to_string(),
            status: raw.trim().to_string(),
            pm_path,
            device_path: dev_path.to_path_buf(),
        });
    }

    // 3. Supplier device links (clocks, resets, regulators, power domains)
    if let Ok(entries) = fs::read_dir(dev_path) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str().filter(|n| is_supplier_link(n)) else {
                continue;
            };
            let link = entry.path();
            let supplier = link.join("supplier");
            let root = if supplier.exists() { supplier } else { link };
            let pm_path = root.join("power/runtime_status");
            if let Ok(raw) = fs::read_to_string(&pm_path)
                && is_runtime_pm_bad(raw.trim())
            {
                return Err(MmregError::ZombieSupplierPm {
                    device_name: dev_name.to_string(),
                    supplier_name: name.to_string(),
                    status: raw.trim().to_string(),
                    supplier_pm_path: pm_path,
                });
            }
        }
    }

    // 4. Common Clock Framework (debugfs)
    check_device_clocks(dev_name, dev_path, Path::new("/sys/kernel/debug/clk"))?;

    // 5. FPGA manager, only for devices on the FPGA fabric
    if is_fpga_backed_device(dev_path) {
        check_fpga_manager_operating(dev_name, fpga_mgr_root)?;
    }

    Ok(())
}

/// Checks the device's clocks in Common Clock Framework debugfs by their hardware-enable state
/// (`clk_summary`), warning instead of refusing when only the software enable count is known.
fn check_device_clocks(
    dev_name: &str,
    dev_path: &Path,
    clk_debug_root: &Path,
) -> Result<(), MmregError> {
    if !clk_debug_root.exists() {
        return Ok(());
    }

    let dt_clock_names: Vec<String> = fs::read(dev_path.join("of_node/clock-names"))
        .map(|raw| {
            raw.split(|&b| b == 0)
                .filter_map(|part| std::str::from_utf8(part).ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let summary_path = clk_debug_root.join("clk_summary");
    if let Ok(file) = File::open(&summary_path) {
        return check_clk_summary(
            BufReader::new(file),
            dev_name,
            &dt_clock_names,
            &summary_path,
        );
    }

    for clk_name in &dt_clock_names {
        let count_path = clk_debug_root.join(clk_name).join("clk_enable_count");
        if fs::read_to_string(&count_path).is_ok_and(|raw| raw.trim() == "0") {
            eprintln!(
                "Warning: Clock '{}' for device '{}' has software clk_enable_count == 0 at '{}' (hardware-enable state unavailable).",
                clk_name,
                dev_name,
                count_path.display()
            );
        }
    }
    Ok(())
}

/// Parses a `clk_summary` clock row: `(enable_count, hardware_enable)`. Header and consumer-only
/// lines return `None`.
fn parse_clk_summary_row(tokens: &[&str]) -> Option<(u32, Option<char>)> {
    let enable_count = tokens.get(1)?.parse().ok()?;
    let _prepare_count: u32 = tokens.get(2)?.parse().ok()?;
    let hw_enable = tokens
        .iter()
        .skip(3)
        .take(7)
        .find(|t| matches!(**t, "Y" | "N" | "-"))
        .and_then(|t| t.chars().next());
    Some((enable_count, hw_enable))
}

fn evaluate_clock_state(
    dev_name: &str,
    clk_name: &str,
    (enable_count, hw_enable): (u32, Option<char>),
    summary_path: &Path,
) -> Result<(), MmregError> {
    match hw_enable {
        Some('N') => Err(MmregError::ZombieClockGated {
            device_name: dev_name.to_string(),
            clock_name: clk_name.to_string(),
            clk_path: summary_path.to_path_buf(),
        }),
        Some('Y') => Ok(()),
        _ => {
            if enable_count == 0 {
                eprintln!(
                    "Warning: Clock '{}' for device '{}' has enable_count == 0 in '{}' (hardware-enable column unavailable).",
                    clk_name,
                    dev_name,
                    summary_path.display()
                );
            }
            Ok(())
        }
    }
}

/// Scans `clk_summary` for the device's clocks: rows naming one of its DT clocks or the device as
/// consumer, and consumer-only lines (newer kernels) below a clock row.
fn check_clk_summary<R: BufRead>(
    mut reader: R,
    dev_name: &str,
    dt_clock_names: &[String],
    summary_path: &Path,
) -> Result<(), MmregError> {
    let mut clock = String::new();
    let mut state = None;
    let mut line = String::with_capacity(160);

    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        let mut buf = [""; 12];
        let mut n = 0;
        for tok in line.split_whitespace().take(buf.len()) {
            buf[n] = tok;
            n += 1;
        }
        let tokens = &buf[..n];

        if let Some(row) = parse_clk_summary_row(tokens) {
            clock.clear();
            clock.push_str(tokens[0]);
            state = Some(row);
            if dt_clock_names.iter().any(|c| c == tokens[0]) || tokens.contains(&dev_name) {
                evaluate_clock_state(dev_name, &clock, row, summary_path)?;
            }
        } else if let Some(row) = state
            && tokens.contains(&dev_name)
        {
            evaluate_clock_state(dev_name, &clock, row, summary_path)?;
        }
        line.clear();
    }
    Ok(())
}

/// Verifies that every FPGA manager under `fpga_mgr_root` is `"operating"`.
fn check_fpga_manager_operating(dev_name: &str, fpga_mgr_root: &Path) -> Result<(), MmregError> {
    let Ok(entries) = fs::read_dir(fpga_mgr_root) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let state_path = entry.path().join("state");
        if let Ok(raw) = fs::read_to_string(&state_path)
            && !raw.trim().eq_ignore_ascii_case("operating")
        {
            return Err(MmregError::ZombieFpgaState {
                device_name: dev_name.to_string(),
                manager_name: entry.file_name().to_string_lossy().into_owned(),
                state: raw.trim().to_string(),
                state_path,
            });
        }
    }
    Ok(())
}

/// Holds `<device>/power/control` at `"on"` while a `/dev/mem` mapping is active, so the kernel
/// cannot autosuspend the device mid-access, and restores the previous value on drop.
#[derive(Debug)]
pub(crate) struct RuntimePmGuard {
    control_path: PathBuf,
    previous: String,
}

impl RuntimePmGuard {
    /// Sets `<dev_path>/power/control` to `"on"` unless it already is; warns and returns `None`
    /// when that is not permitted.
    pub(crate) fn acquire(dev_path: &Path) -> Option<Self> {
        let control_path = dev_path.join("power/control");
        let previous = fs::read_to_string(&control_path).ok()?.trim().to_string();
        if previous.eq_ignore_ascii_case("on") {
            return None;
        }
        match fs::write(&control_path, "on") {
            Ok(()) => Some(RuntimePmGuard {
                control_path,
                previous,
            }),
            Err(e) => {
                eprintln!(
                    "Warning: Failed to set '{}' to 'on' ({}); device may autosuspend during /dev/mem access.",
                    control_path.display(),
                    e
                );
                None
            }
        }
    }
}

impl Drop for RuntimePmGuard {
    fn drop(&mut self) {
        if let Err(e) = fs::write(&self.control_path, &self.previous) {
            eprintln!(
                "Warning: Failed to restore '{}' to '{}': {}",
                self.control_path.display(),
                self.previous,
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir;

    #[test]
    fn test_reg_covers_64bit_and_32bit() {
        let mut buf64 = [0u8; 16];
        buf64[0..8].copy_from_slice(&0xA000_0000u64.to_be_bytes());
        buf64[8..16].copy_from_slice(&0x1000u64.to_be_bytes());
        assert!(reg_covers(&buf64, 0xA000_0000, 4, None));
        assert!(reg_covers(&buf64, 0xA000_0FFC, 4, None));
        assert!(!reg_covers(&buf64, 0xA000_1000, 4, None));

        let mut buf32 = [0u8; 8];
        buf32[0..4].copy_from_slice(&0x43C0_0000u32.to_be_bytes());
        buf32[4..8].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        assert!(reg_covers(&buf32, 0x43C0_0010, 4, None));
        assert!(reg_covers(&buf32, 0x43C0_0010, 4, Some((1, 1))));
        assert!(!reg_covers(&buf32, 0x43C1_0000, 4, None));
    }

    #[test]
    fn test_parse_iomem_rejects_ram_and_reserved() {
        let sample = "\
00000000-7fffffff : System RAM
  00080000-015fffff : Kernel code
  01a00000-021fffff : Kernel data
80000000-8fffffff : reserved
e0000000-e0000fff : xuartps
  e0000000-e0000fff : serial
a0000000-a000ffff : axi_gpio@a0000000
";
        let parse = |target, size| parse_iomem(sample.as_bytes(), target, size);
        assert!(parse(0xA000_0000, 4).unwrap());
        assert!(parse(0xE000_0FFC, 4).unwrap());
        assert!(!parse(0xE000_0FFC, 8).unwrap());
        assert!(!parse(0x9000_0000, 4).unwrap());

        for (addr, resource) in [
            (0x0000_1000, "System RAM"),
            (0x0010_0000, "System RAM"),
            (0x8000_0100, "reserved"),
        ] {
            let err = parse(addr, 4).unwrap_err();
            assert_eq!(err.code(), "E_BLUEPRINT_RAM");
            assert!(err.to_string().contains(resource));
        }

        let hidden = "00000000-00000000 : System RAM\n00000000-00000000 : serial\n";
        assert!(!parse_iomem(hidden.as_bytes(), 0, 4).unwrap());
    }

    #[test]
    fn test_non_fpga_device_ignores_fpga_manager_state() {
        let root = test_dir("non_fpga");
        let ps_dev = root.join("soc").join("e0000000.serial");
        fs::create_dir_all(ps_dev.join("of_node")).unwrap();
        fs::create_dir_all(ps_dev.join("power")).unwrap();
        fs::write(ps_dev.join("of_node/status"), "okay\0").unwrap();
        fs::write(ps_dev.join("power/runtime_status"), "active\n").unwrap();

        let fpga_root = root.join("fpga_manager");
        fs::create_dir_all(fpga_root.join("fpga0")).unwrap();
        fs::write(fpga_root.join("fpga0/state"), "power off\n").unwrap();

        assert!(!is_fpga_backed_device(&ps_dev));
        assert!(preflight_check_device("e0000000.serial", &ps_dev, &fpga_root).is_ok());

        let pl_dev = root.join("amba_pl@0").join("a0000000.axi_gpio");
        fs::create_dir_all(pl_dev.join("of_node")).unwrap();
        fs::write(pl_dev.join("of_node/status"), "okay\0").unwrap();

        assert!(is_fpga_backed_device(&pl_dev));
        let err = preflight_check_device("a0000000.axi_gpio", &pl_dev, &fpga_root).unwrap_err();
        assert!(
            err.to_string()
                .contains("FPGA manager 'fpga0' is in state 'power off'")
        );

        fs::write(fpga_root.join("fpga0/state"), "operating\n").unwrap();
        assert!(preflight_check_device("a0000000.axi_gpio", &pl_dev, &fpga_root).is_ok());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_zombie_dt_status_runtime_pm_and_pm_guard() {
        let root = test_dir("zombie_checks");
        let fpga_root = root.join("fpga_manager");
        let dev = root.join("soc").join("ff000000.i2c");
        fs::create_dir_all(dev.join("of_node")).unwrap();
        fs::create_dir_all(dev.join("power")).unwrap();

        fs::write(dev.join("of_node/status"), "disabled\0").unwrap();
        let err = preflight_check_device("ff000000.i2c", &dev, &fpga_root).unwrap_err();
        assert!(err.to_string().contains("Device Tree status 'disabled'"));

        fs::write(dev.join("of_node/status"), "okay\0").unwrap();
        for status in ["suspended", "suspending"] {
            fs::write(dev.join("power/runtime_status"), format!("{status}\n")).unwrap();
            let err = preflight_check_device("ff000000.i2c", &dev, &fpga_root).unwrap_err();
            assert_eq!(err.code(), "E_ZOMBIE_RUNTIME_PM");
            assert!(err.to_string().contains(status));
        }

        fs::write(dev.join("power/runtime_status"), "active\n").unwrap();
        assert!(preflight_check_device("ff000000.i2c", &dev, &fpga_root).is_ok());

        fs::write(dev.join("power/control"), "auto\n").unwrap();
        {
            let guard = RuntimePmGuard::acquire(&dev);
            assert!(guard.is_some());
            let cur = fs::read_to_string(dev.join("power/control")).unwrap();
            assert_eq!(cur.trim(), "on");
        }
        let restored = fs::read_to_string(dev.join("power/control")).unwrap();
        assert_eq!(restored.trim(), "auto");

        fs::write(dev.join("power/control"), "on\n").unwrap();
        assert!(RuntimePmGuard::acquire(&dev).is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_find_uio_region() {
        let root = test_dir("uio_scan");
        let sysfs_uio = root.join("sys_uio");
        let dev_dir = root.join("dev");
        let map0 = sysfs_uio.join("uio0/maps/map0");
        fs::create_dir_all(&map0).unwrap();
        fs::create_dir_all(sysfs_uio.join("uio0/maps/junk")).unwrap();
        fs::write(map0.join("addr"), "0xa0000000\n").unwrap();
        fs::write(map0.join("size"), "0x00001000\n").unwrap();

        let found = find_uio_region(&sysfs_uio, &dev_dir, 0xA000_0120, 4).unwrap();
        assert_eq!(found.region_index, 0);
        assert_eq!(found.device_name, "uio0");
        assert_eq!(Path::new(&found.dev_node), dev_dir.join("uio0"));
        assert!(find_uio_region(&sysfs_uio, &dev_dir, 0xA000_0FFC, 8).is_none());
        assert!(find_uio_region(&sysfs_uio, &dev_dir, 0xA000_1000, 4).is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_find_owning_device_with_cells_and_suppliers() {
        let root = test_dir("sysfs_platform");
        let devices_dir = root.join("devices");
        let dev_path = devices_dir.join("a0000000.custom_ip");
        let of_node = dev_path.join("of_node");
        fs::create_dir_all(&of_node).unwrap();
        fs::write(dev_path.join("#address-cells"), 2u32.to_be_bytes()).unwrap();
        fs::write(dev_path.join("#size-cells"), 1u32.to_be_bytes()).unwrap();

        let mut reg = [0u8; 12];
        reg[0..8].copy_from_slice(&0xA000_0000u64.to_be_bytes());
        reg[8..12].copy_from_slice(&0x2000u32.to_be_bytes());
        fs::write(of_node.join("reg"), reg).unwrap();

        let found = find_owning_device(&devices_dir, 0xA000_0100, 4).unwrap();
        assert_eq!(found.0, "a0000000.custom_ip");
        assert!(find_owning_device(&devices_dir, 0xB000_0000, 4).is_none());

        let sup_dir = dev_path.join("supplier_platform_clk_ctrl").join("power");
        fs::create_dir_all(&sup_dir).unwrap();
        fs::write(sup_dir.join("runtime_status"), "suspending\n").unwrap();
        let fpga_root = root.join("fpga_mgr");
        let err = preflight_check_device("a0000000.custom_ip", &dev_path, &fpga_root).unwrap_err();
        assert_eq!(err.code(), "E_ZOMBIE_SUPPLIER_PM");
        assert!(err.to_string().contains("supplier_platform_clk_ctrl"));

        fs::write(sup_dir.join("runtime_status"), "active\n").unwrap();
        fs::create_dir_all(dev_path.join("supplier_platform_fpga_region0")).unwrap();
        assert!(is_fpga_backed_device(&dev_path));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_resolve_rejects_unknown_address() {
        let err = resolve(0xDEAD_BEEF_0000, 4, true).err().unwrap();
        assert_eq!(err.code(), "E_BLUEPRINT_UNMAPPED");
        assert!(matches!(
            resolve(0xDEAD_BEEF_0000, 4, false).unwrap().mapper,
            MapperType::DevMem
        ));
    }

    #[test]
    fn test_clock_hardware_enable_vs_software_enable_count() {
        let root = test_dir("ccf_clocks");
        let dev_path = root.join("a0000000.axi_gpio");
        fs::create_dir_all(dev_path.join("of_node")).unwrap();
        fs::write(dev_path.join("of_node/clock-names"), b"pl0_ref\0").unwrap();
        let clk_root = root.join("clk_debug");
        fs::create_dir_all(clk_root.join("pl0_ref")).unwrap();

        fs::write(clk_root.join("pl0_ref/clk_enable_count"), "0\n").unwrap();
        assert!(check_device_clocks("a0000000.axi_gpio", &dev_path, &clk_root).is_ok());

        let header = "\
                                 enable  prepare  protect                                duty  hardware                            connection
   clock                          count    count    count        rate   accuracy phase  cycle    enable   consumer                         id
---------------------------------------------------------------------------------------------------------------------------------------------
";
        let row = |hw| {
            format!(
                "{header}   fclk0                              0        0        0   100000000          0     0  50000         {hw}   a0000000.axi_gpio                s_axi_aclk\n"
            )
        };
        fs::write(clk_root.join("clk_summary"), row("Y")).unwrap();
        assert!(check_device_clocks("a0000000.axi_gpio", &dev_path, &clk_root).is_ok());

        fs::write(clk_root.join("clk_summary"), row("N")).unwrap();
        let err = check_device_clocks("a0000000.axi_gpio", &dev_path, &clk_root).unwrap_err();
        assert_eq!(err.code(), "E_ZOMBIE_CLOCK_GATED");
        assert!(err.to_string().contains("fclk0"));

        // Consumer listed on its own line below the clock row.
        let split = format!(
            "{header}   fclk1                              1        1        0   100000000          0     0  50000         N   deviceless                       no_connection_id\n                                                                                                         a0000000.axi_gpio                s_axi_aclk\n"
        );
        fs::write(clk_root.join("clk_summary"), split).unwrap();
        let err = check_device_clocks("a0000000.axi_gpio", &dev_path, &clk_root).unwrap_err();
        assert!(err.to_string().contains("fclk1"));

        let legacy = "\
   clock                         enable_cnt  prepare_cnt        rate   accuracy   phase
----------------------------------------------------------------------------------------
   pl0_ref                                0            0   100000000          0       0
";
        fs::write(clk_root.join("clk_summary"), legacy).unwrap();
        assert!(check_device_clocks("a0000000.axi_gpio", &dev_path, &clk_root).is_ok());

        let _ = fs::remove_dir_all(&root);
    }
}

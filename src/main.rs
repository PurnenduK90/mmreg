use mmreg::parse::{parse_u32_radix, parse_u64_radix};
use mmreg::{Device, MmregError, Width};
use std::env;
use std::process;

fn usage_text() -> &'static str {
    "Usage:\n  \
  mmreg [options] <read|write> <address> [value]\n  \
  mmreg [options] <read|write> <uio-name> <offset> [value]\n\
Options:\n  \
  -w, --width <8|16|32|64>  Access width in bits (default: 32)\n  \
  -f, --force               Skip address blueprint and hardware pre-flight validation\n  \
  -v, --validate <file>     Run a JSON Safety Profile's checks before the access (may be repeated)\n  \
  -d, --dec                 Interpret unprefixed numbers as decimal instead of hexadecimal\n  \
  -V, --version             Print version and enabled features\n  \
  -h, --help                Print this help message\n\
Commands:\n  \
  read  <address>                   Read value at physical address\n  \
  read  <uio-name> <offset>         Read value at byte offset in UIO device\n  \
  write <address> <value>           Write value to physical address\n  \
  write <uio-name> <offset> <value> Write value to byte offset in UIO device\n\
Examples:\n  \
  mmreg read 0x4000_0000\n  \
  mmreg --width 16 read axi_gpio 0x04\n  \
  mmreg write 0x4000_0000 0xDEADBEEF\n  \
  mmreg write axi_gpio 0x00 0x00000001\n  \
  mmreg --validate zynqmp_safety.json read 0xA000_0000\n  \
  mmreg --force read 0x4000_0000"
}

fn version_text() -> String {
    let mut features = Vec::new();
    if cfg!(feature = "profile") {
        features.push("profile");
    }
    if cfg!(feature = "debug") {
        features.push("debug");
    }
    let feat_str = if features.is_empty() {
        "none".to_string()
    } else {
        features.join(", ")
    };
    format!(
        "mmreg {} (features: {})",
        env!("CARGO_PKG_VERSION"),
        feat_str
    )
}

fn usage_error(message: &str) -> String {
    format!("{}\n{}", message, usage_text())
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CliTarget<'a> {
    Address(u64),
    Uio { name: &'a str, offset: u32 },
}

trait CliBackend {
    fn read(&mut self, target: CliTarget<'_>, width: Width, force: bool)
        -> Result<u64, MmregError>;
    fn write(
        &mut self,
        target: CliTarget<'_>,
        width: Width,
        value: u64,
        force: bool,
    ) -> Result<(), MmregError>;
}

struct HardwareBackend;

impl CliBackend for HardwareBackend {
    fn read(
        &mut self,
        target: CliTarget<'_>,
        width: Width,
        force: bool,
    ) -> Result<u64, MmregError> {
        match target {
            CliTarget::Address(address) => {
                if address % (width.bytes() as u64) != 0 {
                    return Err(MmregError::UnalignedWidthAccess {
                        address,
                        offset: 0,
                        width_bits: width.bits(),
                    });
                }
                let dev = Device::addr(address, width.bytes())
                    .read_only()
                    .with_force(force)
                    .open()?;
                for w in dev.warnings() {
                    eprintln!("{w}");
                }
                dev.read(0, width)
            }
            CliTarget::Uio { name, offset } => {
                let dev = Device::uio(name).read_only().with_force(force).open()?;
                for w in dev.warnings() {
                    eprintln!("{w}");
                }
                dev.read(offset, width)
            }
        }
    }

    fn write(
        &mut self,
        target: CliTarget<'_>,
        width: Width,
        value: u64,
        force: bool,
    ) -> Result<(), MmregError> {
        match target {
            CliTarget::Address(address) => {
                if address % (width.bytes() as u64) != 0 {
                    return Err(MmregError::UnalignedWidthAccess {
                        address,
                        offset: 0,
                        width_bits: width.bits(),
                    });
                }
                let dev = Device::addr(address, width.bytes())
                    .with_force(force)
                    .open()?;
                for w in dev.warnings() {
                    eprintln!("{w}");
                }
                dev.write(0, width, value)
            }
            CliTarget::Uio { name, offset } => {
                let dev = Device::uio(name).with_force(force).open()?;
                for w in dev.warnings() {
                    eprintln!("{w}");
                }
                dev.write(offset, width, value)
            }
        }
    }
}

fn looks_like_address(arg: &str, default_radix: u32) -> bool {
    arg.trim()
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_digit)
        && parse_u64_radix(arg, default_radix).is_ok()
}

fn parse_width(raw: &str) -> Result<Width, String> {
    raw.trim()
        .parse::<u32>()
        .ok()
        .and_then(Width::from_bits)
        .ok_or_else(|| {
            usage_error(&format!(
                "[E_CLI_INVALID_WIDTH] Invalid width '{}' (expected 8, 16, 32, or 64)",
                raw
            ))
        })
}

fn parse_value(raw: &str, radix: u32, width: Width) -> Result<u64, String> {
    let val = parse_u64_radix(raw, radix).ok().filter(|&v| match width {
        Width::U8 => u8::try_from(v).is_ok(),
        Width::U16 => u16::try_from(v).is_ok(),
        Width::U32 => u32::try_from(v).is_ok(),
        Width::U64 => true,
    });
    val.ok_or_else(|| {
        format!(
            "[E_CLI_INVALID_VAL] Invalid value '{}' (expected {}-bit hex value, e.g., 0xDEADBEEF, or decimal with 0d/--dec)",
            raw,
            width.bits()
        )
    })
}

fn format_hex(value: u64, width: Width) -> String {
    match width {
        Width::U8 => format!("0x{:02X}", value as u8),
        Width::U16 => format!("0x{:04X}", value as u16),
        Width::U32 => format!("0x{:08X}", value as u32),
        Width::U64 => format!("0x{:016X}", value),
    }
}

/// Runs the CLI against `backend`.
/// Returns `Ok(stdout_message)` on success or `Err(stderr_message)` on failure.
fn run_cli<B: CliBackend>(args: &[String], backend: &mut B) -> Result<String, String> {
    if args[1..].iter().any(|a| a == "-h" || a == "--help") {
        return Ok(usage_text().to_string());
    }
    if args[1..].iter().any(|a| a == "-V" || a == "--version") {
        return Ok(version_text());
    }

    let mut force_map = false;
    let mut default_radix: u32 = 16;
    let mut width = Width::U32;
    let mut validate_paths: Vec<&str> = Vec::new();
    let mut positionals: Vec<&str> = Vec::new();
    let mut idx = 1;

    while let Some(arg) = args.get(idx) {
        match arg.as_str() {
            "-f" | "--force" => force_map = true,
            "-d" | "--dec" => default_radix = 10,
            "-v" | "--validate" => {
                idx += 1;
                let path = args.get(idx).ok_or_else(|| {
                    usage_error(&format!(
                        "[E_CLI_MISSING_PROFILE] Missing file path after {}",
                        arg
                    ))
                })?;
                validate_paths.push(path);
            }
            "-w" | "--width" => {
                idx += 1;
                let w_arg = args.get(idx).ok_or_else(|| {
                    usage_error(&format!(
                        "[E_CLI_MISSING_WIDTH] Missing bit width after {}",
                        arg
                    ))
                })?;
                width = parse_width(w_arg)?;
            }
            a if a.starts_with('-') => {
                return Err(usage_error(&format!(
                    "[E_CLI_UNKNOWN_OPT] Unknown option: {}",
                    a
                )));
            }
            a => positionals.push(a),
        }
        idx += 1;
    }

    // Parse every operand before touching hardware (a profile's checks read registers).
    let (target, value) = match positionals.as_slice() {
        [] => return Err(usage_error("[E_CLI_MISSING_CMD] Missing command")),
        [cmd] if *cmd == "read" || *cmd == "write" => {
            return Err(usage_error("[E_CLI_MISSING_ADDR] Missing address argument"));
        }
        ["read", addr] => {
            let address = parse_u64_radix(addr, default_radix).map_err(|_| {
                format!(
                    "[E_CLI_INVALID_ADDR] Invalid address '{}' (expected 64-bit hex address, e.g., 0x4000_0000, or 'mmreg read <uio-name> <offset>')",
                    addr
                )
            })?;
            (CliTarget::Address(address), None)
        }
        ["read", first, offset_arg] => {
            if looks_like_address(first, default_radix) {
                return Err(usage_error("[E_CLI_EXTRA_ARGS] Too many arguments"));
            }
            let offset = parse_u32_radix(offset_arg, default_radix).map_err(|_| {
                format!(
                    "[E_CLI_INVALID_OFFSET] Invalid offset '{}' (expected 32-bit hex offset, e.g., 0x04, or decimal with 0d/--dec)",
                    offset_arg
                )
            })?;
            (
                CliTarget::Uio {
                    name: first,
                    offset,
                },
                None,
            )
        }
        ["write", _] => {
            return Err(usage_error(
                "[E_CLI_MISSING_VAL] Missing value argument for write",
            ));
        }
        ["write", addr, val_arg] => {
            let address = parse_u64_radix(addr, default_radix).map_err(|_| {
                format!(
                    "[E_CLI_INVALID_ADDR] Invalid address '{}' (expected 64-bit hex address, e.g., 0x4000_0000, or 'mmreg write <uio-name> <offset> <value>')",
                    addr
                )
            })?;
            let val = parse_value(val_arg, default_radix, width)?;
            (CliTarget::Address(address), Some(val))
        }
        ["write", first, offset_arg, val_arg] => {
            if looks_like_address(first, default_radix) {
                return Err(usage_error("[E_CLI_EXTRA_ARGS] Too many arguments"));
            }
            let offset = parse_u32_radix(offset_arg, default_radix).map_err(|_| {
                format!(
                    "[E_CLI_INVALID_OFFSET] Invalid offset '{}' (expected 32-bit hex offset, e.g., 0x04, or decimal with 0d/--dec)",
                    offset_arg
                )
            })?;
            let val = parse_value(val_arg, default_radix, width)?;
            (
                CliTarget::Uio {
                    name: first,
                    offset,
                },
                Some(val),
            )
        }
        [cmd, ..] if *cmd == "read" || *cmd == "write" => {
            return Err(usage_error("[E_CLI_EXTRA_ARGS] Too many arguments"));
        }
        [cmd, ..] => {
            return Err(usage_error(&format!(
                "[E_CLI_UNKNOWN_CMD] Unknown command: '{}'",
                cmd
            )));
        }
    };

    for path in validate_paths {
        run_profile(path, |addr| {
            backend
                .read(CliTarget::Address(addr), Width::U32, true)
                .map(|v| v as u32)
        })?;
    }

    match value {
        None => {
            let val = backend
                .read(target, width, force_map)
                .map_err(|e| format!("Read error: {}", e))?;
            Ok(format_hex(val, width))
        }
        Some(val) => {
            backend
                .write(target.clone(), width, val, force_map)
                .map_err(|e| format!("Write error: {}", e))?;
            let formatted = format_hex(val, width);
            match target {
                CliTarget::Address(address) => {
                    Ok(format!("Wrote {} to 0x{:X}", formatted, address))
                }
                CliTarget::Uio { name, offset } => {
                    Ok(format!("Wrote {} to {}+0x{:X}", formatted, name, offset))
                }
            }
        }
    }
}

/// Loads the JSON safety profile at `path` and runs its checks with `read`.
#[cfg(feature = "profile")]
fn run_profile(path: &str, read: impl FnMut(u64) -> Result<u32, MmregError>) -> Result<(), String> {
    mmreg::SafetyProfile::from_file(std::path::Path::new(path))
        .and_then(|profile| profile.execute_with(read))
        .map_err(|e| e.to_string())
}

#[cfg(not(feature = "profile"))]
fn run_profile(
    _path: &str,
    _read: impl FnMut(u64) -> Result<u32, MmregError>,
) -> Result<(), String> {
    Err("[E_CLI_NO_PROFILE] --validate needs mmreg built with the `profile` feature".to_string())
}

fn main() {
    let args: Vec<String> = env::args().collect();
    match run_cli(&args, &mut HardwareBackend) {
        Ok(out) => println!("{}", out),
        Err(err) => {
            eprintln!("{}", err);
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FnBackend<R, W> {
        read_fn: R,
        write_fn: Option<W>,
    }

    impl<R, W> CliBackend for FnBackend<R, W>
    where
        R: FnMut(CliTarget<'_>, Width, bool) -> Result<u64, MmregError>,
        W: FnOnce(CliTarget<'_>, Width, u64, bool) -> Result<(), MmregError>,
    {
        fn read(
            &mut self,
            target: CliTarget<'_>,
            width: Width,
            force: bool,
        ) -> Result<u64, MmregError> {
            (self.read_fn)(target, width, force)
        }

        fn write(
            &mut self,
            target: CliTarget<'_>,
            width: Width,
            value: u64,
            force: bool,
        ) -> Result<(), MmregError> {
            (self.write_fn.take().expect("write called once"))(target, width, value, force)
        }
    }

    fn to_args(slice: &[&str]) -> Vec<String> {
        slice.iter().map(|s| (*s).to_string()).collect()
    }

    fn run_cli_with<R, W>(args: &[String], mut read_fn: R, write_fn: W) -> Result<String, String>
    where
        R: FnMut(u64, bool) -> Result<u32, MmregError>,
        W: FnOnce(u64, u32, bool) -> Result<(), MmregError>,
    {
        let mut backend = FnBackend {
            read_fn: |target: CliTarget<'_>, _w: Width, force: bool| match target {
                CliTarget::Address(addr) => read_fn(addr, force).map(u64::from),
                CliTarget::Uio { name, offset } => {
                    assert!(name == "axi_gpio" || name == "adc0");
                    Ok(u64::from(0xA000_0000 + offset))
                }
            },
            write_fn: Some(
                |target: CliTarget<'_>, _w: Width, val: u64, force: bool| match target {
                    CliTarget::Address(addr) => write_fn(addr, val as u32, force),
                    CliTarget::Uio { name, offset } => {
                        assert!(name == "axi_gpio" || name == "dac0");
                        assert_eq!(offset, 0x08);
                        assert_eq!(val, 0x55);
                        Ok(())
                    }
                },
            ),
        };
        run_cli(args, &mut backend)
    }

    fn ok_read(addr: u64, force: bool) -> Result<u32, MmregError> {
        if addr == 0xFFCA_3010 {
            return Ok(0x4);
        }
        assert_eq!(addr, 0x4000_0000);
        Ok(if force { 0xDEAD_BEEF } else { 0x1234_5678 })
    }

    fn ok_write(addr: u64, val: u32, _force: bool) -> Result<(), MmregError> {
        assert_eq!(addr, 0x4000_0000);
        assert_eq!(val, 0xCAFE_BABE);
        Ok(())
    }

    fn run(args: &[&str]) -> Result<String, String> {
        run_cli_with(&to_args(args), ok_read, ok_write)
    }

    #[test]
    fn test_cli_arguments() {
        assert!(run(&["mmreg", "--help"]).unwrap().contains("Usage:"));
        assert!(run(&["mmreg", "read", "--help"])
            .unwrap()
            .contains("Usage:"));
        let ver = run(&["mmreg", "--version"]).unwrap();
        assert!(ver.starts_with("mmreg 1.1.") && ver.contains("features:"));
        assert_eq!(run(&["mmreg", "-V"]).unwrap(), ver);

        assert!(run(&["mmreg"]).unwrap_err().contains("[E_CLI_MISSING_CMD]"));
        assert!(run(&["mmreg", "--bad"])
            .unwrap_err()
            .contains("[E_CLI_UNKNOWN_OPT] Unknown option: --bad"));
        assert!(run(&["mmreg", "--validate"])
            .unwrap_err()
            .contains("[E_CLI_MISSING_PROFILE]"));
        assert!(run(&["mmreg", "--width"])
            .unwrap_err()
            .contains("[E_CLI_MISSING_WIDTH]"));
        assert!(run(&["mmreg", "--width", "24", "read", "0x4000_0000"])
            .unwrap_err()
            .contains("[E_CLI_INVALID_WIDTH]"));
        assert!(run(&["mmreg", "read"])
            .unwrap_err()
            .contains("[E_CLI_MISSING_ADDR]"));
        for bad in ["0xZZZZ", "0x0x10"] {
            assert!(run(&["mmreg", "read", bad])
                .unwrap_err()
                .contains("[E_CLI_INVALID_ADDR]"));
        }
        assert!(run(&["mmreg", "read", "0x4000_0000", "0x5"])
            .unwrap_err()
            .contains("[E_CLI_EXTRA_ARGS]"));
        assert!(run(&["mmreg", "write", "0x4000_0000", "0x5", "0x6"])
            .unwrap_err()
            .contains("[E_CLI_EXTRA_ARGS]"));
        assert!(run(&["mmreg", "read", "axi_gpio", "bad_off"])
            .unwrap_err()
            .contains("[E_CLI_INVALID_OFFSET]"));
        assert!(run(&["mmreg", "write", "axi_gpio", "bad_off", "1"])
            .unwrap_err()
            .contains("[E_CLI_INVALID_OFFSET]"));
        assert!(run(&["mmreg", "write", "0x4000_0000"])
            .unwrap_err()
            .contains("[E_CLI_MISSING_VAL]"));
        assert!(run(&["mmreg", "write", "0x4000_0000", "not_hex"])
            .unwrap_err()
            .contains("[E_CLI_INVALID_VAL]"));
        assert!(run(&["mmreg", "-w", "8", "write", "0x4000_0000", "0x100"])
            .unwrap_err()
            .contains("[E_CLI_INVALID_VAL]"));
        assert!(run(&["mmreg", "erase"])
            .unwrap_err()
            .contains("[E_CLI_UNKNOWN_CMD]"));
        assert!(run(&["mmreg", "erase", "0x4000_0000"])
            .unwrap_err()
            .contains("[E_CLI_UNKNOWN_CMD]"));

        assert_eq!(
            run(&["mmreg", "read", "0x4000_0000"]).unwrap(),
            "0x12345678"
        );
        assert_eq!(
            run(&["mmreg", "-w", "8", "read", "0x4000_0000"]).unwrap(),
            "0x78"
        );
        assert_eq!(
            run(&["mmreg", "--width", "16", "read", "0x4000_0000"]).unwrap(),
            "0x5678"
        );
        assert_eq!(
            run(&["mmreg", "--width", "64", "read", "0x4000_0000"]).unwrap(),
            "0x0000000012345678"
        );
        assert_eq!(
            run(&["mmreg", "read", "axi_gpio", "0x04"]).unwrap(),
            "0xA0000004"
        );
        // UIO device names consisting of hex letters/digits (`adc0`, `dac0`) are not rejected as addresses.
        assert_eq!(
            run(&["mmreg", "read", "adc0", "0x04"]).unwrap(),
            "0xA0000004"
        );
        assert_eq!(
            run(&["mmreg", "--dec", "read", "adc0", "4"]).unwrap(),
            "0xA0000004"
        );
        assert_eq!(
            run(&["mmreg", "-w", "8", "write", "axi_gpio", "0x08", "0x55"]).unwrap(),
            "Wrote 0x55 to axi_gpio+0x8"
        );
        assert_eq!(
            run(&["mmreg", "-w", "8", "write", "dac0", "0x08", "0x55"]).unwrap(),
            "Wrote 0x55 to dac0+0x8"
        );
        assert_eq!(
            run(&["mmreg", "--dec", "-w", "16", "write", "dac0", "8", "85"]).unwrap(),
            "Wrote 0x0055 to dac0+0x8"
        );
        assert_eq!(
            run(&["mmreg", "-w", "64", "write", "axi_gpio", "0d8", "0x55"]).unwrap(),
            "Wrote 0x0000000000000055 to axi_gpio+0x8"
        );
        assert_eq!(
            run(&["mmreg", "-f", "read", "0x4000_0000"]).unwrap(),
            "0xDEADBEEF"
        );
        assert_eq!(
            run(&["mmreg", "write", "0x4000_0000", "0xCAFE_BABE"]).unwrap(),
            "Wrote 0xCAFEBABE to 0x40000000"
        );

        let write_100 = |addr: u64, val: u32, _: bool| {
            assert_eq!((addr, val), (0x4000_0000, 100));
            Ok(())
        };
        let out = run_cli_with(
            &to_args(&["mmreg", "--dec", "write", "0x4000_0000", "100"]),
            ok_read,
            write_100,
        )
        .unwrap();
        assert_eq!(out, "Wrote 0x00000064 to 0x40000000");

        let fail_read = |address, _| Err(MmregError::BlueprintUnmapped { address, size: 4 });
        let fail_write = |address, _, _| Err(MmregError::BlueprintUnmapped { address, size: 4 });
        let args = to_args(&["mmreg", "read", "0x4000_0000"]);
        assert!(run_cli_with(&args, fail_read, ok_write).is_err());
        let args = to_args(&["mmreg", "write", "0x4000_0000", "0x1"]);
        assert!(run_cli_with(&args, ok_read, fail_write).is_err());
    }

    #[cfg(feature = "profile")]
    #[test]
    fn test_cli_validate_profile() {
        let profile = std::env::temp_dir().join(format!("mmreg_cli_{}.json", process::id()));
        std::fs::write(
            &profile,
            r#"{"name": "ZynqMP", "checks": [{"name": "PCAP_DONE", "addr": "0xFFCA_3010", "mask": "0x4", "expected": "0x4"}]}"#,
        )
        .unwrap();
        let path = profile.to_str().unwrap();
        assert_eq!(
            run(&["mmreg", "--validate", path, "read", "0x4000_0000"]).unwrap(),
            "0x12345678"
        );

        // A bad operand is reported before the profile reads any register.
        let no_reads = |_: u64, _: bool| -> Result<u32, MmregError> { panic!("hardware read") };
        let args = to_args(&["mmreg", "-v", path, "write", "0x4000_0000", "zz"]);
        assert!(run_cli_with(&args, no_reads, ok_write).is_err());

        let _ = std::fs::remove_file(&profile);
    }

    #[cfg(not(feature = "profile"))]
    #[test]
    fn test_cli_validate_without_profile_feature() {
        assert!(run(&["mmreg", "-v", "p.json", "read", "0x4000_0000"])
            .unwrap_err()
            .contains("[E_CLI_NO_PROFILE]"));
    }
}

use mmreg::parse::{parse_u32_radix, parse_u64_radix};
use mmreg::MmregError;
use std::env;
use std::process;

fn usage_text() -> &'static str {
    "Usage: mmreg [options] <read|write> <address> [value]\n\
Options:\n  \
  -f, --force               Skip address blueprint and hardware pre-flight validation\n  \
  -v, --validate <file>     Run a TOML Safety Profile's checks before the access\n  \
  -d, --dec                 Interpret unprefixed numbers as decimal instead of hexadecimal\n  \
  -h, --help                Print this help message\n\
Commands:\n  \
  read  <address>           Read 32-bit value at address\n  \
  write <address> <value>   Write 32-bit value to address\n\
Examples:\n  \
  mmreg read 0x4000_0000\n  \
  mmreg write 0x4000_0000 0xDEADBEEF\n  \
  mmreg write 0x4000_0000 0d100\n  \
  mmreg --validate zynqmp_safety.toml read 0xA000_0000\n  \
  mmreg --force read 0x4000_0000"
}

fn usage_error(message: &str) -> String {
    format!("{}\n{}", message, usage_text())
}

/// Runs the CLI with `read_fn` / `write_fn` as the register accessors.
/// Returns `Ok(stdout_message)` on success or `Err(stderr_message)` on failure.
fn run_cli_with<R, W>(args: &[String], mut read_fn: R, write_fn: W) -> Result<String, String>
where
    R: FnMut(u64, bool) -> Result<u32, MmregError>,
    W: FnOnce(u64, u32, bool) -> Result<(), MmregError>,
{
    let mut force_map = false;
    let mut default_radix: u32 = 16;
    let mut validate_path: Option<&str> = None;
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
                validate_path = Some(path);
            }
            "-h" | "--help" => return Ok(usage_text().to_string()),
            a if !a.starts_with('-') => break,
            a => return Err(usage_error(&format!("Unknown option: {}", a))),
        }
        idx += 1;
    }

    // Parse every operand before touching hardware (a profile's checks read registers).
    let (address_arg, value_arg) = match args.get(idx..).unwrap_or_default() {
        [] => return Err(usage_text().to_string()),
        [_] => return Err(usage_error("Missing address argument")),
        [cmd, addr] if cmd == "read" => (addr, None),
        [cmd, _] if cmd == "write" => {
            return Err(usage_error(
                "[E_CLI_MISSING_VAL] Missing value argument for write",
            ));
        }
        [cmd, addr, value] if cmd == "write" => (addr, Some(value)),
        [cmd, ..] if cmd == "read" || cmd == "write" => {
            return Err(usage_error("[E_CLI_EXTRA_ARGS] Too many arguments"));
        }
        [cmd, ..] => {
            return Err(usage_error(&format!(
                "[E_CLI_UNKNOWN_CMD] Unknown command: '{}'",
                cmd
            )));
        }
    };

    let address = parse_u64_radix(address_arg, default_radix).map_err(|_| {
        format!(
            "[E_CLI_INVALID_ADDR] Invalid address '{}' (expected 64-bit hex address, e.g., 0x4000_0000, or decimal with 0d/--dec)",
            address_arg
        )
    })?;
    let value = value_arg
        .map(|v| {
            parse_u32_radix(v, default_radix).map_err(|_| {
                format!(
                    "[E_CLI_INVALID_VAL] Invalid value '{}' (expected 32-bit hex value, e.g., 0xDEADBEEF, or decimal with 0d/--dec)",
                    v
                )
            })
        })
        .transpose()?;

    if let Some(path) = validate_path {
        run_profile(path, |addr| read_fn(addr, true))?;
    }

    match value {
        None => {
            let val = read_fn(address, force_map).map_err(|e| format!("Read error: {}", e))?;
            Ok(format!("0x{:08X}", val))
        }
        Some(value) => {
            write_fn(address, value, force_map).map_err(|e| format!("Write error: {}", e))?;
            Ok(format!("Wrote 0x{:08X} to 0x{:X}", value, address))
        }
    }
}

/// Loads the TOML safety profile at `path` and runs its checks with `read`.
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
    match run_cli_with(&args, mmreg::read_register_at, mmreg::write_register_at) {
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

    fn to_args(slice: &[&str]) -> Vec<String> {
        slice.iter().map(|s| (*s).to_string()).collect()
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
        assert!(run(&["mmreg", "--help"]).unwrap().contains("Usage: mmreg"));
        assert!(run(&["mmreg"]).is_err());
        assert!(run(&["mmreg", "--bad"])
            .unwrap_err()
            .contains("Unknown option: --bad"));
        assert!(run(&["mmreg", "--validate"])
            .unwrap_err()
            .contains("[E_CLI_MISSING_PROFILE]"));
        assert!(run(&["mmreg", "read"])
            .unwrap_err()
            .contains("Missing address argument"));
        for bad in ["0xZZZZ", "0x0x10"] {
            assert!(run(&["mmreg", "read", bad])
                .unwrap_err()
                .contains("[E_CLI_INVALID_ADDR]"));
        }
        assert!(run(&["mmreg", "read", "0x4000_0000", "0x5"])
            .unwrap_err()
            .contains("[E_CLI_EXTRA_ARGS]"));
        assert!(run(&["mmreg", "write", "0x4000_0000"])
            .unwrap_err()
            .contains("[E_CLI_MISSING_VAL]"));
        assert!(run(&["mmreg", "write", "0x4000_0000", "not_hex"])
            .unwrap_err()
            .contains("[E_CLI_INVALID_VAL]"));
        assert!(run(&["mmreg", "erase", "0x4000_0000"])
            .unwrap_err()
            .contains("[E_CLI_UNKNOWN_CMD]"));

        assert_eq!(
            run(&["mmreg", "read", "0x4000_0000"]).unwrap(),
            "0x12345678"
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
        let profile = std::env::temp_dir().join(format!("mmreg_cli_{}.toml", process::id()));
        std::fs::write(
            &profile,
            "name = \"ZynqMP\"\n[[checks]]\nname = \"PCAP_DONE\"\naddr = 0xFFCA_3010\nmask = 0x4\nexpected = 0x4\n",
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
}

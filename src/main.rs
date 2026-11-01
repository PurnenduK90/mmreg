use mmreg::parse::{parse_u32_radix, parse_u64_radix};
use mmreg::{Device, MmregError, Width};
use std::env;
use std::process;
use std::time::Duration;

fn usage_text() -> &'static str {
    "Usage:\n  \
  mmreg [options] <read|write|watch> <address> [value]\n  \
  mmreg [options] <read|write|watch> <uio-name> <offset> [value]\n  \
  mmreg [options] irq <uio-name> [timeout_ms]\n\
Options:\n  \
  -w, --width <8|16|32|64>         Access width in bits (default: 32)\n  \
  -c, --count <N>                  Word count for read, or poll limit for watch\n  \
  -i, --interval <ms>              Poll interval in ms for watch (default: 100)\n  \
  -F, --format <hex|dec|bin|json>  Output format (default: hex)\n  \
  -u, --unmask                     Unmask the UIO interrupt line before waiting in irq\n  \
  -f, --force                      Skip address blueprint and hardware pre-flight validation\n  \
  -v, --validate <file>            Run a JSON Safety Profile's checks before the access (may be repeated)\n  \
  -d, --dec                        Interpret unprefixed numbers as decimal instead of hexadecimal\n  \
  -V, --version                    Print version and enabled features\n  \
  -h, --help                       Print this help message\n\
Commands:\n  \
  read  <address>                   Read value(s) at physical address (--count N for block read)\n  \
  read  <uio-name> <offset>         Read value(s) at byte offset in UIO device\n  \
  write <address> <value>           Write value to physical address\n  \
  write <uio-name> <offset> <value> Write value to byte offset in UIO device\n  \
  watch <address>                   Poll physical address and print changes\n  \
  watch <uio-name> <offset>         Poll byte offset in UIO device and print changes\n  \
  irq   <uio-name> [timeout_ms]     Wait for a UIO interrupt (optional timeout in ms)\n\
Examples:\n  \
  mmreg read 0x4000_0000\n  \
  mmreg read axi_gpio 0x00 --count 8\n  \
  mmreg --width 16 read axi_gpio 0x04\n  \
  mmreg write 0x4000_0000 0xDEADBEEF\n  \
  mmreg write axi_gpio 0x00 0x00000001\n  \
  mmreg watch axi_gpio 0x04 --interval 50\n  \
  mmreg irq axi_timer 5000\n  \
  mmreg --format json read axi_gpio 0x00 --count 4\n  \
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputFormat {
    Hex,
    Dec,
    Bin,
    Json,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CliTarget<'a> {
    Address(u64),
    Uio { name: &'a str, offset: u32 },
}

enum CliCommand<'a> {
    Read {
        target: CliTarget<'a>,
    },
    Write {
        target: CliTarget<'a>,
        value: u64,
    },
    Watch {
        target: CliTarget<'a>,
    },
    Irq {
        name: &'a str,
        timeout_ms: Option<u64>,
    },
}

trait CliBackend {
    fn read(&mut self, target: CliTarget<'_>, width: Width, force: bool)
        -> Result<u64, MmregError>;

    fn read_block(
        &mut self,
        target: CliTarget<'_>,
        width: Width,
        count: usize,
        force: bool,
    ) -> Result<Vec<u64>, MmregError>;

    fn write(
        &mut self,
        target: CliTarget<'_>,
        width: Width,
        value: u64,
        force: bool,
    ) -> Result<(), MmregError>;

    fn watch<F>(
        &mut self,
        target: CliTarget<'_>,
        width: Width,
        interval_ms: u64,
        max_polls: Option<usize>,
        force: bool,
        on_change: F,
    ) -> Result<(), MmregError>
    where
        F: FnMut(usize, Option<u64>, u64);

    fn irq_wait(
        &mut self,
        name: &str,
        timeout_ms: Option<u64>,
        unmask: bool,
        force: bool,
    ) -> Result<Option<u32>, MmregError>;
}

/// Polls `read` every `interval`, `max_polls` times (forever with `None`), calling
/// `on_change(poll, previous, value)` for the first value and each change: the one watch loop,
/// shared by the hardware backend and the tests.
fn poll_changes(
    mut read: impl FnMut() -> Result<u64, MmregError>,
    interval: Duration,
    max_polls: Option<usize>,
    mut on_change: impl FnMut(usize, Option<u64>, u64),
) -> Result<(), MmregError> {
    let mut prev = None;
    let mut poll = 0;
    loop {
        let value = read()?;
        if prev != Some(value) {
            on_change(poll, prev, value);
            prev = Some(value);
        }
        poll += 1;
        if max_polls.is_some_and(|limit| poll >= limit) {
            return Ok(());
        }
        std::thread::sleep(interval);
    }
}

struct HardwareBackend;

impl HardwareBackend {
    fn open_target(
        target: CliTarget<'_>,
        width: Width,
        count: usize,
        read_only: bool,
        force: bool,
    ) -> Result<(Device, u32), MmregError> {
        let step = width.bytes();
        // `parse_count` keeps `count * 8` within `usize`.
        let total_bytes = count * step;
        let (builder, offset) = match target {
            CliTarget::Address(address) => {
                if address % (step as u64) != 0 {
                    return Err(MmregError::UnalignedWidthAccess {
                        address,
                        offset: 0,
                        width_bits: width.bits(),
                    });
                }
                (Device::addr(address, total_bytes), 0)
            }
            CliTarget::Uio { name, offset } => (Device::uio(name), offset),
        };
        let dev = builder.with_read_only(read_only).with_force(force).open()?;
        for w in dev.warnings() {
            eprintln!("{w}");
        }
        Ok((dev, offset))
    }
}

impl CliBackend for HardwareBackend {
    fn read(
        &mut self,
        target: CliTarget<'_>,
        width: Width,
        force: bool,
    ) -> Result<u64, MmregError> {
        let (dev, offset) = Self::open_target(target, width, 1, true, force)?;
        dev.read(offset, width)
    }

    fn read_block(
        &mut self,
        target: CliTarget<'_>,
        width: Width,
        count: usize,
        force: bool,
    ) -> Result<Vec<u64>, MmregError> {
        let (dev, offset) = Self::open_target(target, width, count, true, force)?;
        // Checked before allocating `count` words; `Device::read_block` checks the range again.
        let available = dev.size().saturating_sub(offset as usize) / width.bytes();
        if count > available {
            return Err(MmregError::OutOfBoundsAccess {
                register_name: dev.name().to_string(),
                offset,
                access_size: count * width.bytes(),
                region_size: dev.size(),
            });
        }
        let mut values = vec![0; count];
        dev.read_block(offset, width, &mut values)?;
        Ok(values)
    }

    fn write(
        &mut self,
        target: CliTarget<'_>,
        width: Width,
        value: u64,
        force: bool,
    ) -> Result<(), MmregError> {
        let (dev, offset) = Self::open_target(target, width, 1, false, force)?;
        dev.write(offset, width, value)
    }

    fn watch<F>(
        &mut self,
        target: CliTarget<'_>,
        width: Width,
        interval_ms: u64,
        max_polls: Option<usize>,
        force: bool,
        on_change: F,
    ) -> Result<(), MmregError>
    where
        F: FnMut(usize, Option<u64>, u64),
    {
        let (dev, offset) = Self::open_target(target, width, 1, true, force)?;
        poll_changes(
            || dev.read(offset, width),
            Duration::from_millis(interval_ms),
            max_polls,
            on_change,
        )
    }

    fn irq_wait(
        &mut self,
        name: &str,
        timeout_ms: Option<u64>,
        unmask: bool,
        force: bool,
    ) -> Result<Option<u32>, MmregError> {
        let dev = Device::uio(name)
            .with_read_only(!unmask)
            .with_force(force)
            .open()?;
        for w in dev.warnings() {
            eprintln!("{w}");
        }
        if unmask {
            dev.irq_unmask()?;
        }
        let duration = timeout_ms.map_or(Duration::MAX, Duration::from_millis);
        dev.irq_wait(duration)
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

/// Parses a count, interval or timeout. Quantities are decimal (`0x` selects hex), unlike addresses,
/// offsets and values, whose default is hex.
fn parse_quantity(raw: &str) -> Option<u64> {
    parse_u64_radix(raw, 10).ok()
}

fn parse_count(raw: &str) -> Result<usize, String> {
    // At most `usize::MAX / 8` words, so the byte size at any width fits `usize`.
    let val = parse_quantity(raw)
        .and_then(|v| usize::try_from(v).ok())
        .filter(|&n| (1..=usize::MAX / 8).contains(&n));
    val.ok_or_else(|| {
        usage_error(&format!(
            "[E_CLI_INVALID_COUNT] Invalid count '{}' (expected integer >= 1)",
            raw
        ))
    })
}

fn parse_interval(raw: &str) -> Result<u64, String> {
    let val = parse_quantity(raw).filter(|&ms| ms >= 1);
    val.ok_or_else(|| {
        usage_error(&format!(
            "[E_CLI_INVALID_INTERVAL] Invalid interval '{}' (expected integer >= 1 ms)",
            raw
        ))
    })
}

fn parse_format(raw: &str) -> Result<OutputFormat, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "hex" => Ok(OutputFormat::Hex),
        "dec" => Ok(OutputFormat::Dec),
        "bin" => Ok(OutputFormat::Bin),
        "json" => Ok(OutputFormat::Json),
        _ => Err(usage_error(&format!(
            "[E_CLI_INVALID_FORMAT] Invalid format '{}' (expected hex, dec, bin, or json)",
            raw
        ))),
    }
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

fn parse_target<'a>(
    cmd: &str,
    operands: &[&'a str],
    default_radix: u32,
) -> Result<CliTarget<'a>, String> {
    match operands {
        [] => Err(usage_error("[E_CLI_MISSING_ADDR] Missing address argument")),
        [addr] => {
            let address = parse_u64_radix(addr, default_radix).map_err(|_| {
                format!(
                    "[E_CLI_INVALID_ADDR] Invalid address '{}' (expected 64-bit hex address, e.g., 0x4000_0000, or 'mmreg {} <uio-name> <offset>')",
                    addr, cmd
                )
            })?;
            Ok(CliTarget::Address(address))
        }
        [first, offset_arg] => {
            if looks_like_address(first, default_radix) {
                return Err(usage_error("[E_CLI_EXTRA_ARGS] Too many arguments"));
            }
            let offset = parse_u32_radix(offset_arg, default_radix).map_err(|_| {
                format!(
                    "[E_CLI_INVALID_OFFSET] Invalid offset '{}' (expected 32-bit hex offset, e.g., 0x04, or decimal with 0d/--dec)",
                    offset_arg
                )
            })?;
            Ok(CliTarget::Uio {
                name: first,
                offset,
            })
        }
        _ => Err(usage_error("[E_CLI_EXTRA_ARGS] Too many arguments")),
    }
}

fn format_hex(value: u64, width: Width) -> String {
    match width {
        Width::U8 => format!("0x{:02X}", value as u8),
        Width::U16 => format!("0x{:04X}", value as u16),
        Width::U32 => format!("0x{:08X}", value as u32),
        Width::U64 => format!("0x{:016X}", value),
    }
}

fn format_bin(value: u64, width: Width) -> String {
    match width {
        Width::U8 => format!("0b{:08b}", value as u8),
        Width::U16 => format!("0b{:016b}", value as u16),
        Width::U32 => format!("0b{:032b}", value as u32),
        Width::U64 => format!("0b{:064b}", value),
    }
}

fn format_scalar(value: u64, width: Width, fmt: OutputFormat) -> String {
    match fmt {
        OutputFormat::Hex | OutputFormat::Json => format_hex(value, width),
        OutputFormat::Dec => value.to_string(),
        OutputFormat::Bin => format_bin(value, width),
    }
}

fn escape_json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn json_target_fields(target: &CliTarget<'_>) -> String {
    match target {
        CliTarget::Address(address) => format!("\"address\":\"0x{:X}\"", address),
        CliTarget::Uio { name, offset } => format!(
            "\"uio\":\"{}\",\"offset\":\"0x{:X}\"",
            escape_json_str(name),
            offset
        ),
    }
}

fn format_target_label(target: &CliTarget<'_>, byte_offset: u64) -> String {
    match target {
        CliTarget::Address(base) => format!("0x{:08X}", base.wrapping_add(byte_offset)),
        CliTarget::Uio { name, offset } => {
            format!(
                "{}+0x{:04X}",
                name,
                (*offset as u64).wrapping_add(byte_offset)
            )
        }
    }
}

fn format_single_read(
    target: &CliTarget<'_>,
    width: Width,
    value: u64,
    fmt: OutputFormat,
) -> String {
    match fmt {
        OutputFormat::Json => format!(
            "{{{0},\"width\":{1},\"value\":{2},\"hex\":\"{3}\"}}",
            json_target_fields(target),
            width.bits(),
            value,
            format_hex(value, width)
        ),
        _ => format_scalar(value, width, fmt),
    }
}

fn format_block_read(
    target: &CliTarget<'_>,
    width: Width,
    values: &[u64],
    fmt: OutputFormat,
) -> String {
    let step = width.bytes() as u64;
    if fmt == OutputFormat::Json {
        let items: Vec<String> = values
            .iter()
            .enumerate()
            .map(|(i, &val)| {
                let delta = (i as u64) * step;
                let loc = match target {
                    CliTarget::Address(base) => {
                        format!("\"address\":\"0x{:X}\"", base.wrapping_add(delta))
                    }
                    CliTarget::Uio { offset, .. } => {
                        format!(
                            "\"offset\":\"0x{:X}\"",
                            (*offset as u64).wrapping_add(delta)
                        )
                    }
                };
                format!(
                    "{{{loc},\"value\":{val},\"hex\":\"{}\"}}",
                    format_hex(val, width)
                )
            })
            .collect();
        return format!(
            "{{{0},\"width\":{1},\"count\":{2},\"values\":[{3}]}}",
            json_target_fields(target),
            width.bits(),
            values.len(),
            items.join(",")
        );
    }

    // Hexdump-style rows: up to 16 bytes per row (16x u8, 8x u16, 4x u32, 2x u64).
    let words_per_row = (16 / width.bytes()).max(1);
    let mut lines = Vec::with_capacity(values.len().div_ceil(words_per_row));
    for (row_idx, chunk) in values.chunks(words_per_row).enumerate() {
        let byte_off = (row_idx * words_per_row) as u64 * step;
        let label = format_target_label(target, byte_off);
        let words: Vec<String> = chunk
            .iter()
            .map(|&v| format_scalar(v, width, fmt))
            .collect();
        lines.push(format!("{}: {}", label, words.join(" ")));
    }
    lines.join("\n")
}

fn format_write_output(
    target: &CliTarget<'_>,
    width: Width,
    value: u64,
    fmt: OutputFormat,
) -> String {
    if fmt == OutputFormat::Json {
        return format!(
            "{{{0},\"width\":{1},\"value\":{2},\"hex\":\"{3}\"}}",
            json_target_fields(target),
            width.bits(),
            value,
            format_hex(value, width)
        );
    }
    let formatted = format_scalar(value, width, fmt);
    match target {
        CliTarget::Address(address) => format!("Wrote {} to 0x{:X}", formatted, address),
        CliTarget::Uio { name, offset } => {
            format!("Wrote {} to {}+0x{:X}", formatted, name, offset)
        }
    }
}

fn format_watch_event(
    target: &CliTarget<'_>,
    width: Width,
    poll: usize,
    prev: Option<u64>,
    value: u64,
    fmt: OutputFormat,
) -> String {
    if fmt == OutputFormat::Json {
        let prev_fields = match prev {
            Some(p) => format!(
                ",\"previous\":{},\"previous_hex\":\"{}\"",
                p,
                format_hex(p, width)
            ),
            None => String::new(),
        };
        return format!(
            "{{{0},\"width\":{1},\"poll\":{2},\"value\":{3},\"hex\":\"{4}\"{5}}}",
            json_target_fields(target),
            width.bits(),
            poll,
            value,
            format_hex(value, width),
            prev_fields
        );
    }
    let label = format_target_label(target, 0);
    let cur_str = format_scalar(value, width, fmt);
    match prev {
        None => format!("{}: {}", label, cur_str),
        Some(p) => format!("{}: {} -> {}", label, format_scalar(p, width, fmt), cur_str),
    }
}

fn format_irq_output(name: &str, count: u32, fmt: OutputFormat) -> String {
    match fmt {
        OutputFormat::Json => format!(
            "{{\"uio\":\"{}\",\"count\":{},\"hex\":\"0x{:08X}\"}}",
            escape_json_str(name),
            count,
            count
        ),
        OutputFormat::Hex => format!("0x{:08X}", count),
        OutputFormat::Dec => count.to_string(),
        OutputFormat::Bin => format!("0b{:032b}", count),
    }
}

/// Runs the CLI against `backend`, invoking `emit` for streaming `watch` lines when desired.
/// Returns `Ok(stdout_message)` on success or `Err(stderr_message)` on failure.
fn run_cli_streaming<B, E>(
    args: &[String],
    backend: &mut B,
    mut emit: Option<E>,
) -> Result<String, String>
where
    B: CliBackend,
    E: FnMut(&str),
{
    if args[1..].iter().any(|a| a == "-h" || a == "--help") {
        return Ok(usage_text().to_string());
    }
    if args[1..].iter().any(|a| a == "-V" || a == "--version") {
        return Ok(version_text());
    }

    let mut force_map = false;
    let mut default_radix: u32 = 16;
    let mut width = Width::U32;
    let mut raw_count: Option<&str> = None;
    let mut raw_interval: Option<&str> = None;
    let mut format = OutputFormat::Hex;
    let mut unmask = false;
    let mut validate_paths: Vec<&str> = Vec::new();
    let mut positionals: Vec<&str> = Vec::new();
    let mut idx = 1;

    while let Some(arg) = args.get(idx) {
        match arg.as_str() {
            "-f" | "--force" => force_map = true,
            "-d" | "--dec" => default_radix = 10,
            "-u" | "--unmask" => unmask = true,
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
            "-c" | "--count" => {
                idx += 1;
                let c_arg = args.get(idx).ok_or_else(|| {
                    usage_error(&format!(
                        "[E_CLI_MISSING_COUNT] Missing count after {}",
                        arg
                    ))
                })?;
                raw_count = Some(c_arg);
            }
            "-i" | "--interval" => {
                idx += 1;
                let i_arg = args.get(idx).ok_or_else(|| {
                    usage_error(&format!(
                        "[E_CLI_MISSING_INTERVAL] Missing interval (ms) after {}",
                        arg
                    ))
                })?;
                raw_interval = Some(i_arg);
            }
            "-F" | "--format" => {
                idx += 1;
                let f_arg = args.get(idx).ok_or_else(|| {
                    usage_error(&format!(
                        "[E_CLI_MISSING_FORMAT] Missing format after {}",
                        arg
                    ))
                })?;
                format = parse_format(f_arg)?;
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

    let count = match raw_count {
        Some(raw) => Some(parse_count(raw)?),
        None => None,
    };
    let interval_ms = match raw_interval {
        Some(raw) => Some(parse_interval(raw)?),
        None => None,
    };

    // Parse every operand before touching hardware (a profile's checks read registers).
    let command = match positionals.as_slice() {
        [] => return Err(usage_error("[E_CLI_MISSING_CMD] Missing command")),
        ["read", rest @ ..] => {
            if interval_ms.is_some() || unmask {
                return Err(usage_error(
                    "[E_CLI_OPT_CONFLICT] --interval and --unmask cannot be used with read",
                ));
            }
            CliCommand::Read {
                target: parse_target("read", rest, default_radix)?,
            }
        }
        ["watch", rest @ ..] => {
            if unmask {
                return Err(usage_error(
                    "[E_CLI_OPT_CONFLICT] --unmask cannot be used with watch",
                ));
            }
            CliCommand::Watch {
                target: parse_target("watch", rest, default_radix)?,
            }
        }
        ["write", rest @ ..] => {
            if count.is_some() || interval_ms.is_some() || unmask {
                return Err(usage_error(
                    "[E_CLI_OPT_CONFLICT] --count, --interval, and --unmask cannot be used with write",
                ));
            }
            match rest {
                [] => return Err(usage_error("[E_CLI_MISSING_ADDR] Missing address argument")),
                [_] => {
                    return Err(usage_error(
                        "[E_CLI_MISSING_VAL] Missing value argument for write",
                    ))
                }
                [addr, val_arg] => {
                    let target = parse_target("write", &[addr], default_radix)?;
                    let value = parse_value(val_arg, default_radix, width)?;
                    CliCommand::Write { target, value }
                }
                [first, offset_arg, val_arg] => {
                    let target = parse_target("write", &[first, offset_arg], default_radix)?;
                    let value = parse_value(val_arg, default_radix, width)?;
                    CliCommand::Write { target, value }
                }
                _ => return Err(usage_error("[E_CLI_EXTRA_ARGS] Too many arguments")),
            }
        }
        ["irq", rest @ ..] => {
            if count.is_some() || interval_ms.is_some() {
                return Err(usage_error(
                    "[E_CLI_OPT_CONFLICT] --count and --interval cannot be used with irq",
                ));
            }
            match rest {
                [] => {
                    return Err(usage_error(
                        "[E_CLI_MISSING_UIO] Missing UIO device name for irq",
                    ))
                }
                [name] => CliCommand::Irq {
                    name,
                    timeout_ms: None,
                },
                [name, timeout_arg] => {
                    let ms = parse_quantity(timeout_arg).ok_or_else(|| {
                        format!(
                            "[E_CLI_INVALID_TIMEOUT] Invalid timeout '{}' (expected non-negative integer milliseconds)",
                            timeout_arg
                        )
                    })?;
                    CliCommand::Irq {
                        name,
                        timeout_ms: Some(ms),
                    }
                }
                _ => return Err(usage_error("[E_CLI_EXTRA_ARGS] Too many arguments")),
            }
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

    match command {
        CliCommand::Read { target } => match count {
            None => {
                let val = backend
                    .read(target.clone(), width, force_map)
                    .map_err(|e| format!("Read error: {}", e))?;
                Ok(format_single_read(&target, width, val, format))
            }
            Some(n) => {
                let vals = backend
                    .read_block(target.clone(), width, n, force_map)
                    .map_err(|e| format!("Read error: {}", e))?;
                Ok(format_block_read(&target, width, &vals, format))
            }
        },
        CliCommand::Write { target, value } => {
            backend
                .write(target.clone(), width, value, force_map)
                .map_err(|e| format!("Write error: {}", e))?;
            Ok(format_write_output(&target, width, value, format))
        }
        CliCommand::Watch { target } => {
            let mut lines: Vec<String> = Vec::new();
            let poll_interval = interval_ms.unwrap_or(100);
            backend
                .watch(
                    target.clone(),
                    width,
                    poll_interval,
                    count,
                    force_map,
                    |poll, prev, val| {
                        let line = format_watch_event(&target, width, poll, prev, val, format);
                        if let Some(ref mut cb) = emit {
                            cb(&line);
                        } else {
                            lines.push(line);
                        }
                    },
                )
                .map_err(|e| format!("Watch error: {}", e))?;
            Ok(lines.join("\n"))
        }
        CliCommand::Irq { name, timeout_ms } => {
            let res = backend
                .irq_wait(name, timeout_ms, unmask, force_map)
                .map_err(|e| format!("IRQ error: {}", e))?;
            match res {
                Some(irq_count) => Ok(format_irq_output(name, irq_count, format)),
                None => Err(format!(
                    "[E_CLI_IRQ_TIMEOUT] Timed out after {} ms waiting for interrupt on UIO device '{}'",
                    timeout_ms.unwrap_or(0),
                    name
                )),
            }
        }
    }
}

#[cfg(test)]
fn run_cli<B: CliBackend>(args: &[String], backend: &mut B) -> Result<String, String> {
    run_cli_streaming(args, backend, None::<fn(&str)>)
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

/// Exit status for an error message: 2 when `irq` timed out (no interrupt, not a failure), else 1.
fn exit_status(err: &str) -> i32 {
    if err.starts_with("[E_CLI_IRQ_TIMEOUT]") {
        2
    } else {
        1
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut streamed = false;
    let res = run_cli_streaming(
        &args,
        &mut HardwareBackend,
        Some(|line: &str| {
            streamed = true;
            println!("{}", line);
        }),
    );
    match res {
        Ok(out) => {
            if !streamed && !out.is_empty() {
                println!("{}", out);
            }
        }
        Err(err) => {
            eprintln!("{}", err);
            process::exit(exit_status(&err));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FnBackend<R, W> {
        read_fn: R,
        write_fn: Option<W>,
        watch_values: Vec<u64>,
        irq_result: Result<Option<u32>, MmregError>,
        last_unmask: bool,
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

        fn read_block(
            &mut self,
            target: CliTarget<'_>,
            width: Width,
            count: usize,
            force: bool,
        ) -> Result<Vec<u64>, MmregError> {
            let step = width.bytes();
            let mut out = Vec::with_capacity(count);
            for i in 0..count {
                let item_target = match &target {
                    CliTarget::Address(base) => CliTarget::Address(base + (i * step) as u64),
                    CliTarget::Uio { name, offset } => CliTarget::Uio {
                        name,
                        offset: offset + (i * step) as u32,
                    },
                };
                out.push((self.read_fn)(item_target, width, force)?);
            }
            Ok(out)
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

        fn watch<F>(
            &mut self,
            target: CliTarget<'_>,
            width: Width,
            _interval_ms: u64,
            max_polls: Option<usize>,
            force: bool,
            on_change: F,
        ) -> Result<(), MmregError>
        where
            F: FnMut(usize, Option<u64>, u64),
        {
            // Scripted values (the last one repeats), else `read_fn`, through the CLI's own loop.
            let values = self.watch_values.clone();
            let read_fn = &mut self.read_fn;
            let mut poll = 0;
            poll_changes(
                || {
                    let value = match values.get(poll.min(values.len().saturating_sub(1))) {
                        Some(&v) => v,
                        None => read_fn(target.clone(), width, force)?,
                    };
                    poll += 1;
                    Ok(value)
                },
                Duration::ZERO,
                Some(max_polls.unwrap_or(values.len().max(1))),
                on_change,
            )
        }

        fn irq_wait(
            &mut self,
            _name: &str,
            _timeout_ms: Option<u64>,
            unmask: bool,
            _force: bool,
        ) -> Result<Option<u32>, MmregError> {
            self.last_unmask = unmask;
            self.irq_result.clone()
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
            watch_values: Vec::new(),
            irq_result: Ok(Some(42)),
            last_unmask: false,
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
        assert!(
            ver.starts_with(concat!("mmreg ", env!("CARGO_PKG_VERSION")))
                && ver.contains("features:")
        );
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
        assert!(run(&["mmreg", "--count"])
            .unwrap_err()
            .contains("[E_CLI_MISSING_COUNT]"));
        assert!(run(&["mmreg", "--count", "0", "read", "0x4000_0000"])
            .unwrap_err()
            .contains("[E_CLI_INVALID_COUNT]"));
        assert!(run(&["mmreg", "--interval"])
            .unwrap_err()
            .contains("[E_CLI_MISSING_INTERVAL]"));
        assert!(run(&["mmreg", "--interval", "0", "watch", "0x4000_0000"])
            .unwrap_err()
            .contains("[E_CLI_INVALID_INTERVAL]"));
        assert!(run(&["mmreg", "--format"])
            .unwrap_err()
            .contains("[E_CLI_MISSING_FORMAT]"));
        assert!(run(&["mmreg", "--format", "yaml", "read", "0x4000_0000"])
            .unwrap_err()
            .contains("[E_CLI_INVALID_FORMAT]"));
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

    #[test]
    fn test_cli_formats_and_block_read() {
        // Single read formats: dec, bin, json
        assert_eq!(
            run(&["mmreg", "-F", "dec", "read", "0x4000_0000"]).unwrap(),
            "305419896"
        );
        assert_eq!(
            run(&["mmreg", "-w", "8", "-F", "bin", "read", "0x4000_0000"]).unwrap(),
            "0b01111000"
        );
        assert_eq!(
            run(&["mmreg", "--format", "json", "read", "0x4000_0000"]).unwrap(),
            r#"{"address":"0x40000000","width":32,"value":305419896,"hex":"0x12345678"}"#
        );
        assert_eq!(
            run(&["mmreg", "-F", "json", "read", "axi_gpio", "0x04"]).unwrap(),
            r#"{"uio":"axi_gpio","offset":"0x4","width":32,"value":2684354564,"hex":"0xA0000004"}"#
        );

        // Write with --format json / dec / bin
        assert_eq!(
            run(&["mmreg", "-F", "json", "write", "0x4000_0000", "0xCAFE_BABE"]).unwrap(),
            r#"{"address":"0x40000000","width":32,"value":3405691582,"hex":"0xCAFEBABE"}"#
        );
        assert_eq!(
            run(&["mmreg", "-F", "dec", "-w", "8", "write", "axi_gpio", "0x08", "0x55"]).unwrap(),
            "Wrote 85 to axi_gpio+0x8"
        );

        // Block read (--count N): hexdump rows (4 words per row for u32) and JSON
        let block_out = run(&["mmreg", "read", "axi_gpio", "0x00", "--count", "6"]).unwrap();
        assert_eq!(
            block_out,
            "axi_gpio+0x0000: 0xA0000000 0xA0000004 0xA0000008 0xA000000C\n\
             axi_gpio+0x0010: 0xA0000010 0xA0000014"
        );

        let block_json =
            run(&["mmreg", "-F", "json", "read", "axi_gpio", "0x00", "-c", "2"]).unwrap();
        assert_eq!(
            block_json,
            r#"{"uio":"axi_gpio","offset":"0x0","width":32,"count":2,"values":[{"offset":"0x0","value":2684354560,"hex":"0xA0000000"},{"offset":"0x4","value":2684354564,"hex":"0xA0000004"}]}"#
        );
    }

    #[test]
    fn test_cli_quantities_are_decimal_and_conflicts_and_exit_status() {
        // `--count 10` is ten words, not 0x10, whatever the default radix of addresses.
        let rows = run(&["mmreg", "read", "axi_gpio", "0x00", "--count", "10"]).unwrap();
        assert_eq!(
            rows.split_whitespace()
                .filter(|w| w.starts_with("0xA"))
                .count(),
            10
        );
        let rows = run(&["mmreg", "--count", "0x10", "read", "axi_gpio", "0x00"]).unwrap();
        assert_eq!(
            rows.split_whitespace()
                .filter(|w| w.starts_with("0xA"))
                .count(),
            16
        );
        assert!(run(&["mmreg", "--count", "1a", "read", "0x4000_0000"])
            .unwrap_err()
            .contains("[E_CLI_INVALID_COUNT]"));
        assert!(run(&["mmreg", "--interval", "ff", "watch", "0x4000_0000"])
            .unwrap_err()
            .contains("[E_CLI_INVALID_INTERVAL]"));
        assert_eq!(parse_interval("50"), Ok(50));
        assert_eq!(parse_interval("0x32"), Ok(50));

        assert!(run(&["mmreg", "--unmask", "watch", "0x4000_0000"])
            .unwrap_err()
            .contains("[E_CLI_OPT_CONFLICT]"));
        assert!(run(&["mmreg", "--count", "2", "irq", "axi_timer"])
            .unwrap_err()
            .contains("[E_CLI_OPT_CONFLICT]"));

        assert_eq!(exit_status("[E_CLI_IRQ_TIMEOUT] Timed out"), 2);
        assert_eq!(exit_status("IRQ error: [E_UIO_NOT_FOUND] ..."), 1);
    }

    #[test]
    fn test_cli_watch_and_irq() {
        let mut backend = FnBackend {
            read_fn: |_: CliTarget<'_>, _: Width, _: bool| Ok(0),
            write_fn: None::<fn(CliTarget<'_>, Width, u64, bool) -> Result<(), MmregError>>,
            watch_values: vec![0x10, 0x10, 0x25, 0x25, 0x30],
            irq_result: Ok(Some(7)),
            last_unmask: false,
        };

        // watch prints initial reading and changes only
        let watch_out = run_cli(
            &to_args(&[
                "mmreg",
                "watch",
                "0x4000_0000",
                "--interval",
                "10",
                "--count",
                "5",
            ]),
            &mut backend,
        )
        .unwrap();
        assert_eq!(
            watch_out,
            "0x40000000: 0x00000010\n\
             0x40000000: 0x00000010 -> 0x00000025\n\
             0x40000000: 0x00000025 -> 0x00000030"
        );

        // watch with --format json
        let watch_json = run_cli(
            &to_args(&[
                "mmreg", "-F", "json", "watch", "axi_gpio", "0x04", "-i", "5", "-c", "3",
            ]),
            &mut backend,
        )
        .unwrap();
        assert_eq!(
            watch_json,
            "{\"uio\":\"axi_gpio\",\"offset\":\"0x4\",\"width\":32,\"poll\":0,\"value\":16,\"hex\":\"0x00000010\"}\n\
             {\"uio\":\"axi_gpio\",\"offset\":\"0x4\",\"width\":32,\"poll\":2,\"value\":37,\"hex\":\"0x00000025\",\"previous\":16,\"previous_hex\":\"0x00000010\"}"
        );

        // irq success (hex, dec, json) and --unmask
        let irq_hex = run_cli(
            &to_args(&["mmreg", "--unmask", "irq", "axi_timer", "500"]),
            &mut backend,
        )
        .unwrap();
        assert!(backend.last_unmask);
        assert_eq!(irq_hex, "0x00000007");

        let irq_dec = run_cli(
            &to_args(&["mmreg", "-F", "dec", "irq", "axi_timer"]),
            &mut backend,
        )
        .unwrap();
        assert!(!backend.last_unmask);
        assert_eq!(irq_dec, "7");

        let irq_json = run_cli(
            &to_args(&["mmreg", "-F", "json", "irq", "axi_timer", "1000"]),
            &mut backend,
        )
        .unwrap();
        assert_eq!(
            irq_json,
            r#"{"uio":"axi_timer","count":7,"hex":"0x00000007"}"#
        );

        // irq timeout returns [E_CLI_IRQ_TIMEOUT]
        backend.irq_result = Ok(None);
        let err = run_cli(
            &to_args(&["mmreg", "irq", "axi_timer", "250"]),
            &mut backend,
        )
        .unwrap_err();
        assert!(err.contains("[E_CLI_IRQ_TIMEOUT]"), "{err}");

        // irq argument validation
        assert!(run_cli(&to_args(&["mmreg", "irq"]), &mut backend)
            .unwrap_err()
            .contains("[E_CLI_MISSING_UIO]"));
        assert!(run_cli(
            &to_args(&["mmreg", "irq", "axi_timer", "bad_ms"]),
            &mut backend
        )
        .unwrap_err()
        .contains("[E_CLI_INVALID_TIMEOUT]"));
        assert!(run_cli(
            &to_args(&["mmreg", "irq", "axi_timer", "100", "extra"]),
            &mut backend
        )
        .unwrap_err()
        .contains("[E_CLI_EXTRA_ARGS]"));
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

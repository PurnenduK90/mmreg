# 3. CLI Usage Guide

`mmreg` provides a command-line interface for reading, writing, watching, and waiting on interrupts
for 8-, 16-, 32-, and 64-bit memory-mapped registers by **physical address** or by **UIO device
name + byte offset**, with blueprint validation, hardware pre-flight checks, and inter-process
locking enabled by default.

---

## Command Syntax

```text
Usage:
  mmreg [options] <read|write|watch> <address> [value]
  mmreg [options] <read|write|watch> <uio-name> <offset> [value]
  mmreg [options] irq <uio-name> [timeout_ms]

Options:
  -w, --width <8|16|32|64>         Access width in bits (default: 32)
  -c, --count <N>                  Word count for read, or poll limit for watch
  -i, --interval <ms>              Poll interval in ms for watch (default: 100)
  -F, --format <hex|dec|bin|json>  Output format (default: hex)
  -u, --unmask                     Unmask the UIO interrupt line before waiting in irq
  -f, --force                      Skip address blueprint and hardware pre-flight validation
  -v, --validate <file>            Run a JSON Safety Profile's checks before the access (may be repeated)
  -d, --dec                        Interpret unprefixed numbers as decimal instead of hexadecimal
  -V, --version                    Print version and enabled features
  -h, --help                       Print this help message

Commands:
  read  <address>                   Read value(s) at physical address (--count N for block read)
  read  <uio-name> <offset>         Read value(s) at byte offset in UIO device
  write <address> <value>           Write value to physical address
  write <uio-name> <offset> <value> Write value to byte offset in UIO device
  watch <address>                   Poll physical address and print changes
  watch <uio-name> <offset>         Poll byte offset in UIO device and print changes
  irq   <uio-name> [timeout_ms]     Wait for a UIO interrupt (optional timeout in ms)
```

Options can appear before or after the command. All arguments are validated before any hardware
access occurs (including before `--validate` profile reads). Extra arguments are an error
(`[E_CLI_EXTRA_ARGS]`), so a mistyped `read` with a value is never silently ignored, and so is an
option the command does not use (`[E_CLI_OPT_CONFLICT]`, e.g. `--unmask` with `watch`).

**Exit status:** `0` on success, `2` when `irq` times out without an interrupt, `1` on any other
error. Errors, and non-fatal pre-flight warnings, go to stderr with an `[E_*]` code (see
[Troubleshooting](06-troubleshooting.md)); results go to stdout.

### Number Formats (Hexadecimal vs. Decimal)
- **Default (Hexadecimal):** Unprefixed addresses, offsets and values (`40000000`) and `0x`/`0X`-prefixed numbers (`0x4000_0000`) are parsed as base-16.
- **Quantities are decimal:** `--count`, `--interval` and the `irq` timeout are counts and milliseconds, parsed as base-10 whatever `--dec` says (`0x` still selects hex): `--count 10` is ten words, `--interval 50` is 50 ms.
- **Explicit Decimal Prefix (`0d` / `0D`):** Prefixing any number with `0d` or `0D` (e.g., `0d1024`) parses that specific argument as base-10, even without `--dec`.
- **Global Decimal Flag (`-d` / `--dec`):** Passing `-d` or `--dec` changes the default base for unprefixed numbers to base-10; arguments explicitly prefixed with `0x` are still parsed as hexadecimal.
- **Visual Separators:** Underscores (`_`) are permitted anywhere between digits (`0xFF5E_005C`, `0d1_000_000`).
- **Rejected:** signs (`+1`, `-1`) and repeated prefixes (`0x0x10`).
- **Alignment:** Every access must be naturally aligned to its width (`1` byte for `-w 8`, `2` bytes for `-w 16`, `4` bytes for `-w 32`, and `8` bytes for `-w 64`).

---

## 1. Reading Registers (`read` and `--count N`)

Read a register at a physical address or at a byte offset inside a UIO device (`read` opens the
device in read-only mode, requiring only `0640` permissions on `/dev/uioN`):

```sh
# 32-bit read at a physical address (uses a covering UIO region if present, or /dev/mem in `debug` builds)
sudo mmreg read 0x4000_0000
# Output:
# 0x00000000

# Read at a byte offset inside a UIO device by name (works in default builds without `debug` or sudo)
mmreg read axi_gpio 0x04
# Output:
# 0x00000001

# 8-bit, 16-bit, and 64-bit reads
mmreg -w 8  read axi_gpio 0x01
mmreg -w 16 read axi_gpio 0x02
mmreg -w 64 read axi_gpio 0x08
```

### Block Read (`--count N` / `-c N`)

Pass `-c` / `--count <N>` to read `N` consecutive words starting at `<address>` or `<uio-name> <offset>`
in a hexdump-style table (`16` bytes per line: `16`×`u8`, `8`×`u16`, `4`×`u32`, or `2`×`u64`).
The CLI maps the window once and reads it with the library's `Device::read_block`, which holds
**one lock per page** (of the kernel's page size) across the words in that page, so other programs
wait for at most one page:

```sh
mmreg read axi_gpio 0x00 --count 6
# Output:
# axi_gpio+0x0000: 0x00000001 0x00000000 0x000000FF 0x00000000
# axi_gpio+0x0010: 0x00000000 0x00000003
```

---

## 2. Writing Registers (`write`)

Write an 8-, 16-, 32-, or 64-bit value to a physical address or UIO device offset:

```sh
# Write 32-bit hex value to a UIO device at offset 0x00
mmreg write axi_gpio 0x00 0x0000_0001
# Output:
# Wrote 0x00000001 to axi_gpio+0x0

# Write a 16-bit value at offset 0x06
mmreg --width 16 write axi_gpio 0x06 0xCAFE
# Output:
# Wrote 0xCAFE to axi_gpio+0x6

# Write a decimal value using the 0d prefix or --dec flag
sudo mmreg write 0x4000_0000 0d255
sudo mmreg --dec write 0x4000_0000 255
# Output:
# Wrote 0x000000FF to 0x40000000
```

---

## 3. Watching a Register for Changes (`watch`)

Poll a register at a rate-limited interval (`-i` / `--interval <ms>`, default `100` ms) and print
its initial value followed by every transition (`old -> new`). Pass `-c` / `--count <N>` to stop
after `N` poll iterations (useful in scripts and tests); omit `--count` to poll until interrupted
with `Ctrl-C`:

```sh
# Poll offset 0x04 on axi_gpio every 50 ms
mmreg watch axi_gpio 0x04 --interval 50
# Output:
# axi_gpio+0x0004: 0x00000000
# axi_gpio+0x0004: 0x00000000 -> 0x00000001
# axi_gpio+0x0004: 0x00000001 -> 0x00000003
```

> [!WARNING]
> `watch` reads the register on every poll. The CLI works by address and does not know a
> register's access type, so watching a read-to-clear status register or a FIFO clears it or pops
> an entry on every poll. Watch such registers from Python by name (`d.watch("IRQ_STATUS")`,
> [Block Files](07-block-files.md)), which refuses read side effects unless `side_effects=True`.

---

## 4. Waiting for a UIO Interrupt (`irq`)

Wait for an interrupt on `<uio-name>` from the shell (`poll(POLLIN)` + 4-byte `read` on `/dev/uioN`).
An optional `timeout_ms` (decimal milliseconds) bounds the wait; if the timeout elapses before an
interrupt arrives, `mmreg` prints `[E_CLI_IRQ_TIMEOUT]` and exits with status `2`, so scripts can tell
"no interrupt" (`2`) from a failure (`1`). Pass `-u` / `--unmask`
to unmask the interrupt line (`write(fd, &1u32, 4)`) before waiting:

```sh
# Unmask and wait up to 5000 ms for an interrupt on axi_timer
mmreg --unmask irq axi_timer 5000
# Output (cumulative kernel interrupt count):
# 0x00000007
```

---

## 5. Output Formats (`--format hex|dec|bin|json`)

Select the output representation with `-F` / `--format`:
- `hex` (default): zero-padded hexadecimal (`0x12345678`)
- `dec`: unsigned decimal integer (`305419896`)
- `bin`: zero-padded binary (`0b00010010001101000101011001111000`)
- `json`: structured single-line JSON (or JSON Lines for `watch`), suitable for scripts and agents:

```sh
mmreg --format json read axi_gpio 0x04
# {"uio":"axi_gpio","offset":"0x4","width":32,"value":1,"hex":"0x00000001"}

mmreg --format json read axi_gpio 0x00 --count 2
# {"uio":"axi_gpio","offset":"0x0","width":32,"count":2,"values":[{"offset":"0x0","value":1,"hex":"0x00000001"},{"offset":"0x4","value":0,"hex":"0x00000000"}]}

mmreg --format json irq axi_timer 1000
# {"uio":"axi_timer","count":7,"hex":"0x00000007"}
```

---

## 6. Using Declarative Safety Profiles (`--validate`)

Before executing the target read or write, you can require that one or more declarative JSON safety
profiles pass (e.g., verifying that a clock or reset controller register bit is asserted):

```sh
sudo mmreg --validate zynqmp_gem3.json read 0xFF0E_0000
```

See [5. Safety & Hardware Pre-Flight](05-safety-and-preflight.md) for the JSON profile schema.

---

## 7. Bypassing Pre-Flight Validation (`--force`)

If you are bringing up a new board whose Device Tree node is not yet populated or where `/proc/iomem`
is hidden, you can bypass Blueprint and Hardware Pre-Flight checks with `-f` or `--force`:

```sh
sudo mmreg --force read 0x4000_0000
```

> [!CAUTION]
> `--force` bypasses Blueprint and Zombie Hardware Pre-Flight checks. If the target peripheral's clock or power domain is gated, accessing it with `--force` can still cause a hard bus hang. Note that natural width alignment and any explicitly supplied `--validate` profiles are **still enforced** even when `--force` is passed.

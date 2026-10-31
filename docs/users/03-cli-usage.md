# 3. CLI Usage

The `mmreg` command reads and writes one 8-, 16-, 32-, or 64-bit register by physical address or by
UIO device name and byte offset, after the blueprint and hardware pre-flight checks described in
[Safety & Hardware Pre-Flight](05-safety-and-preflight.md).

---

## Synopsis

```text
Usage:
  mmreg [options] <read|write> <address> [value]
  mmreg [options] <read|write> <uio-name> <offset> [value]
Options:
  -w, --width <8|16|32|64>  Access width in bits (default: 32)
  -f, --force               Skip address blueprint and hardware pre-flight validation
  -v, --validate <file>     Run a JSON Safety Profile's checks before the access (may be repeated)
  -d, --dec                 Interpret unprefixed numbers as decimal instead of hexadecimal
  -V, --version             Print version and enabled features
  -h, --help                Print this help message
Commands:
  read  <address>                   Read value at physical address
  read  <uio-name> <offset>         Read value at byte offset in UIO device
  write <address> <value>           Write value to physical address
  write <uio-name> <offset> <value> Write value to byte offset in UIO device
```

Extra arguments are an error (`[E_CLI_EXTRA_ARGS]`), so a mistyped `read` with a value is never
silently ignored.

---

## Numbers & Widths

- Unprefixed numbers are **hexadecimal** (`40000000`), or decimal with `-d` / `--dec`.
- `0x` forces hexadecimal and `0d` forces decimal, whatever the default (`0d100` is 100).
- Underscores may separate digits (`0x4000_0000`). Signs and repeated prefixes (`0x0x10`) are
  rejected.
- `-w` / `--width` selects `8`, `16`, `32` (default), or `64` bits. The target address or offset
  must be naturally aligned to the chosen width (`[E_UNALIGNED_ACCESS]`).
- Reads print zero-padded hexadecimal matching the width (`0xA1`, `0x00A1`, `0x000000A1`, or
  `0x00000000000000A1`).
- `mmreg read` opens UIO and `/dev/mem` descriptors read-only (`O_RDONLY` + `PROT_READ`), so
  non-root users only need read permission (`0440`/`0640`) on `/dev/uioN` to read registers.

The exit status is 0 on success and 1 on any error; errors (and any non-fatal pre-flight warnings)
go to stderr with an `[E_*]` code (see [Troubleshooting](06-troubleshooting.md)).

---

## Examples

### Read and Write by UIO Name and Offset
```sh
$ mmreg read axi_gpio 0x04
0x000000A1
$ mmreg --width 16 read axi_gpio 0x02
0x00A1
$ mmreg write axi_gpio 0x00 0x0000_0001
Wrote 0x00000001 to axi_gpio+0x0
```

Accessing a UIO device by name (`<uio-name> <offset>`) looks up `/sys/class/uio/uio*/name`, maps
region `map0`, and works in **all** builds (without the `debug` feature and without `sudo` when your
user has permission on `/dev/uioN`).

### Read and Write by Physical Address
```sh
$ sudo mmreg read 0x4000_0000
0x000000A1
$ sudo mmreg write 0x4000_0000 0xDEADBEEF
Wrote 0xDEADBEEF to 0x40000000
$ sudo mmreg --dec write 0x4000_0000 100
Wrote 0x00000064 to 0x40000000
```

`sudo` is needed for `/dev/mem` and to see real addresses in `/proc/iomem`; it is not needed for a
UIO device your user may open.

An address inside a UIO device's region works with any build. Any other address needs an `mmreg`
built with the `debug` feature (development images, prebuilt binaries); other builds refuse it with
`[E_DEBUG_DISABLED]` before opening anything. The same applies to the register reads of a safety
profile.

### Safety Profile (`--validate <profile.json>`)
A profile lists registers that must hold expected values before the target is touched, such as
clock, reset or isolation bits on always-on system-controller registers:

```json
{
  "name": "ZynqMP_AXI_Safety",
  "checks": [
    {
      "name": "PCAP_DONE",
      "addr": "0xFFCA_3010",
      "mask": "0x4",
      "expected": "0x4"
    },
    {
      "name": "ISOLATION_REMOVED",
      "addr": "0xFF41_0000",
      "mask": "0xF",
      "expected": "0x0"
    }
  ]
}
```

```sh
$ sudo mmreg --validate zynqmp_safety.json read 0xA000_0000
0x00000000
```

All operands are checked before the profile runs. The profile's own register reads skip the
blueprint and pre-flight checks (they target always-on registers by design), and the profile runs
even with `--force`. If a check's address is outside UIO regions, reading it uses `/dev/mem` and
therefore needs a `debug` build. `--validate` needs the `profile` feature (on by default); a minimal
build reports `[E_CLI_NO_PROFILE]`.

### Skipping Validation (`--force`)
During board bring-up, when a peripheral is intentionally missing from the Device Tree and
`/proc/iomem`, `-f` / `--force` skips the blueprint and pre-flight checks:

```sh
$ sudo mmreg --force read 0x43C0_0000
0x00000001
```

> [!WARNING]
> `--force` removes the protection against unmapped, unclocked, unpowered or unprogrammed
> hardware: such an access can still hang the bus or stall the kernel. Alignment is always checked.

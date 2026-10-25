# 3. CLI Usage

The `mmreg` command reads and writes one 32-bit register, after the blueprint and hardware
pre-flight checks described in [Safety & Hardware Pre-Flight](05-safety-and-preflight.md).

---

## Synopsis

```text
Usage: mmreg [options] <read|write> <address> [value]
Options:
  -f, --force               Skip address blueprint and hardware pre-flight validation
  -v, --validate <file>     Run a TOML Safety Profile's checks before the access
  -d, --dec                 Interpret unprefixed numbers as decimal instead of hexadecimal
  -h, --help                Print this help message
Commands:
  read  <address>           Read 32-bit value at address
  write <address> <value>   Write 32-bit value to address
```

Options come before the command. Extra arguments are an error (`[E_CLI_EXTRA_ARGS]`), so a
mistyped `read` with a value is never silently ignored.

---

## Numbers

- Unprefixed numbers are **hexadecimal** (`40000000`), or decimal with `-d` / `--dec`.
- `0x` forces hexadecimal and `0d` forces decimal, whatever the default (`0d100` is 100).
- Underscores may separate digits (`0x4000_0000`). Signs and repeated prefixes (`0x0x10`) are
  rejected.
- The address must be 4-byte aligned (`[E_UNALIGNED_ACCESS]`).
- Reads print zero-padded hexadecimal (`0x000000A1`).

The exit status is 0 on success and 1 on any error; errors go to stderr with an `[E_*]` code (see
[Troubleshooting](06-troubleshooting.md)).

---

## Examples

### Read and Write
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

### Safety Profile (`--validate <profile.toml>`)
A profile lists registers that must hold expected values before the target is touched, such as
clock, reset or isolation bits on always-on system-controller registers:

```toml
# zynqmp_safety.toml
name = "ZynqMP_AXI_Safety"

[[checks]]
name = "PCAP_DONE"
addr = 0xFFCA_3010
mask = 0x4
expected = 0x4

[[checks]]
name = "ISOLATION_REMOVED"
addr = 0xFF41_0000
mask = 0xF
expected = 0x0
```

```sh
$ sudo mmreg --validate zynqmp_safety.toml read 0xA000_0000
0x00000000
```

All operands are checked before the profile runs. The profile's own register reads skip the
blueprint and pre-flight checks (they target always-on registers by design), and the profile runs
even with `--force`. `--validate` needs the `profile` feature (on by default); a minimal build
reports `[E_CLI_NO_PROFILE]`.

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

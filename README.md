# mmreg
[![Documentation](https://docs.rs/mmreg/badge.svg)](https://docs.rs/mmreg)
[![Crates.io](https://img.shields.io/crates/v/mmreg.svg)](https://crates.io/crates/mmreg)
[![Downloads](https://img.shields.io/crates/d/mmreg.svg)](https://crates.io/crates/mmreg)
[![License](https://img.shields.io/crates/l/mmreg.svg)](LICENSE.txt)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-blue.svg?maxAge=3600)](https://www.rust-lang.org/)
[![Build Status](https://github.com/PurnenduK90/mmreg/workflows/CI/badge.svg)](https://github.com/PurnenduK90/mmreg/actions)

A Rust library and CLI for safe, concurrent access to 8/16/32/64-bit memory-mapped IO registers via
Linux Userspace I/O (`/dev/uioN`) and `/dev/mem`: a `devmem` and UIO helper-library replacement that
refuses accesses likely to hang the bus instead of performing them.

## Features
- **UIO first, `/dev/mem` only in debug builds**: open UIO devices directly by name
  (`Device::open_uio("axi_gpio")`, `mmreg read axi_gpio 0x04`) or let `mmreg` automatically use the
  `/dev/uioN` region covering a physical address. Addresses outside UIO regions need the `debug`
  feature (`[E_DEBUG_DISABLED]`), so release builds cannot reach arbitrary physical memory.
  `/dev/mem` is opened `O_SYNC` (uncached), and addresses at or above 2 GiB work on 32-bit ARM
  (64-bit `mmap` offset).
- **Read-only mode**: `Device::uio("...").read_only()` and `mmreg read` open with `O_RDONLY` +
  `PROT_READ`, so read-only tools work with `0640` permissions on `/dev/uioN`.
- **Blueprint validation**: the range must be known to the kernel (`/sys/class/uio`, Device Tree
  `of_node/reg`, `/proc/iomem`) and must not overlap System RAM, kernel memory, reserved or ACPI
  memory.
- **Zombie hardware pre-flight**: the owning device must have DT `status = "okay"`, be runtime-PM
  active with active suppliers, have no clock gated in hardware (`clk_summary`), and, on an FPGA
  fabric only, have its FPGA manager `operating`. With `/dev/mem`, the device is held runtime-active
  while mapped.
- **Safety profiles** (`--validate <profile.json>`): extra register checks (clock, reset,
  isolation bits) that must pass before the target is touched.
- **Alignment and bounds**: 8-, 16-, 32-, and 64-bit accesses are checked for natural alignment and
  window bounds before touching hardware.
- **Concurrency & sequence locks**: an inter-process `flock(2)` lock around every access and
  `update_u32` read-modify-write (`flock` on `/dev/uioN` for UIO devices, shared with C programs;
  per-page lock files for `/dev/mem`), plus a per-process `(st_dev, st_ino)` lock registry and
  reentrant sequence guard (`let _g = dev.lock()?;`).
- **UIO interrupts**: `dev.irq_wait(timeout)` (`poll` + `read`) and `dev.irq_unmask()`.
- **Structured errors & silent library**: every failure is a `MmregError` with a stable `[E_*]`
  code and remediation hint; the library never writes to `stderr` (non-fatal pre-flight warnings are
  returned via `dev.warnings()`).

## CLI
```sh
mmreg read 0x4000_0000
mmreg --width 16 read axi_gpio 0x04      # UIO device by name + offset (works without `debug`)
mmreg write axi_gpio 0x00 0x0000_0001
mmreg write 0x4000_0000 0xDEADBEEF
mmreg --dec write 0x4000_0000 100        # or: mmreg write 0x4000_0000 0d100
mmreg --validate zynqmp_gem3.json read 0xFF0E_0000
mmreg --force read 0x4000_0000           # skip blueprint and pre-flight checks
mmreg --version                          # print version and enabled features
```
Numbers are hexadecimal with or without `0x` (underscores allowed); `0d` or `--dec` selects
decimal. Addresses outside UIO regions need a `debug` build (`cargo install mmreg --features
debug`; the prebuilt binaries have it). See [CLI Usage](docs/users/03-cli-usage.md).

## Library
```rust
use mmreg::{Device, Interface, MmregError, Register, SubRegister};
use std::time::Duration;

fn main() -> Result<(), MmregError> {
    // Open a UIO device by its /sys/class/uio/uio*/name (`Send + Sync`, `&self` access)
    let dev = Device::open_uio("axi_gpio")?;
    let status = dev.read_u32(0x04)?;

    // Hold a reentrant lock across a multi-register sequence
    {
        let _guard = dev.lock()?;
        dev.update_u32(0x00, 0x0000_00F0, 0x0000_0050)?;
        dev.write_u32(0x08, status)?;
    }

    // Wait for a UIO interrupt (clear source in device, then unmask)
    if let Some(_irq_count) = dev.irq_wait(Duration::from_millis(100))? {
        dev.write_u32(0x0C, 0x1)?; // clear W1C interrupt status bit
        dev.irq_unmask()?;
    }

    // Or use the named-register Interface layer
    let mut gpio = Interface::new(
        "axi_gpio",
        0xA000_0000,
        0x10,
        vec![Register::new("ctrl", 0x0, vec![SubRegister::new("mode", 7, 4)])],
    );
    gpio.write_subregister("ctrl", "mode", 0x5)?;
    Ok(())
}
```
See [Library Usage](docs/users/04-library-usage.md) and the [API documentation](https://docs.rs/mmreg).

## Footprint
The only dependency is `libc` (safety-profile JSON parsing is built in, behind the default
`profile` feature). Release binaries (x86_64 Linux, stripped, LTO, `panic = "abort"`):

| Build | Size |
| :--- | ---: |
| `cargo build --release` | ~365 KB |
| `cargo build --release --no-default-features` (no `--validate`) | ~360 KB |

Prebuilt targets ([`targets.txt`](targets.txt)): `x86_64`, `aarch64`, `armv7` and `riscv64gc`
Linux, with static musl builds for `aarch64` and `armv7` that run on any rootfs.

## Documentation
- [User guide](docs/users/README.md): installation, UIO setup, CLI, library & generated-code guide,
  safety checks, error codes
- [Developer guide](docs/developers/README.md): architecture, mappers, pre-flight internals,
  build/test/release
- [Changelog](changelog.md) and [roadmap](TODO.md)

## License
MIT

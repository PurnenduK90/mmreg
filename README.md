# mmreg
[![Documentation](https://docs.rs/mmreg/badge.svg)](https://docs.rs/mmreg)
[![Crates.io](https://img.shields.io/crates/v/mmreg.svg)](https://crates.io/crates/mmreg)
[![Downloads](https://img.shields.io/crates/d/mmreg.svg)](https://crates.io/crates/mmreg)
[![License](https://img.shields.io/crates/l/mmreg.svg)](LICENSE.txt)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-blue.svg?maxAge=3600)](https://www.rust-lang.org/)
[![Build Status](https://github.com/PurnenduK90/mmreg/workflows/CI/badge.svg)](https://github.com/PurnenduK90/mmreg/actions)

A Rust library and CLI for safe, concurrent access to 32-bit memory-mapped IO registers: a
`devmem2` replacement that refuses accesses likely to hang the bus instead of performing them.

## Features
- **UIO first, `/dev/mem` fallback**: uses the `/dev/uioN` region covering an address when there
  is one; `/dev/mem` is opened `O_SYNC` (uncached), and addresses at or above 2 GiB work on 32-bit
  ARM (64-bit `mmap` offset).
- **Blueprint validation**: the range must be known to the kernel (`/sys/class/uio`, Device Tree
  `of_node/reg`, `/proc/iomem`) and must not be System RAM, kernel memory, reserved or ACPI memory.
- **Zombie hardware pre-flight**: the owning device must have DT `status = "okay"`, be runtime-PM
  active with active suppliers, have no clock gated in hardware (`clk_summary`), and, on an FPGA
  fabric only, have its FPGA manager `operating`. With `/dev/mem`, the device is held runtime-active
  while mapped.
- **Safety profiles** (`--validate <profile.toml>`): extra register checks (clock, reset,
  isolation bits) that must pass before the target is touched.
- **Alignment and bounds**: every access is 4-byte aligned and inside the declared window.
- **Concurrency**: a per-page inter-process lock around every access and read-modify-write.
- **Structured errors**: every failure is a `MmregError` with a stable `[E_*]` code and a
  remediation hint.

## CLI
```sh
mmreg read 0x4000_0000
mmreg write 0x4000_0000 0xDEADBEEF
mmreg --dec write 0x4000_0000 100        # or: mmreg write 0x4000_0000 0d100
mmreg --validate zynqmp_gem3.toml read 0xFF0E_0000
mmreg --force read 0x4000_0000           # skip blueprint and pre-flight checks
```
Numbers are hexadecimal with or without `0x` (underscores allowed); `0d` or `--dec` selects
decimal. See [CLI Usage](docs/users/03-cli-usage.md).

## Library
```rust
use mmreg::{Interface, MmregError, Register, SubRegister};

fn main() -> Result<(), MmregError> {
    let value = mmreg::read_register_at(0x4000_0000, false)?;
    mmreg::write_register_at(0x4000_0000, value | 1, false)?;

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
The only runtime dependency is `libc`. TOML safety-profile files need the `profile` feature (on by
default), which adds `toml` and `serde`. Release binaries (x86_64 Linux, stripped, LTO,
`panic = "abort"`):

| Build | Size |
| :--- | ---: |
| `cargo build --release` | ~545 KB |
| `cargo build --release --no-default-features` (no `--validate`) | ~360 KB |

Prebuilt targets ([`targets.txt`](targets.txt)): `x86_64`, `aarch64`, `armv7` and `riscv64gc`
Linux, with static musl builds of the embedded ones that run on any rootfs.

## Documentation
- [User guide](docs/users/README.md): installation, CLI, library, safety checks, error codes
- [Developer guide](docs/developers/README.md): architecture, mappers, pre-flight internals,
  build/test/release
- [Changelog](changelog.md) and [roadmap](TODO.md)

## License
MIT

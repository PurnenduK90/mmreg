# 4. Build, Test & Release

## Requirements
- Rust 1.89+ (`rust-version` in `Cargo.toml`; `std::fs::File::lock` needs 1.89).
- [`just`](https://github.com/casey/just) for the tasks below; `cargo-llvm-cov` for coverage;
  [`cross`](https://github.com/cross-rs/cross) and Docker for cross builds.

## Tasks (`justfile`)

| Command | Runs |
| :--- | :--- |
| `just` / `just ci` | `fmt-check`, `clippy`, `test`, `coverage`, `doc`, `package-check` |
| `just ci-all` | `ci` plus `build-targets` |
| `just lint` | `fmt-check` + `clippy` |
| `just clippy` | clippy with `-D warnings`, default and `--no-default-features` |
| `just test` | unit and doc tests, default and `--no-default-features` |
| `just coverage` | `cargo llvm-cov`, failing under 80% line coverage |
| `just doc` | `cargo doc` with warnings denied |
| `just build-targets` | `cross build --release` for every triple in `targets.txt` |

Tests run on Linux, Windows and macOS: hardware and sysfs are replaced by temporary directory trees
and a mock mapper. On Windows only the non-Unix stubs of `mmio.rs` compile, so run clippy and tests
on Linux (or WSL) too; `cargo clippy --target armv7-unknown-linux-gnueabihf` (after
`rustup target add`) checks the 32-bit `mmap64` path without a cross linker.

## Features and Size
- `profile` (default): TOML profile files, about 170 KB of the binary.
- The release profile uses `opt-level = "s"`, LTO, one codegen unit, `panic = "abort"` and
  `strip`. On x86_64 Linux the CLI is about 545 KB, or 360 KB with `--no-default-features`.
- Most of the remaining size is the standard library's formatting and panic machinery; removing it
  needs a nightly `-Zbuild-std` build and is not part of the supported builds.

## Cross Compilation
Release targets are listed once, in [`targets.txt`](../../targets.txt):

| Target | Boards |
| :--- | :--- |
| `x86_64-unknown-linux-gnu` | PCs with PCIe FPGA cards, x86 SBCs |
| `aarch64-unknown-linux-gnu` / `-musl` | Zynq UltraScale+ / Kria, i.MX8, Raspberry Pi 4/5 |
| `armv7-unknown-linux-gnueabihf` / `-musleabihf` | Zynq-7000, Cyclone V SoC, i.MX6, STM32MP1 |
| `riscv64gc-unknown-linux-gnu` / `-musl` | PolarFire SoC, StarFive VisionFive, SiFive |

`gnu` builds link the target's glibc (distro and Yocto packaging); `musl` builds are static and run
on any rootfs (copy and run, like `devmem2`). To add a target, add its triple there; `just
build-targets` and CI pick it up. Binaries land in `target/<triple>/release/mmreg`.

Targets without a prebuilt Rust standard library (`mips*`, `powerpc-*-gnuspe`) need nightly
`-Zbuild-std` and are not supported builds.

## CI/CD (`.github/workflows/`)
- **[`ci.yml`](../../.github/workflows/ci.yml)** (push / PR to `main`): `lint` (`just lint`),
  `test` (`just test coverage doc`), `msrv` (`cargo check` on 1.89, both feature sets), then
  `build` (`just build-targets`).
- **[`publish.yml`](../../.github/workflows/publish.yml)**: on a `v*.*.*` tag, `cargo publish`
  with the `CARGO_TOKEN` secret.

`Cargo.lock` is committed so binary and Yocto builds are reproducible.

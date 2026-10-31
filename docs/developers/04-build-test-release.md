# 4. Build, Test & Release

## Requirements
- Rust 1.75+ (`rust-version` in `Cargo.toml`), the Rust of Yocto 5.0 "scarthgap". Clippy reads
  `rust-version` and flags newer APIs; the CI `msrv` job builds and tests on 1.75 with the
  committed `Cargo.lock`. Policy and the features to avoid:
  [Roadmap Reference → Rust version](05-roadmap-reference.md#rust-version).
- [`just`](https://github.com/casey/just) for the tasks below; `cargo-llvm-cov` for coverage;
  [`cross`](https://github.com/cross-rs/cross) and Docker for cross builds.

## Tasks (`justfile`)

| Command | Runs |
| :--- | :--- |
| `just` / `just ci` | `fmt-check`, `clippy`, `test`, `coverage`, `doc`, `package-check` |
| `just ci-all` | `ci` plus `build-targets` |
| `just lint` | `fmt-check` + `clippy` |
| `just clippy` | clippy with `-D warnings`: `--no-default-features`, default, `--all-features` |
| `just test` | unit and doc tests for the same three feature sets |
| `just coverage` | `cargo llvm-cov`, failing under 80% line coverage |
| `just doc` | `cargo doc` with warnings denied |
| `just build-targets` | `cross build --release --features debug` for every triple in `targets.txt` |

Tests run on Linux, Windows and macOS: hardware and sysfs are replaced by temporary directory trees
and a mock mapper. On Windows only the non-Unix stubs of `mmio.rs` compile, so run clippy and tests
on Linux (or WSL) too; `cargo clippy --target armv7-unknown-linux-gnueabihf` (after
`rustup target add`) checks the 32-bit `mmap64` path without a cross linker.

## Features and Size
- `profile` (default): JSON safety-profile files (`SafetyProfile::parse`, `--validate`), parsed with
  zero external dependencies (`libc` is the only dependency in `Cargo.toml`).
- `debug` (off): `/dev/mem` access for addresses outside UIO regions. Prebuilt binaries
  (`just build-targets`) are bring-up tools and enable it; Yocto / Buildroot recipes choose per
  image (development images with `debug`, release images without).
- **SemVer:** features are additive; the public API is stable across 1.x (public enums and the
  register structs are `#[non_exhaustive]`). Check API changes against the previous release before
  tagging (e.g. `cargo semver-checks`).
- The release profile uses `opt-level = "s"`, LTO, one codegen unit, `panic = "abort"` and
  `strip`. On x86_64 Linux the CLI is about 365 KB, or 360 KB with `--no-default-features`.
- Most of the remaining size is the standard library's formatting and panic machinery; removing it
  needs a nightly `-Zbuild-std` build and is not part of the supported builds.

## Cross Compilation
Release targets are listed once, in [`targets.txt`](../../targets.txt):

| Target | Boards |
| :--- | :--- |
| `x86_64-unknown-linux-gnu` | PCs with PCIe FPGA cards, x86 SBCs |
| `aarch64-unknown-linux-gnu` / `-musl` | Zynq UltraScale+ / Kria, i.MX8, Raspberry Pi 4/5 |
| `armv7-unknown-linux-gnueabihf` / `-musleabihf` | Zynq-7000, Cyclone V SoC, i.MX6, STM32MP1 |
| `riscv64gc-unknown-linux-gnu` | PolarFire SoC, StarFive VisionFive, SiFive |

`gnu` builds link the target's glibc (distro and Yocto packaging); `musl` builds are static and run
on any rootfs (copy and run, like `devmem2`). To add a target, add its triple there; `just
build-targets` and CI pick it up. Binaries land in `target/<triple>/release/mmreg`.

Only add targets that `cross` ships a Docker image for; for others it silently falls back to the
host toolchain and the build fails. Left out for that reason: `riscv64gc-unknown-linux-musl`
(RISC-V boards are covered by the glibc build). Targets without a prebuilt Rust standard library
(`mips*`, `powerpc-*-gnuspe`) need nightly `-Zbuild-std` and are not supported builds.

## CI/CD (`.github/workflows/`)
- **[`ci.yml`](../../.github/workflows/ci.yml)** (push / PR to `main`): `lint` (`just lint`),
  `test` (`just test coverage doc`), `msrv` (`cargo test --locked` on 1.75, both feature sets), then
  `build` (`just build-targets`).
- **[`publish.yml`](../../.github/workflows/publish.yml)**: on a `v*.*.*` tag, `cargo publish`
  with the `CARGO_TOKEN` secret.

`Cargo.lock` is committed so binary and Yocto builds are reproducible.

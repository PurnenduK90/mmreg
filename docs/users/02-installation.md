# 2. Installation & Setup

`mmreg` can be installed as a standalone CLI binary or added as a library dependency in your Rust project.

---

## Installing the CLI Binary

### Using Cargo (from Crates.io)
```sh
cargo install mmreg
```

### Installing from Source
```sh
git clone https://github.com/PurnenduK90/mmreg.git
cd mmreg
cargo install --path .
```

### Features: Debug, Release and Minimal Builds

| Feature | Default | Adds |
| :--- | :--- | :--- |
| `profile` | on | JSON safety-profile files (`--validate`) |
| `debug` | off | `/dev/mem` access for addresses no UIO region covers (`devmem` replacement) |

```sh
cargo install mmreg --features debug         # bring-up / debug images: any address
cargo install mmreg                          # release images: UIO devices only
cargo install mmreg --no-default-features    # smallest: UIO only, no --validate
```

The prebuilt binaries (`just build-targets`) are built with `debug`.

---

## Adding `mmreg` as a Rust Library Dependency

Add `mmreg` to your project's `Cargo.toml`:

```toml
[dependencies]
mmreg = "1.0"
```

Or using `cargo add`:
```sh
cargo add mmreg                          # with JSON profile files
cargo add mmreg --no-default-features    # without profile file parsing
```

`mmreg` needs Rust 1.75 or newer (the Rust of Yocto 5.0 "scarthgap").

---

## Cross-Compiling for Embedded Linux Targets

`mmreg` uses [`cross`](https://github.com/cross-rs/cross) to build release binaries for embedded architectures listed in `targets.txt`: `x86_64`, `aarch64`, `armv7` (hard-float) and `riscv64gc` Linux, each as a glibc build (for distro / Yocto images), and `aarch64` and `armv7` also as a static musl build that runs on any rootfs:

```sh
# Install cross
cargo install cross --git https://github.com/cross-rs/cross

# Build all targets defined in targets.txt
just build-targets
```

Compiled binaries are placed in `target/<target-triple>/release/mmreg`. For a quick test on a board
without a matching toolchain, copy the musl binary (e.g.
`target/aarch64-unknown-linux-musl/release/mmreg`) to it and run it directly.

---

## Including `mmreg` in a Yocto Project / PetaLinux Build

You can include `mmreg` in a Yocto / OpenEmbedded (or Xilinx PetaLinux) root filesystem using either a **source-built Cargo recipe** (Rust $\ge 1.75$: Yocto 5.0 "scarthgap" or newer) or a **pre-built binary recipe** (compatible with any Yocto release).

### Option A: Building from Source with `inherit cargo` (`mmreg_1.0.1.bb`)

Create `meta-custom/recipes-devtools/mmreg/mmreg_1.0.1.bb`:

```bitbake
SUMMARY = "Safe, concurrent 32-bit memory-mapped IO register access CLI and library"
HOMEPAGE = "https://github.com/PurnenduK90/mmreg"
LICENSE = "MIT"
LIC_FILES_CHKSUM = "file://LICENSE.txt;md5=3914388ed65427d749c0c5886d2d12cf"

SRC_URI = "git://github.com/PurnenduK90/mmreg.git;protocol=https;branch=main"
SRCREV = "${AUTOREV}"
S = "${WORKDIR}/git"

inherit cargo cargo-update-recipe-crates
require ${BPN}-crates.inc

# Development images: /dev/mem access for any address (devmem replacement)
# CARGO_BUILD_FLAGS:append = " --features debug"
# Release images, smallest binary: UIO only, no JSON profile files (--validate)
# CARGO_BUILD_FLAGS:append = " --no-default-features"
```

> [!TIP]
> Generate `mmreg-crates.inc` (the `crate://` entries and checksums, from the committed `Cargo.lock`) with:
> ```sh
> bitbake -c update_recipe_crates mmreg
> ```
> Or generate the recipe directly from your development host with [`cargo-bitbake`](https://github.com/meta-rust/cargo-bitbake) (`cargo bitbake`).

### Option B: Pre-Built Binary Recipe (For Older Yocto / PetaLinux Toolchains)

If your BSP's `rustc` is older than 1.75, cross-compile the binary on your host (`just build-targets`) and package the target binary (`aarch64` or `armv7`) directly:

```bitbake
SUMMARY = "Pre-built mmreg CLI binary for safe MMIO register access"
LICENSE = "MIT"
LIC_FILES_CHKSUM = "file://${COMMON_LICENSE_DIR}/MIT;md5=0835ade698e0bcf8506ecda2f7b4f302"

SRC_URI = "file://mmreg"
S = "${WORKDIR}"

INSANE_SKIP:${PN} += "already-stripped"

do_install() {
    install -d ${D}${bindir}
    install -m 0755 ${WORKDIR}/mmreg ${D}${bindir}/mmreg
}
```

### Adding `mmreg` to Your Target Image

Add `mmreg` to `conf/local.conf` or your custom image recipe (e.g., `petalinux-image-minimal.bbappend`):

```bitbake
IMAGE_INSTALL:append = " mmreg"
```

### Recommended Yocto Kernel & Device Tree Configuration for `mmreg`

To get full benefit from `mmreg`'s automatic UIO backend and Hardware Pre-Flight checks on embedded targets (such as Xilinx Zynq-7000 / Zynq UltraScale+ MPSoC):

1. **Enable Generic UIO and FPGA Manager in Kernel Config (`cfg/mmreg.cfg`)**:
   ```ini
   CONFIG_UIO=y
   CONFIG_UIO_PDRV_GENIRQ=y
   CONFIG_FPGA=y
   CONFIG_FPGA_MGR_ZYNQ_FPGA=y
   CONFIG_FPGA_MGR_ZYNQMP_FPGA=y
   ```
2. **Bind `generic-uio` in Device Tree (`system-user.dtsi`)**:
   ```dts
   / {
       chosen {
           bootargs = "earlycon console=ttyPS0,115200 uio_pdrv_genirq.of_id=generic-uio root=/dev/mmcblk0p2 rw rootwait";
       };
   };

   &axi_gpio_0 {
       compatible = "generic-uio";
       status = "okay";
   };
   ```

---

## System Permissions on Linux

### 1. Using UIO (`/dev/uioN`) — Recommended
When a peripheral is bound to the Linux `uio_pdrv_genirq` or `generic-uio` driver, `mmreg` automatically detects and uses `/dev/uioN`.
- You can grant non-root users access to `/dev/uioN` via a `udev` rule (e.g., assigning group `uio` or `dialout` with mode `0660`).

### 2. Using `/dev/mem` (Debug Builds)
When no UIO device maps the target address, an `mmreg` built with the `debug` feature uses
`/dev/mem`; other builds refuse with `[E_DEBUG_DISABLED]` before opening anything. Keep `debug`
builds to development images.
- Accessing `/dev/mem` requires `root` privileges (`sudo mmreg ...`) or the `CAP_SYS_RAWIO` capability:
  ```sh
  sudo setcap cap_sys_rawio+ep $(which mmreg)
  ```
- With `CONFIG_STRICT_DEVMEM=y` the kernel refuses `/dev/mem` mappings of RAM, and with `CONFIG_IO_STRICT_DEVMEM=y` also of ranges claimed by a driver (`[E_DEVMEM_MMAP]`, `Operation not permitted`); expose such devices through UIO.

### 3. Locking and the Lock Directory
Every access to a UIO device holds an exclusive `flock` on `/dev/uioN` itself, so C programs that
lock their own `/dev/uioN` descriptor the same way exclude and are excluded by `mmreg`. Accesses
through `/dev/mem` (and custom mappers) lock their physical page through a file in
`/run/lock/mmreg` (or `/tmp/mmreg-lock` only on systems where `/run/lock` does not exist). To prevent
root and non-root processes from silently locking different directories, `mmreg` never falls back to
`/tmp/mmreg-lock` when `/run/lock` exists. If your image has a root-only `/run/lock` (`0755`, common
on BusyBox / minimal Yocto images), pre-create `/run/lock/mmreg` at boot with mode `1777`
(`mkdir -m 1777 -p /run/lock/mmreg` or a `tmpfiles.d` entry `d /run/lock/mmreg 1777 root root -`).

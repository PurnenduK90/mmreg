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

### Features: Debug, Release, C ABI, and Minimal Builds

| Feature | Default | Adds |
| :--- | :--- | :--- |
| `profile` | on | JSON safety-profile files (`--validate`) |
| `debug` | off | `/dev/mem` access for addresses no UIO region covers (`devmem` replacement) |

```sh
cargo install mmreg --features debug                        # bring-up / debug images: any address
cargo install mmreg                                         # release images: UIO devices only
cargo install mmreg --no-default-features                   # smallest CLI: UIO only, no --validate
```

The C library (`libmmreg.a`, `libmmreg.so`, used by C programs and the Python binding) is the
`mmreg-capi` crate in `bindings/c`, built from a checkout of the repository:

```sh
cargo build --release -p mmreg-capi                  # UIO only, no JSON parser
cargo build --release -p mmreg-capi --features debug # also /dev/mem, for bring-up images
```

Its header is `bindings/c/include/mmreg.h` and its pkg-config file `bindings/c/mmreg.pc`.

The prebuilt binaries (`just build-targets`) are built with `debug`.

---

## Adding `mmreg` as a Rust Library Dependency

Add `mmreg` to your project's `Cargo.toml`:

```toml
[dependencies]
mmreg = "1.2"
```

Or using `cargo add`:
```sh
cargo add mmreg                          # with JSON profile files
cargo add mmreg --no-default-features    # without profile file parsing
```

`mmreg` needs Rust 1.75 or newer (the Rust of Yocto 5.0 "scarthgap").

---

## Python Package

The Python package (`bindings/python`) is pure Python over `libmmreg.so` and installs with `pip`
in one of two ways.

**A wheel that bundles `libmmreg.so`** (development hosts, boards without the C library). Build it
in a checkout of this repository; a target CPU other than the host's needs
[`cross`](https://github.com/cross-rs/cross):

```sh
just python-wheel                                   # this machine: target/wheels/mmreg-1.2.1-py3-none-linux_x86_64.whl
just python-wheel aarch64-unknown-linux-gnu         # Zynq UltraScale+, i.MX8, Raspberry Pi 4/5
just python-wheel armv7-unknown-linux-gnueabihf     # Zynq-7000, Cyclone V SoC, i.MX6, STM32MP1
pip install mmreg-1.2.1-py3-none-linux_aarch64.whl  # on the target
```

The wheel carries a release `libmmreg.so` (UIO only, no `/dev/mem`) and a CPU tag, so `pip`
refuses it on another architecture. `just python-wheel-test` installs the host wheel into a fresh
virtual environment and runs the tests and examples against it. Wheels are for glibc targets.

**The pure-Python package** when the image already installs `libmmreg.so` (Yocto, Buildroot, see
below) or a development build points to one:

```sh
pip install ./bindings/python
MMREG_LIB_PATH=$PWD/target/debug/libmmreg.so python3 bindings/python/examples/quickstart.py
```

The package loads the first library it finds: `$MMREG_LIB_PATH`, the copy bundled in the package,
`target/release` / `target/debug` of the repository it is imported from, `/usr/local/lib`,
`/usr/lib`, then the system search path. A missing or incompatible library raises `MmregError`
`E_LIB_LOAD` on first use.

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

## Including `mmreg` in a Yocto Project / PetaLinux or Buildroot Build

You can include `mmreg` in a Yocto / OpenEmbedded (or Xilinx PetaLinux) or Buildroot root filesystem using either a **source-built Cargo recipe** (Rust $\ge 1.75$: Yocto 5.0 "scarthgap" or newer) or a **pre-built binary recipe** (compatible with any Yocto release).

### Recommended Image Split (Release vs. Development Images)

| Artifact | Feature Flags | Target Images |
| :--- | :--- | :--- |
| `libmmreg.a`, `libmmreg.so`, `mmreg.h`, `mmreg.pc` | `-p mmreg-capi` | Every image with C or Python applications (UIO only, zero parser overhead) |
| `mmreg` CLI (release) | default or `--no-default-features` | Production images (UIO devices only; arbitrary `/dev/mem` disabled) |
| `mmreg` CLI (bring-up) | `--features debug` | Development / bring-up images (`devmem` replacement) |
| Python `mmreg` (`bindings/python/mmreg`) | pure-Python (`ctypes` over `libmmreg.so`) | Development images or Python-enabled application images |

### Option A: Building CLI, C Library, and Python Module from Source (`mmreg_1.2.1.bb`)

Create `meta-custom/recipes-devtools/mmreg/mmreg_1.2.1.bb`:

```bitbake
SUMMARY = "Safe, concurrent 8/16/32/64-bit memory-mapped IO register access (Rust, C, Python, and CLI)"
HOMEPAGE = "https://github.com/PurnenduK90/mmreg"
LICENSE = "MIT"
LIC_FILES_CHKSUM = "file://LICENSE.txt;md5=3914388ed65427d749c0c5886d2d12cf"

SRC_URI = "git://github.com/PurnenduK90/mmreg.git;protocol=https;branch=main"
# Pin the commit of the v1.2.1 tag: `${AUTOREV}` would drift away from the pinned `mmreg-crates.inc`.
SRCREV = "<commit-sha-of-v1.2.1>"
S = "${WORKDIR}/git"

inherit cargo cargo-update-recipe-crates python3-dir
require ${BPN}-crates.inc

# Build the CLI and the C library (`mmreg-capi` in bindings/c: `libmmreg.a`, `libmmreg.so`).
# For development/bring-up images that also allow /dev/mem access:
#   CARGO_BUILD_FLAGS:append = " --workspace --features debug"
CARGO_BUILD_FLAGS:append = " --workspace"

do_install:append() {
    # Install C header, static/shared libraries, and pkg-config file into sysroot
    install -d ${D}${includedir}
    install -m 0644 ${S}/bindings/c/include/mmreg.h ${D}${includedir}/mmreg.h

    install -d ${D}${libdir}/pkgconfig
    install -m 0644 ${B}/target/${CARGO_TARGET_SUBDIR}/libmmreg.a ${D}${libdir}/libmmreg.a
    # libmmreg.so carries the SONAME `libmmreg.so.1` (set by bindings/c/build.rs): ship the versioned file in
    # ${PN} and keep the unversioned development symlink in ${PN}-dev.
    install -m 0755 ${B}/target/${CARGO_TARGET_SUBDIR}/libmmreg.so ${D}${libdir}/libmmreg.so.${PV}
    ln -sf libmmreg.so.${PV} ${D}${libdir}/libmmreg.so.1
    ln -sf libmmreg.so.1 ${D}${libdir}/libmmreg.so
    install -m 0644 ${S}/bindings/c/mmreg.pc ${D}${libdir}/pkgconfig/mmreg.pc

    # Install pure-Python ctypes package (no C extension compilation needed)
    install -d ${D}${PYTHON_SITEPACKAGES_DIR}/mmreg
    install -m 0644 ${S}/bindings/python/mmreg/__init__.py ${D}${PYTHON_SITEPACKAGES_DIR}/mmreg/__init__.py
}

PACKAGES =+ "${PN}-python"
FILES:${PN}-python = "${PYTHON_SITEPACKAGES_DIR}/mmreg"
RDEPENDS:${PN}-python = "${PN} python3-core"
```

> [!TIP]
> Generate `mmreg-crates.inc` (the `crate://` entries and checksums, from the committed `Cargo.lock`) with:
> ```sh
> bitbake -c update_recipe_crates mmreg
> ```
> In **Buildroot**, use `$(eval $(cargo-package))` with `MMREG_CARGO_BUILD_OPTS = --workspace` and install `libmmreg.a`, `bindings/c/include/mmreg.h`, and `bindings/c/mmreg.pc` into `$(STAGING_DIR)/usr`, and `libmmreg.so` as `libmmreg.so.1.2.1` with `libmmreg.so.1` and `libmmreg.so` symlinks into both `$(STAGING_DIR)/usr/lib` and `$(TARGET_DIR)/usr/lib` (the library's SONAME is `libmmreg.so.1`).

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

---

## Setting Up Linux Userspace I/O (UIO)

In production embedded Linux images, peripherals and custom FPGA IP blocks should be bound to the
kernel's generic UIO driver (`uio_pdrv_genirq`) rather than accessed via `/dev/mem`. UIO maps only
the peripheral's declared register window and works in default `mmreg` builds without the `debug`
feature or `CAP_SYS_RAWIO`.

### 1. Kernel Configuration (`cfg/mmreg.cfg`)

Enable UIO, `uio_pdrv_genirq`, and (on FPGA SoCs) the FPGA Manager subsystem:

```ini
CONFIG_UIO=y
CONFIG_UIO_PDRV_GENIRQ=y
CONFIG_FPGA=y
CONFIG_FPGA_MGR_ZYNQ_FPGA=y
CONFIG_FPGA_MGR_ZYNQMP_FPGA=y
```

### 2. Device Tree Binding (`system-user.dtsi`)

Give each IP block `compatible = "generic-uio"` and an explicit, unique `linux,uio-name` property:

```dts
my_block@a0010000 {
    compatible = "generic-uio";
    reg = <0x0 0xa0010000 0x0 0x10000>;
    interrupts = <0 89 4>;            /* optional: level-high interrupt */
    linux,uio-name = "my_block";      /* optional; defaults to the DT node name */
    status = "okay";
};
```

Because `/dev/uioN` numbers can change between boots or Device Tree overlays, programs and scripts
should always open devices by name (`Device::open_uio("my_block")` or `mmreg read my_block 0x04`),
which inspects `/sys/class/uio/uio*/name`. Keep `linux,uio-name` unique across nodes: if two UIO
devices share a name, `mmreg` refuses with `[E_UIO_AMBIGUOUS]` rather than guessing.

### 3. Passing `of_id=generic-uio` to `uio_pdrv_genirq`

The kernel's `uio_pdrv_genirq` driver only binds `compatible = "generic-uio"` nodes when told which
Device Tree `compatible` string to match:

- **Built-in driver (`CONFIG_UIO_PDRV_GENIRQ=y`)** — add to the kernel command line (`bootargs`):
  ```dts
  / {
      chosen {
          bootargs = "earlycon console=ttyPS0,115200 uio_pdrv_genirq.of_id=generic-uio root=/dev/mmcblk0p2 rw rootwait";
      };
  };
  ```
- **Loadable module (`CONFIG_UIO_PDRV_GENIRQ=m`)** — pass the module parameter or add
  `/etc/modprobe.d/uio.conf`:
  ```text
  options uio_pdrv_genirq of_id=generic-uio
  ```

### 4. `udev` Permissions (Read-Write vs. Read-Only)

By default, `/dev/uioN` nodes are created as `root:root 0600`. Use a `udev` rule (for example in
`/etc/udev/rules.d/99-uio.rules`) to grant access to a service group by device name:

```text
# Read-write access for control daemons
SUBSYSTEM=="uio", ATTR{name}=="my_block", GROUP="hwaccess", MODE="0660"

# Read-only access (0640) for monitoring / version-reader tools
SUBSYSTEM=="uio", ATTR{name}==" telemetry_block", GROUP="monitors", MODE="0640"
```

`mmreg read` and `Device::uio("...").read_only()` open `/dev/uioN` with `O_RDONLY` and map it with
`PROT_READ` (`flock(2)` works on a read-only descriptor), so read-only tools work with `0640`
permissions without needing write access to the hardware.

---

## Using `/dev/mem` (Debug Builds) and the Lock Directory

### 1. `/dev/mem` Permissions
When no UIO device maps the target address, an `mmreg` built with the `debug` feature uses
`/dev/mem`; other builds refuse with `[E_DEBUG_DISABLED]` before opening anything. Keep `debug`
builds to development images.
- Accessing `/dev/mem` requires `root` privileges (`sudo mmreg ...`) or the `CAP_SYS_RAWIO` capability:
  ```sh
  sudo setcap cap_sys_rawio+ep $(which mmreg)
  ```
- With `CONFIG_STRICT_DEVMEM=y` the kernel refuses `/dev/mem` mappings of RAM, and with `CONFIG_IO_STRICT_DEVMEM=y` also of ranges claimed by a driver (`[E_DEVMEM_MMAP]`, `Operation not permitted`); expose such devices through UIO.

### 2. Locking and the Lock Directory
Every access to a UIO device holds an exclusive `flock` on `/dev/uioN` itself, so C programs that
lock their own `/dev/uioN` descriptor the same way exclude and are excluded by `mmreg`. Accesses
through `/dev/mem` (and custom mappers) lock their physical page through a file in
`/run/lock/mmreg` (or `/tmp/mmreg-lock` only on systems where `/run/lock` does not exist). To prevent
root and non-root processes from silently locking different directories, `mmreg` never falls back to
`/tmp/mmreg-lock` when `/run/lock` exists. If your image has a root-only `/run/lock` (`0755`, common
on BusyBox / minimal Yocto images), pre-create `/run/lock/mmreg` at boot with mode `1777`
(`mkdir -m 1777 -p /run/lock/mmreg` or a `tmpfiles.d` entry `d /run/lock/mmreg 1777 root root -`).

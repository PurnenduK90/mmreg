# 5. Roadmap Reference

Background for the 1.x work in [TODO.md](../../TODO.md): the use case that drives it, the kernel
interfaces it builds on, the API it must cover, and the build environments it must fit. Everything
needed to implement the roadmap is here or in the linked kernel documentation; no particular
project's sources are required.

---

## The Use Case

A typical adopter is an embedded Linux board with an FPGA or SoC fabric:

- Several custom IP blocks, each an AXI-Lite register window, each bound to the kernel's generic
  UIO driver in the Device Tree with its own `linux,uio-name` (`/dev/uioN`, no IP-specific kernel
  driver). Some blocks raise a level-triggered interrupt.
- Register headers (C `#define`s, Rust constants) and documentation generated from a register
  description by a register generator, which can also emit `mmreg-block/1` block files.
- Small C programs per IP block (version readers, functional tests, production daemons) built in
  Yocto, linking a project-local UIO helper library: open by name, locked access, held lock for
  sequences, interrupt wait.
- `devmem` (BusyBox) for bring-up and field debugging.

mmreg replaces the helper library (1.1 for Rust, 1.2 for C / Python) and `devmem` (1.1). The
project keeps its generated headers for production code; block files (1.3) add access by name in
Python for debugging and experiments.

---

## Kernel UIO Interface

Reference: the kernel's *The Userspace I/O HOWTO* (`Documentation/driver-api/uio-howto.rst`).

### Binding a block to UIO (Device Tree)

```dts
my_block@a0010000 {
    compatible = "generic-uio";
    reg = <0x0 0xa0010000 0x0 0x10000>;
    interrupts = <0 89 4>;            /* optional */
    linux,uio-name = "my_block";      /* optional; default is the node name */
};
```

`uio_pdrv_genirq` only binds `generic-uio` when told to: kernel command line
`uio_pdrv_genirq.of_id=generic-uio` (or the module parameter `of_id=generic-uio`).

### Sysfs

| Path | Content |
|---|---|
| `/sys/class/uio/uioN/name` | `linux,uio-name` (one line). Lookup by name compares this, trimmed |
| `/sys/class/uio/uioN/maps/mapM/addr` | physical address of region M (hex, page-aligned for `uio_pdrv_genirq`) |
| `/sys/class/uio/uioN/maps/mapM/size` | region size in bytes, counted from `addr` |
| `/sys/class/uio/uioN/maps/mapM/offset` | position of the device inside the first page (informational) |
| `/sys/class/uio/uioN/device` | link to the owning platform device (used by pre-flight) |

Names are not guaranteed unique; two devices with the same name should be an error
(`[E_UIO_AMBIGUOUS]`), not "first wins". Device numbers (`uioN`) can change between boots, so
programs must look up by name.

### `/dev/uioN`

| Operation | Meaning |
|---|---|
| `mmap(len, offset = M * page_size)` | maps region M |
| `write(fd, &(u32)1, 4)` | re-enables (unmasks) the interrupt; `(u32)0` masks it. Only for drivers with `irqcontrol` (`uio_pdrv_genirq` has it); otherwise `EIO`/`ENOSYS` |
| `read(fd, &u32, 4)` | blocks until an interrupt; returns the total interrupt count (detect missed interrupts by the jump) |
| `poll(POLLIN)` | readable when an interrupt is pending: use for timeouts |
| `flock` | advisory; mmreg's lock protocol uses it (see TODO, Lock protocol) |

`uio_pdrv_genirq` disables the interrupt line when it fires. For a level-triggered source the
program must clear the cause in the device first, then unmask, or the interrupt fires again
immediately. `irq_wait` therefore does not unmask by itself.

### Permissions

`/dev/uioN` is `root:root 0600` by default. Distributions open it to a group with a udev rule, e.g.

```text
SUBSYSTEM=="uio", ATTR{name}=="my_block", GROUP="hwaccess", MODE="0660"
```

Read-only access (`O_RDONLY` + `PROT_READ`) works with read permission only, so read-only tools
(version readers, monitors) can run with `0640`.

---

## The API to Cover (UIO Helper Library)

What project-local helper libraries typically provide, and so what `Device` (1.1) and the C ABI
(1.2) must offer. A program written against such a library must port mechanically:

| Helper library call | Semantics | mmreg 1.1 (Rust) | mmreg 1.2 (C) |
|---|---|---|---|
| `find(name) -> "/dev/uioN"` | look up by `uioN/name` | `uio::find(name)` | `mmreg_find_uio` |
| `open(name, dev, writable)` | `dev` overrides the lookup; map region 0 | `Device::open_uio(name)` / `open_uio_path(dev)`, `.read_only()` | `mmreg_open_uio(name, flags)`, `mmreg_open_uio_path` |
| `close` | unmap, close | `Drop` | `mmreg_close` |
| `rd(off)` / `wr(off, v)` | 32-bit, locked per access | `read_u32` / `write_u32` | `mmreg_rd32` / `mmreg_wr32` |
| `update(off, mask, v)` | locked read-modify-write, returns new value | `update_u32` | `mmreg_update32` |
| `lock` / `unlock` | held across a sequence; nests; also excludes other processes | `lock()` guard | `mmreg_lock` / `mmreg_unlock` |
| `irq_unmask` | write 1 | `irq_unmask` | `mmreg_irq_unmask` |
| `irq_wait(timeout_ms, &count)` | poll + read; 0 on interrupt, timeout distinguishable from error | `irq_wait(Duration) -> Result<Option<u32>>` | `mmreg_irq_wait` (returns 1 / 0 / -1) |
| field macro `FIELD(v, mask, shift)` | `(v & mask) >> shift` | generated headers / `Interface` | `MMREG_FIELD` macro in `mmreg.h` |

Offsets are byte offsets from the region start. Errors in C: negative return plus
`mmreg_last_error()`; never a message on stderr.

---

## Build Environments

### Rust version

mmreg must build with the Rust shipped by the oldest supported embedded LTS. Yocto 5.0
"scarthgap" (LTS) ships **Rust 1.75**, so `rust-version = "1.75"`. Features newer than that, and
what mmreg uses instead:

| Avoid | Needs | Use |
|---|---|---|
| `edition = "2024"`, `if let … && let …` chains | 1.85 / 1.88 | `edition = "2021"`; a helper returning `Option`, or `Option::filter` |
| `Cargo.lock` `version = 4` | Cargo 1.78 | lockfile v3 (Cargo keeps v3 while `rust-version` < 1.78) |
| `std::fs::File::lock` | 1.89 | `mmio::lock_exclusive` (`libc::flock`) |
| `<[T]>::as_chunks` | 1.88 | `chunks_exact` |
| `u32::is_multiple_of` | 1.87 | `x % n == 0` |
| `Option::is_none_or` | 1.82 | `map_or(true, …)` |

Clippy reads `rust-version` and flags newer standard-library APIs; the CI `msrv` job runs
`cargo test --locked` on 1.75 for both feature sets. Dependencies are kept 1.75-compatible by the
MSRV-aware resolver; update them with:

```sh
CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS=fallback cargo update
```

Raise the MSRV only when the oldest supported Yocto / Buildroot LTS does.

### Yocto

- Recipes use `inherit cargo` (or `cargo_c` for the C library) with crates listed in the recipe
  (`cargo-update-recipe-crates`), so builds are offline: keep dependencies few, and the
  `capi` library build (`--no-default-features --features capi`) dependency-free apart from `libc`.
- A C consumer needs `libmmreg.a` (static, so the image does not need the Rust standard library as a
  shared object), `mmreg.h` and `mmreg.pc` in the sysroot; the recipe installs them into
  `${libdir}`, `${includedir}`, `${libdir}/pkgconfig`.
- Typical split: the CLI with `debug` in development images, without `debug` (or not at all) in
  release images; the library in every image that has a consumer.
- Block files install to `/usr/share/mmreg/blocks/` from the recipe of the IP that owns them,
  together with the Python package, in development images only. Device Tree nodes list an
  IP-specific `compatible` before `generic-uio`, so instances are found without per-board files.

### Buildroot

`cargo` package infrastructure, same constraints (offline vendored crates, toolchain Rust
version).

---

## Testing Without Hardware

- Sysfs and `/dev` are parameters (`sysfs_uio`, `dev_root`) in the existing code; keep that for
  every new lookup, so tests build fake trees in a temp dir.
- Registers: the mock `MemoryMapper` (see `interface.rs` tests), extended to all widths.
- Interrupt wait: a fake `/dev/uioN` backed by a `socketpair` or pipe (write 4 bytes to raise,
  check the unmask write), so `poll` + `read` run for real.
- Lock protocol: two processes (`std::process::Command` on the test binary) and two threads on
  one temp file; a C test program for C ↔ Rust interop (1.2).
- On real hardware (optional CI runner): QEMU with a `generic-uio` node, or any board with UIO;
  tests that need it are `#[ignore]` by default.

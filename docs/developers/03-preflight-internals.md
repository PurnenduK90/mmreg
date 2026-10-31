# 3. Blueprint & Pre-Flight Internals

Address validation and Zombie Hardware Pre-Flight live in [`src/memcheck.rs`](../../src/memcheck.rs);
safety profiles in [`src/validation.rs`](../../src/validation.rs).

---

## 1. Entry Point: `resolve(target, size, mapper_type, check)`

Returns a `Target`: the backend (`MapperType::Uio` for a covering or explicit UIO region, else
`DevMem`) and the owning sysfs device, if found. With `check` (no `--force`):
1. Explicit `MapperType::Uio`, or `MapperType::Auto` with a covering UIO region: pre-flight its
   parent device (`uioN/device`, resolved, when present in sysfs) and return. A UIO region is a
   device range by construction (and `OsMapper` still verifies `maps/mapM/{addr,size}`), so
   `/proc/iomem` is not consulted.
2. Otherwise `/proc/iomem` is scanned (any overlap with RAM, kernel, reserved or ACPI ranges is
   refused; only leaf entries with no child lines count as listed MMIO, so unmapped holes inside
   parent bus windows like `PCI Bus` or `amba` are rejected), the platform device whose DT `reg`
   covers the range is pre-flighted, and a range with neither owner nor leaf `/proc/iomem` entry is
   `[E_BLUEPRINT_UNMAPPED]`.

Without `check`, only the lookups run (the owner is still needed for the runtime PM guard).

## 2. Testable Roots

Filesystem roots are parameters of the scanning functions so tests run on any OS with temporary
trees: `find_uio_region(sysfs_uio, dev_root, ..)`, `find_owning_device(devices, ..)`,
`parse_iomem(reader, ..)`, `check_clk_summary(reader, ..)`,
`preflight_check_device(.., fpga_mgr_root)`, `check_device_clocks(.., clk_debug_root)`. Supplier
links are matched as `supplier:*` and, because `:` is not allowed in Windows file names,
`supplier_*`.

## 3. Device Tree `of_node/reg`

`reg` is big-endian `(address, size)` cells. `reg_file_covers` reads the parent node's
`#address-cells` / `#size-cells`; without them, `reg_covers` tries the layouts `(2,2)`, `(1,1)` and
`(2,1)` that divide the property length.

## 4. FPGA vs. Non-FPGA Devices

An unprogrammed FPGA must block programmable-logic peripherals and never SoC peripherals.
`is_fpga_backed_device` is true when the device path, its resolved path, or its resolved `of_node`
has a component starting with one of `FPGA_BUS_PREFIXES` (`amba_pl`, `axi_pl`, `pl-bus`,
`fpga-region`, `fpga_region`, `fpga-full`, `fpga-bus`), or when a supplier link names `fpga`. Only
then does `check_fpga_manager_operating` run.

## 5. Clocks (`clk_summary`)

`check_device_clocks` collects the device's clock names from `of_node/clock-names` and streams
`/sys/kernel/debug/clk/clk_summary` with one reused line buffer. A row (clock name, enable count,
prepare count, ...) is relevant when it is one of those clocks or lists the device as consumer;
consumer-only lines (newer kernels) apply to the clock row above. A relevant clock with hardware
enable `N` is `[E_ZOMBIE_CLOCK_GATED]`. Without that column, or without `clk_summary` (per-clock
`clk_enable_count` files), an enable count of 0 only warns: UIO devices never enable their clocks
and `clk_ignore_unused` keeps unused clocks running at count 0.

## 6. Runtime PM Guard

`RuntimePmGuard::acquire(dev)` sets `dev/power/control` to `on` when it is not, and restores the
previous value on drop. `Interface::map` takes it only for `/dev/mem`; an open `/dev/uioN` already
holds a runtime-PM reference.

## 7. Safety Profiles

`SafetyProfile` and `ValidationCheck` are plain structs, always available, and
`SafetyProfile::execute_with(read_fn)` runs the checks with any reader (real hardware in
`Interface::map` and the CLI, closures in tests). With the `profile` feature,
`SafetyProfile::parse` parses JSON with a small built-in recursive-descent parser (no external crate
dependencies), rejecting unknown keys except `x-*` extensions and routing string numbers through
`parse_u64_radix`.

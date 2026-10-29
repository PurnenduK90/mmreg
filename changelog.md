* **[0.1.0]** Initial Release
* **[0.1.1]** Github Repository Added
    * Readme Updated
    * Subregister Mask made public to edit multiple subregister together
    * CI github action for build
* **[1.0.0]** UIO Backend, Blueprint Validation & Hardware Pre-Flight
    * Backends
        * `MemoryMapper` trait (`map`, `unmap`, `is_mapped`, `read_u32`, `write_u32`) with UIO (`/dev/uioN`) and `/dev/mem` backends
        * `MapperType::Auto` (default) prefers the UIO region covering an address and falls back to `/dev/mem`; `MapperType::Uio { device_path, region_index }` takes the region's window from `/sys/class/uio/uioN/maps/mapM/{addr,size}`
        * `/dev/mem` opened `O_SYNC` (uncached); 64-bit `mmap` offset on 32-bit glibc/Android so addresses at or above 2 GiB work on 32-bit ARM
    * Safety
        * Blueprint validation against `/sys/class/uio`, 32/64-bit Device Tree `of_node/reg` and `/proc/iomem`; `System RAM`, kernel, `reserved` and ACPI ranges are refused
        * Zombie Hardware Pre-Flight: `of_node/status`, `power/runtime_status`, `supplier:*` links, Common Clock Framework `clk_summary` hardware-enable state (software enable count only warns), and FPGA manager `state` for FPGA-fabric devices only
        * Device held runtime-active (`power/control = on`) while mapped through `/dev/mem`, restored on unmap
        * 4-byte alignment and bounds (`offset + 4 <= size`) of every register, and valid subregister bit ranges, enforced
        * Declarative TOML `SafetyProfile` (`--validate <file>`) with strict keys; an attached profile also runs with `--force`
    * Concurrency
        * Per-page inter-process lock (`/run/lock/mmreg/<page_base>-<page_end>.lock`, directory mode `1777`) held per access and per read-modify-write; lock files are never deleted
    * API
        * `MmregError` enum with stable `[E_*]` codes and remediation text, returned everywhere
        * `Interface` fields private, with getters and builders (`with_force`, `with_mapper_type`, `with_safety_profile`, `with_custom_mapper`); `map()` is idempotent; `Drop` unmaps
    * CLI
        * `-d` / `--dec` and `0x` / `0d` prefixes; one prefix only, underscores allowed
        * All operands validated before any hardware access; extra arguments rejected
    * Footprint
        * `libc` is the only mandatory dependency; TOML profiles behind the default `profile` feature (`--no-default-features` for the smallest binary)
        * Release profile adds `panic = "abort"`; `rust-version = "1.89"`
    * Targets
        * Release builds for `x86_64`, `aarch64`, `armv7` and `riscv64gc` Linux (glibc), plus static musl builds for `aarch64`, `armv7` and `riscv64gc`; `i686` dropped
    * Tooling
        * `justfile` is the single task runner (CI uses it), checks both feature sets and an MSRV job; `Cargo.lock` committed
        * `docs/users` + `docs/developers` documentation, `.agents/` skill and rules

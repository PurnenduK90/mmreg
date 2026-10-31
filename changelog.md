* **[0.1.0]** Initial Release
* **[0.1.1]** Github Repository Added
    * Readme Updated
    * Subregister Mask made public to edit multiple subregister together
    * CI github action for build
* **[1.0.0]** UIO Backend, Blueprint Validation & Hardware Pre-Flight — first stable API; versioning follows SemVer from here (breaking changes only in 2.0)
    * Backends
        * `MemoryMapper` trait (`map`, `unmap`, `is_mapped`, `read` / `write` with a `Width` of 8, 16, 32 or 64 bits) with UIO (`/dev/uioN`) and `/dev/mem` backends
        * `/dev/mem` only with the `debug` feature (off by default): other builds refuse addresses outside UIO regions with `[E_DEBUG_DISABLED]` before opening anything
        * `MapperType::Auto` (default) prefers the UIO region covering an address and falls back to `/dev/mem`; `MapperType::Uio { device_path, region_index }` takes the region's window from `/sys/class/uio/uioN/maps/mapM/{addr,size}`
        * `/dev/mem` opened `O_SYNC` (uncached); 64-bit `mmap` offset on 32-bit glibc/Android so addresses at or above 2 GiB work on 32-bit ARM
    * Safety
        * Blueprint validation against `/sys/class/uio`, 32/64-bit Device Tree `of_node/reg` and leaf entries of `/proc/iomem`; any overlap with `System RAM`, kernel, `reserved` and ACPI ranges is refused
        * Zombie Hardware Pre-Flight: `of_node/status`, `power/runtime_status`, `supplier:*` links, Common Clock Framework `clk_summary` hardware-enable state (software enable count only warns), and FPGA manager `state` for FPGA-fabric devices only
        * Device held runtime-active (`power/control = on`) while mapped through `/dev/mem`, restored on unmap
        * 4-byte alignment and bounds (`offset + 4 <= size`) of every register, and valid subregister bit ranges, enforced
        * Declarative JSON `SafetyProfile` (`--validate <file>`) with strict keys (`x-*` extensions ignored); an attached profile also runs with `--force`
    * Concurrency
        * Lock protocol held per access and per read-modify-write: exclusive `flock` on `/dev/uioN` for UIO devices (interoperates with C programs locking the device the same way); per-page lock files (`/run/lock/mmreg/<page_base>-<page_end>.lock`, directory mode `1777`, never deleted) for `/dev/mem`
    * API
        * `MmregError` enum with stable `[E_*]` codes and remediation text, returned everywhere
        * `#[non_exhaustive]` on `MmregError`, `MapperType`, `Register` and `SubRegister`, so 1.x releases can add variants and fields
        * `Interface` fields private, with getters and builders (`with_force`, `with_mapper_type`, `with_safety_profile`, `with_custom_mapper`); `map()` is idempotent; `Drop` unmaps
    * CLI
        * `-d` / `--dec` and `0x` / `0d` prefixes; one prefix only, underscores allowed
        * All operands validated before any hardware access; extra arguments rejected
    * Footprint
        * `libc` is the only dependency; JSON safety profiles behind the default `profile` feature (`--no-default-features` for the smallest binary)
        * Release profile adds `panic = "abort"`
    * Build environments
        * Builds with Rust 1.75 (Yocto 5.0 "scarthgap"): edition 2021, `rust-version = "1.75"`, lockfile v3 with dependencies resolved for 1.75, `libc::flock` instead of `File::lock`; CI tests on 1.75
    * Targets
        * Release builds for `x86_64`, `aarch64`, `armv7` and `riscv64gc` Linux (glibc), plus static musl builds for `aarch64` and `armv7`; `i686` dropped
    * Tooling
        * `justfile` is the single task runner (CI uses it), checks both feature sets and an MSRV job; `Cargo.lock` committed
        * `docs/users` + `docs/developers` documentation; 1.x roadmap (`TODO.md`, `docs/developers/05-roadmap-reference.md`) and draft `mmreg-block/1` block-file format for the Python debug layer (`docs/users/07-block-files.md`)
* **[1.0.1]** JSON Safety Profiles, Zero Extra Dependencies & QA Hardening
    * Footprint & Dependencies
        * `SafetyProfile` (`--validate <file.json>`) switched to JSON with a built-in zero-dependency parser (`x-*` extension keys ignored, RFC 8259 strings/surrogate pairs and recursion depth limit); `serde` and `toml` removed so `libc` is the sole dependency in `Cargo.toml`
    * Safety & Soundness
        * `/proc/iomem` blueprint validation accepts leaf entries only (ignoring parent bus windows like `amba`/`soc`) and rejects any partial overlap with `System RAM`, `Kernel`, `reserved` or `ACPI` ranges. Behaviour change: an address inside a device entry but outside all of its child entries is now `[E_BLUEPRINT_UNMAPPED]` unless a Device Tree owner covers it
        * Explicit `MapperType::Uio` skips the `/proc/iomem` check (as auto-detected UIO already did); the UIO region bounds (`[E_UIO_ADDRESS]`) still apply
        * `Mapping::register` enforces alignment and bounds via `assert!` in release builds; zero-sized windows (`size == 0`) and `u64` address overflows are rejected across `Interface`, `Mapping` and `UioMap`
        * `Interface::map` maps the region before opening `DeviceLock` so invalid/missing UIO regions report `[E_UIO_ADDRESS]` / `[E_UIO_OPEN]` instead of `[E_LOCK_OPEN]`; explicit `MapperType` is respected during `resolve()`
        * `check_clk_summary` matches consumer device names only in consumer columns (`tokens[3..]`)
        * Single lock directory selection (`/run/lock/mmreg` whenever `/run/lock` exists) so root and non-root never split across directories; no fallback to `/tmp`, so a root-only `/run/lock` needs `/run/lock/mmreg` pre-created with mode `1777`
        * `[E_LOCK_OPEN]` remediation distinguishes UIO device files from page lock files
    * CLI
        * All CLI errors now carry a stable `[E_CLI_*]` code (`[E_CLI_MISSING_CMD]`, `[E_CLI_MISSING_ADDR]`, `[E_CLI_UNKNOWN_OPT]`); multiple `--validate` files are supported and `-h` / `--help` works after the command too

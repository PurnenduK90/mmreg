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
* **[1.1.0]** `Device` Core, UIO Discovery & Interrupts, Sequence Locks, and Multi-Width CLI
    * `Device` & `DeviceBuilder` (`src/device.rs`)
        * `Send + Sync` register window with `&self` accessors: `read_u8`, `read_u16`, `read_u32`, `read_u64`, `write_u8`, `write_u16`, `write_u32`, `write_u64`, `read`, `write`, and `update_u32(offset, mask, value)` (locked atomic read-modify-write returning the updated `u32`)
        * `Device::open_uio(name)` / `Device::uio(name)`, `Device::open_uio_path(path)` / `Device::uio_path(path)`, `Device::open_addr(base, size)` / `Device::addr(base, size)`, plus `uio::find(name) -> Result<String, MmregError>`
        * `.region(n)` to select `maps/map<n>` and `.read_only()` (`O_RDONLY` + `PROT_READ`, rejecting writes/updates/unmask with `[E_READ_ONLY]`) so read-only tools work with `0640` permissions on `/dev/uioN`
        * Blueprint, hardware pre-flight and safety-profile checks run once at `open()`; `Interface` is layered directly on top of `Device`
    * Concurrency & Sequence Guards (`src/lock.rs`)
        * Per-process `(st_dev, st_ino)` `SharedLock` registry so multiple `Device` handles in one process share a single open lock file descriptor without `flock(2)` self-deadlock and synchronize threads via a reentrant mutex + `Condvar`
        * Reentrant sequence guard (`let _g = dev.lock()?;` returning `LockGuard`) held across multi-register sequences while preserving the inter-process `flock(2)` protocol
    * UIO Interrupts (`src/mmio.rs`, `src/device.rs`)
        * `Device::irq_unmask()` (writes `1u32` to `/dev/uioN`) and `Device::irq_wait(timeout) -> Result<Option<u32>, MmregError>` (`poll(POLLIN)` + 4-byte `read`)
        * New error variants and codes: `[E_UIO_NOT_FOUND]` (listing available UIO names), `[E_UIO_AMBIGUOUS]`, `[E_UIO_REQUIRED]`, `[E_UIO_IRQ]`, and `[E_READ_ONLY]`
        * `MmregError::UnalignedWidthAccess { address, offset, width_bits }` (code `[E_UNALIGNED_ACCESS]`) for `Device` and `--width` accesses, naming the access width and the alignment it needs; `UnalignedAccess` keeps its fields and stays the 32-bit `Interface` / `read_register_at` error
        * `read_u64` / `write_u64` document that 32-bit CPUs may split them into two bus accesses
    * Diagnostics & CLI (`src/memcheck.rs`, `src/main.rs`)
        * Zero `stderr` output from the library: non-fatal pre-flight warnings are collected and returned via `Device::warnings()` and `Interface::warnings()` (printed by the CLI)
        * CLI adds `-w` / `--width <8|16|32|64>`, `mmreg read|write <uio-name> <offset> [value]` (works without the `debug` feature), and `-V` / `--version` showing enabled features
    * Documentation
        * Added UIO setup guide (`generic-uio`, `uio_pdrv_genirq.of_id`, `udev` rules) in `docs/users/02-installation.md` and generated-code guide (including `w1c`/`w1p` field rules) in `docs/users/04-library-usage.md`
* **[1.1.1]** Multi-Page Sequence Lock Bound, Zero-Alloc Single Locks, Sub-Page UIO Offset & CLI Hex-Name Fix
    * Concurrency & Sequence Guards (`src/lock.rs`, `src/device.rs`)
        * On a `/dev/mem` or custom-mapper window spanning several pages, `dev.lock()` takes every page lock in ascending address order so the sequence excludes accesses to the whole window (page lock files are open only while the guard is held); windows over `Device::MAX_SEQUENCE_PAGES` (64) pages are refused with `MmregError::LockRangeTooLarge` (`[E_LOCK_RANGE]`). Single accesses always lock their own physical page's file, as the CLI and other programs do
        * `LockGuard` stores single-lock acquisitions inline (`HeldLocks::One`) so single-register reads/writes and single-page/UIO `dev.lock()` calls do not allocate a `Vec` on the heap
    * UIO Regions & Off-Target Testing (`src/memcheck.rs`, `src/device.rs`)
        * Non-page-aligned UIO regions: newer kernels' `/sys/class/uio/uioN/maps/mapM/offset` attribute is applied when `addr` is page-aligned; an unaligned `addr` keeps its full `size` (counted from `addr`), the same window `open_addr` sees
        * `Device::uio` and `Device::uio_path` with `.with_custom_mapper(...)` and `.with_force(true)` open a 1-page mock window at address `0` even when `/sys/class/uio` is absent on the host
    * CLI & Diagnostics (`src/main.rs`, `src/error.rs`)
        * UIO device names consisting of valid hex characters (`adc0`, `dac0`, `cafe`) are no longer mistaken for extra address arguments in `mmreg read <uio-name> <offset>` and `mmreg write <uio-name> <offset> <value>`
        * Fixed spacing before `Remediation:` in `MmregError::UnalignedWidthAccess` display formatting
* **[1.2.0]** C ABI (`libmmreg.a` / `libmmreg.so` / `mmreg.h` / `mmreg.pc`) & Pure-Python `ctypes` Binding
    * Repository layout: Cargo workspace; the C ABI is the `mmreg-capi` crate in `bindings/c/` (`src/lib.rs`, `include/mmreg.h`, `mmreg.pc`, `examples/test_capi.c`), next to `bindings/python/`, so the `mmreg` crate stays a plain Rust library (no `cdylib` / `staticlib` in its builds or its dependents' builds). `default-members` keeps plain `cargo build` / `cargo test` to the Rust crate and CLI
    * Rust API: `LockGuard::into_owned()` and `OwnedLockGuard` (a lock guard detached from the `Device` borrow, for FFI layers holding a lock across calls; not `Send`)
    * C ABI (`bindings/c`, `cargo build --release -p mmreg-capi`; `--features debug` for `/dev/mem`)
        * `cdylib` (`libmmreg.so`, SONAME `libmmreg.so.1` on Linux/Android via `build.rs`) and `staticlib` (`libmmreg.a`) targets with opaque `mmreg_dev *` handle and `int` return status (`0` success, `-1` error; `mmreg_irq_wait` returns `1` interrupt, `0` timeout, `-1` error)
        * Thread-local error reporting (`mmreg_last_error()`, `mmreg_last_error_code()`, `mmreg_clear_error()`) with stable `[E_*]` codes and zero `stderr` output
        * Panic-free FFI boundary: `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]` in `bindings/c/src/lib.rs` plus `std::panic::catch_unwind` at every `extern "C"` entry point
        * Exported symbols: `mmreg_find_uio`, `mmreg_open_uio`, `mmreg_open_uio_path`, `mmreg_open_addr`, `mmreg_close`, `mmreg_rd{8,16,32,64}`, `mmreg_wr{8,16,32,64}`, `mmreg_update32`, `mmreg_lock`, `mmreg_unlock` (both on `const mmreg_dev *`), `mmreg_irq_unmask`, `mmreg_irq_wait`, `mmreg_base_address`, `mmreg_size`, `mmreg_warning_count`, `mmreg_warning`, and `mmreg_version`
        * Checked-in C header `bindings/c/include/mmreg.h` (verified against exported symbols by unit test, including `MMREG_VERSION_{MAJOR,MINOR,PATCH}` checked against `Cargo.toml`, `uint32_t`-typed `MMREG_FIELD` / `MMREG_PREP` (well defined for bit 31), `MMREG_O_RDONLY`, `MMREG_O_FORCE`, `MMREG_O_MOCK`, `MMREG_REGION(n)`), `mmreg.pc` for `pkg-config`, and standalone C test/example `bindings/c/examples/test_capi.c` (checks run under `-DNDEBUG` too; two-process C ↔ Rust `flock` interop test in `bindings/c/src/interop_tests.rs` that fails instead of skipping under `just test`); `MMREG_O_MOCK` windows are limited to 16 MiB
    * Pure-Python Binding (`bindings/python/mmreg/`)
        * Dependency-free `ctypes` package over `libmmreg` (`Device.open_uio`, `Device.open_uio_path`, `Device.open_addr`, `find_uio`, `MmregError` carrying `.code`, `with dev:` and `with dev.lock():` context managers, `read_u{8,16,32,64}`, `write_u{8,16,32,64}`, `update_u32`, `irq_unmask`, `irq_wait`)
        * Inline register and bitfield definitions for interactive experiments without a block file: `d.define("CTRL", offset=0x8, fields={"EN": "0", "MODE": "7:4"})`, supporting `d.CTRL.EN = 1`, `int(d.CTRL.EN)`, and `int(d.CTRL)`; `w1c={...}` fields are written directly and written as `0` by other fields' updates; `repr()` never reads hardware; names shadowing attributes are rejected
        * Out-of-range values, offsets, masks, field values and negative `irq_wait` timeouts raise `[E_INVALID_ARG]` instead of being truncated (`irq_wait(None)` waits without a timeout); the loaded `libmmreg` must match the package's major version
    * Packaging & Documentation
        * Yocto and Buildroot packaging guide for `--workspace` / `-p mmreg-capi` builds (`libmmreg.a`, `libmmreg.so`, `mmreg.h`, `mmreg.pc`, Python module, and release vs. `debug` CLI) (versioned `libmmreg.so.1.2.0` with `.so.1` / `.so` symlinks, pinned `SRCREV`) in `docs/users/02-installation.md` and C/Python API examples in `docs/users/04-library-usage.md`
* **[1.2.1]** C & Python Binding Hardening, Lock Waiter Notification & Workspace Doc Fix
    * Core & Concurrency (`src/device.rs`, `src/lock.rs`, `src/memcheck.rs`, `src/memregs.rs`)
        * UIO `Device` opens reuse the already-open `/dev/uioN` descriptor from `OsMapper` via `DeviceLock::from_open_file` instead of opening `/dev/uioN` a second time for locking
        * Unified UIO `maps/mapM/{addr,size,offset}` parsing behind `memcheck::uio_region_raw` and deduplicated `TargetSpec::{UioName, UioPath}` resolution, CLI `HardwareBackend` target opening, and test mock mappers
        * Marked `Device::check_access` error constructors as `#[cold]` to keep string formatting off the register read/write hot path
        * `SharedLock::acquire` notifies waiting threads on the `Condvar` if `flock(2)` fails so secondary waiters cannot stall
        * `Device::irq_wait` with no deadline (`Duration::MAX`, C `MMREG_WAIT_FOREVER`, Python `irq_wait(None)`) waits indefinitely (`poll(-1)`); a `poll` that returns at its `i32::MAX` ms limit (~24.8 days) before the deadline polls again instead of reporting a timeout
        * Lock registry and `MmregError::LockOpen` / `UioAddressInvalid` construction each live in one helper (`lock_open_error`, `memcheck::uio_address_invalid`); unit test for `DeviceLock::from_open_file` (shared registry entry, lock still excludes other processes after the source descriptor closes)
    * C ABI (`bindings/c`)
        * `CapiMockMapper` sizes its backing buffer to the window inside `map()` (previously a fixed 4 KiB for UIO mocks, so on 16K/64K-page kernels writes past 4 KiB were dropped and reads returned `0`) and is the single place enforcing `MOCK_MAX_SIZE` (16 MiB): every open function reports it as `[E_INVALID_ARG]`
        * Behaviour change: `mmreg_open_addr` rejects non-zero `MMREG_REGION(n)` flags with `[E_INVALID_ARG]` (previously ignored); documented in `mmreg.h`
        * `MMREG_WAIT_FOREVER` (`UINT64_MAX`) for `mmreg_irq_wait`
        * C interop tests compile every `bindings/c/examples/*.c` through one helper and run `quickstart.c`; a unit test pins the Python package's `__version__` to `Cargo.toml`
        * Fixed multi-line formatting of the `MMREG_O_MOCK` size-limit error message, deduplicated FFI `catch_unwind` wrappers via `ffi_status` / `ffi_ptr` with `#[cold]` error setters, and added `doc = false` to `mmreg-capi`'s `[lib]` target so `cargo doc --workspace` does not collide with `mmreg`
        * C interop test (`bindings/c/src/interop_tests.rs`) resolves `libmmreg.a` from the active test binary's profile/target directory first, uses platform-appropriate linker flags, and cleans up the child process and temporary binary via RAII guards
        * CI `msrv` (Rust 1.75) job now also builds and tests `mmreg-capi` (`bindings/c`)
    * Python Binding (`bindings/python/mmreg/`)
        * Every exception the package raises is an `MmregError`: library load failures (`E_LIB_LOAD`, also an `OSError`; previously a plain `OSError`), copying a `Device` (`E_NOT_COPYABLE`, also a `TypeError`), missing registers and fields (`E_REG_NOT_FOUND` / `E_SUBREG_NOT_FOUND`, also an `AttributeError` so `hasattr` works), bad field bit specifications (`E_SUBREG_RANGE` instead of `ValueError`; `bool` and floats rejected, decimal with leading zeros and `0x` hex accepted), overlapping fields (`E_SUBREG_RANGE`) and wrong `fields` / `w1c` / name types (`E_INVALID_ARG`); a single string is accepted as one `w1c` name
        * `MmregError` survives `pickle` (rebuilt from `(code, message)`), so `multiprocessing` workers return it intact
        * Packaging: `pip install ./bindings/python` works (the package has its own `README.md`; `readme = "../../README.md"` made setuptools refuse to build); the version comes from `__version__`; `py.typed` marks the package as typed; `just python-wheel [triple]` builds a CPU-tagged wheel (`py3-none-linux_<arch>`) bundling a release `libmmreg.so` for the host or a glibc `cross` target, and `just python-wheel-test` (in `just ci` and CI) installs it into a fresh venv and runs the tests and quickstart against it
        * Behaviour change: every `InlineField` write, `w1c` fields included, is a masked read-modify-write (`update_u32(offset, field_mask | other_w1c_mask, value)`). A `w1c` field was written directly with every other bit `0`, which also wrote `0` to the register's `rw` fields (e.g. clearing `T0INT` in an AXI Timer `TCSR0` stopped the timer); `rw` and unmodeled bits now keep their value and other pending `w1c` bits stay pending
        * `_check_uint` uses `operator.index` and rejects `float` (so e.g. `irq_wait(0.5)` raises `[E_INVALID_ARG]` instead of silently truncating to `0` ms) and `bool`, except for 1-bit fields where `d.CTRL.EN = True` keeps working
        * `Device.open_addr` validates `base` (64-bit) and `size` (`size_t`) bounds before passing to `ctypes`, `find_uio` / `open_uio` / `open_uio_path` reject embedded NUL bytes with `[E_INVALID_ARG]`, and `Device.define()` validates 4-byte alignment (`[E_UNALIGNED_ACCESS]`) and window bounds (`[E_OUT_OF_BOUNDS]`) at definition time
        * Replaced redundant `object.__getattribute__` calls on `Device` and `InlineRegister` hot paths with direct dictionary-backed attribute access and unified `InlineRegister._field` lookup; `__getattr__` raises `AttributeError` for `_`-prefixed names so `hasattr`, `copy` and half-initialised objects cannot recurse
        * `Device` refuses `copy.copy`, `copy.deepcopy` and `pickle` with `TypeError` (a copy shared the C handle and closed it twice, aborting the process)
    * Documentation
        * Access-type rules for generated code (`docs/users/04-library-usage.md`): every field writer is a masked read-modify-write with all of the register's `w1c` / `w1p` bits in the mask (previously a direct write for `w1c` / `w1p` fields); write-only and read-side-effect registers are written whole. Rust, C and README interrupt examples clear status bits with `update_u32` / `mmreg_update32`; `write_subregister` documents that it does not know about `w1c` bits; the `mmreg-block/1` draft drops its no-read shortcut for field writes
        * Runnable examples, built and run by `just test` (`just examples`): `examples/mock_quickstart.rs` and `examples/uio_irq.rs`, `bindings/c/examples/quickstart.c` and `uio_irq.c`, `bindings/python/examples/quickstart.py` and `uio_irq.py` (mock device tours run anywhere; the UIO interrupt loops are compiled only), linked from the README and the library guide
        * Error reference lists the C / Python codes (`E_INVALID_ARG`, `E_BUFFER_TOO_SMALL`, `E_INTERNAL_PANIC`, `E_LIB_LOAD`, `E_NOT_COPYABLE`, `E_UNKNOWN`) and the Python exception model; installation guide has a Python package section (wheels, `pip`, library search order, `MMREG_LIB_PATH`); `bindings/python/README.md`; user and developer indexes, task table and CI description updated
* **[1.2.2]** Yocto Recipe Fixes
    * Yocto recipe (`docs/users/02-installation.md`, `mmreg_1.2.2.bb`), checked against poky scarthgap's `cargo.bbclass`, `rust-common.bbclass`, `insane.bbclass` and `python3-manifest.json`
        * `export CARGO_PROFILE_RELEASE_STRIP = "false"`: the release profile's `strip = true` failed the `already-stripped` QA check (an error in scarthgap); Yocto now strips and fills `${PN}-dbg`
        * Removes the copies of `libmmreg.so` and `libmmreg.rlib` that `cargo_do_install` puts in `${rustlibdir}`, so `${PN}` ships the library once (as `libmmreg.so.${PV}`) and `${PN}-dev` no Rust crate
        * `${PN}-python` RDEPENDS on `python3-ctypes` (Yocto packages `ctypes` separately from `python3-core`) and installs `py.typed`
    * Python Binding (`bindings/python/mmreg/`)
        * Searches `/usr/lib64` for `libmmreg` (multilib images), before `ctypes.util.find_library`, which needs `ldconfig`, `gcc` or `ld` on the target
* **[1.3.0]** Python By-Name Access (`mmreg-block/1` & `mmreg-instances/1`), Field Decoding, Access-Type Safety, Wide Registers & `dump`
    * Block & Instance File Loader and Validator (`bindings/python/mmreg/block.py`)
        * Standard-library `json` loader and validator for `mmreg-block/1.0` and `mmreg-instances/1.0` (`Block.from_file`, `Block.from_json`, `Block.from_dict`, `load_instance_file`, `mmreg.check`)
        * Strict schema validation with `"x-*"` extension keys ignored everywhere, format version check (`[E_BLOCK_VERSION]`), duplicate JSON key and case-insensitive sibling collision detection, alignment (`regwidth / 8`), block and window bounds, register/group/array (`dim`, `stride`) and bitfield overlap checks, and enumeration value width checks (`[E_BLOCK_PARSE]`). Every diagnostic names the file, register and field
    * Name Resolution, Pre-Access Instance Checks & Discovery (`mmreg.open`, `mmreg.list`)
        * `mmreg.open(name)` resolves `name` through instance files (`$MMREG_INSTANCES`, `/etc/mmreg/instances.d/*.json`, rejecting duplicate instance names across files with `[E_INSTANCE_DUPLICATE]`), then `/sys/class/uio/uio*/name` → `device/of_node/compatible` → first matching `<compatible>.json` in `$MMREG_BLOCKS`, `/etc/mmreg/blocks/`, `/usr/share/mmreg/blocks/`. Also supports explicit `mmreg.open(block=..., uio=... | at=...)` and raw windows `mmreg.open(at=..., size=...)`
        * Instance `checks` (`name`, `addr`, `mask`, `expected`) run before the peripheral window is mapped (`[E_VALIDATION_HOOK_FAILED]`)
        * `mmreg.list()` lists discovered instances across instance files and UIO sysfs along with whether each device is present
    * By-Name Register/Field Access, Access-Type Safety, Wide Registers & `dump`
        * Case-insensitive register, group and field lookup (`d.CTRL.MODE` / `d.ctrl.mode`), register and group arrays (`d.DEBOUNCE[3].CYCLES = 10`, `d.CH[0].CFG`), single-field registers (`d.IN`), `d.define("SCRATCH", at=0x40, ...)`, and `d.wait_irq(timeout=1.0)`
        * Enumeration encoding on write (`d.CTRL.MODE = "OPEN_DRAIN"`) and decoding on read/print (`EnumValue(int)` comparing equal to both the integer value and the enum name)
        * Access-type enforcement (`sw`, `onwrite`, `onread`, `singlepulse`): masked field RMW under the device lock writes `0` to other `woclr`/`woset`/`wot`/`r`/`w`/`w1`/`singlepulse` fields, `1` to other `wzc`/`wzs`/`wzt` fields, and preserves `rw`/`rw1`/`na`/unmodeled bits; refuses field RMW on registers containing plain `sw: "w"`/`"w1"` (`[E_WRITE_ONLY]`) or `wclr`/`wset` (`[E_RMW_UNSAFE]`) fields; requires `side_effects=True` to read or field-modify `rclr`/`rset` registers (`[E_READ_SIDE_EFFECT]`); refuses writes to `sw: "r"` (`[E_READ_ONLY]`), reads of `sw: "w"` (`[E_WRITE_ONLY]`, except `singlepulse` which reads `0`), and any access to `sw: "na"` (`[E_ACCESS_DENIED]`)
        * Wide registers (`regwidth > accesswidth`) split across `regwidth / accesswidth` bus words under one `with dev.lock():` guard, reading lowest address first and writing highest address first by default (`"word_order": "lo_first"` writes lowest address first)
        * `d.dump(diff_reset=False, side_effects=False)` formats all registers with decoded fields, skipping `rclr`/`rset` registers with a note unless `side_effects=True`
    * CLI Validator (`bindings/python/mmreg/__main__.py`)
        * `python -m mmreg check <file.json> [...]` validates block and instance files (including cross-file duplicate instance checks) without hardware access for generators' CI
    * Documentation & Packaging
        * Froze the `mmreg-block/1` specification in `docs/users/07-block-files.md`, added the new error codes to `docs/users/06-troubleshooting.md`, and updated the Yocto recipe in `docs/users/02-installation.md` (`mmreg_1.3.0.bb`) to install all `bindings/python/mmreg/*.py` modules and `python3-json`
* **[1.3.1]** Python Block-File Fixes: Attribute-Safe Assignment, Instance Checks with `force` / `mock`, Lazy Binding & Module Split
    * By-Name Access (`bindings/python/mmreg/`)
        * Assigning an attribute whose name is also a register or field no longer writes hardware: lookups are case-insensitive, so `d.CTRL.name = 1` wrote a field `NAME` while `d.CTRL.name` read the register's name (likewise `reset`, `read`, `desc`, and `d.size` / `d.lock` for registers `SIZE` / `LOCK`); such assignments now raise `[E_INVALID_ARG]` naming the register or field, which `d["size"]` / `d.CTRL["name"]` reach (also for inline registers, which gained `[]`)
        * Assigning a register or field object writes its value (`d.A = d.B`), and assigning one to itself (`d.CTRL = d.CTRL`) is a no-op instead of a write-back that could clear pending bits; it was silently ignored before, also for other registers
        * Instance `checks` are skipped with `force=True` or `mock=True`, like the library's pre-flight checks, instead of reading an all-zero mock window (which made such instances impossible to open with `mock=True`); `check_reader` still runs them
        * `from mmreg import *` no longer exports `open` and `list`, which shadowed the built-ins (`mmreg.open` / `mmreg.list` are unchanged)
        * `Block.from_dict` rejects keys that are not strings with `[E_BLOCK_PARSE]` instead of an `AttributeError`, and an unknown field property names the field again
        * `dump()` also lists the inline registers (`d.define`) of a device opened with a block
    * Footprint & Structure
        * Registers, fields, array elements and group members are bound on access with `__slots__`, and `dump()` walks the block specification instead of a second bound copy of every register: a 4096-element register array went from 13.6 MB to under 0.05 MB bound (`tracemalloc`)
        * `block.py` split into `block.py` (loader and validator), `_access.py` (bound registers and `dump`) and `_resolve.py` (search paths, `mmreg.open`, `mmreg.list`); shared helpers replace duplicated child scanning, access-key parsing, array indexing, value protocol (`int`, `==`, `format`) and instance merging (`python -m mmreg check` uses the same duplicate check as `mmreg.open`)
    * Examples, Docs & Lint
        * `bindings/python/examples/by_name.py` with `blocks/acme,gpio-1.0.json` and `instances/board.json` (run and `python -m mmreg check`ed by `just examples` and the tests); the block-file part of `quickstart.py` moved there
        * Block-file section in `docs/users/04-library-usage.md`; attribute names, instance checks with `force` / `mock` and `debug` `libmmreg` in `docs/users/07-block-files.md` and `docs/users/06-troubleshooting.md`
        * `just lint-python` (`ruff check`, `ruff format --check`, rules in `bindings/python/pyproject.toml`) is part of `just lint` and CI (ruff pinned to 0.17.3)
* **[1.3.2]** Python 3.8 Import Fix, Volatile Fields in `dump` & Python 3.8 CI
    * Python Binding (`bindings/python/mmreg/`)
        * The package imports again on Python 3.8, the oldest version `requires-python = ">=3.8"` allows: 1.3.1's `_resolve.py` subscripted `collections.abc.Iterable` at import time, which needs Python 3.9 (`TypeError: 'ABCMeta' object is not subscriptable`)
        * `dump()`, `print(d.R)` and `d.R.format()` mark values hardware changes (`hw: "w"` / `"rw"`, status and counters) with `~` after the name (`CHANGE~=1`, `IN~ @ 0x14 = ...`), as the `mmreg-block/1` specification describes; the `hw` key was parsed but never shown
    * CI & Tasks
        * `python-oldest` CI job runs the Python tests on Python 3.8 (installed with `uv`); `just test-python [python]` takes the interpreter and treats warnings as errors
    * Documentation
        * `docs/users/07-block-files.md`: the `~` volatile marker, and `$MMREG_INSTANCES` entries may be directories (their `*.json` files in name order), as implemented
* **[1.4.0]** CLI Block Read, `watch`, `irq`, Output Formats (`--format hex|dec|bin|json`) & Python `watch()`
    * CLI (`src/main.rs`, `src/device.rs`)
        * Block read (`mmreg read <address | uio-name offset> -c/--count <N>`): multi-word hexdump-style read (`16` bytes per row: `16`×`u8`, `8`×`u16`, `4`×`u32`, `2`×`u64`) through `Device::read_block`
        * `watch` (`mmreg watch <address | uio-name offset> [-i/--interval <ms>] [-c/--count <N>]`): rate-limited polling (default `100` ms) printing the initial value and subsequent changes (`old -> new`, or JSON lines with `--format json`)
        * `irq` (`mmreg [-u/--unmask] irq <uio-name> [timeout_ms]`): waits for a UIO interrupt from the shell (`poll(POLLIN)` + 4-byte `read`), optionally unmasking first (`-u` / `--unmask`), printing the kernel cumulative interrupt count, or `[E_CLI_IRQ_TIMEOUT]` with exit status `2` on timeout (`1` stays for failures)
        * Output formats (`-F` / `--format <hex|dec|bin|json>`, default `hex`): hexadecimal (`0x...`), decimal, zero-padded binary (`0b...`), and structured JSON for `read` (single and `--count N`), `write`, `watch`, and `irq`
        * `--count`, `--interval` and the `irq` timeout are decimal quantities whatever `--dec` says (`0x` selects hex), unlike addresses, offsets and values
        * New CLI error codes: `[E_CLI_MISSING_COUNT]`, `[E_CLI_INVALID_COUNT]`, `[E_CLI_MISSING_INTERVAL]`, `[E_CLI_INVALID_INTERVAL]`, `[E_CLI_MISSING_FORMAT]`, `[E_CLI_INVALID_FORMAT]`, `[E_CLI_MISSING_UIO]`, `[E_CLI_INVALID_TIMEOUT]`, `[E_CLI_IRQ_TIMEOUT]`, and `[E_CLI_OPT_CONFLICT]` (an option the command does not use)
    * Rust API (`src/device.rs`)
        * `Device::read_block(offset, width, &mut [u64])`: reads consecutive words after checking the whole range, holding one lock per page (of the kernel's page size, or the UIO device lock) across the words in that page
    * Python `watch()` (`bindings/python/mmreg/`)
        * `d.watch(*targets, interval=0.1, count=None, only_volatile=False, side_effects=False, file=None)` on `Device`, plus `.watch(...)` on every register and field (`BlockRegister`, `BlockField`, `InlineRegister`, `InlineField`): polls registers or fields by name (`"CTRL"`, `"IRQ_STATUS.CHANGE"`, `"DEBOUNCE[2].CYCLES"`) or object every `interval` seconds (at least 1 ms, like the CLI) until `count` polls or Ctrl-C, skipping `rclr`/`rset` registers unless `side_effects=True`, and printing initial values and changes (`old -> new`) formatted as `dump` formats them (`~` marks volatile fields); it keeps only each target's last value, so memory stays constant however long it runs
    * Repository
        * `.gitattributes` (`* text=auto eol=lf`): the 1.3.1 / 1.3.2 commits stored 13 files (Python modules and tests, docs, `TODO.md`, `ci.yml`, `justfile`) with CRLF line endings written by Windows tools; a CRLF `justfile` breaks its `#!/usr/bin/env bash` recipes (`python-wheel`, `python-wheel-test`) on Linux. All are LF again

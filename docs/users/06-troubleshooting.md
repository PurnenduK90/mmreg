# 6. Troubleshooting & Error Reference

Every error (CLI, Rust, C and Python) starts with a stable `[E_*]` code (`MmregError::code()` in Rust,
`mmreg_last_error_code()` in C, `MmregError.code` in Python),
names the resource or sysfs path involved, and ends with a `Remediation:` hint.

---

## Blueprint

| Code | Meaning | Remediation |
| :--- | :--- | :--- |
| `E_BLUEPRINT_UNMAPPED` | The range is in no UIO region, Device Tree `reg` or leaf `/proc/iomem` entry. | 1. Run with `sudo` (non-root sees `/proc/iomem` as `00000000-00000000`). 2. Check the peripheral's Device Tree node or UIO driver is loaded. 3. Use `--force` only if you are sure the bus will answer. |
| `E_BLUEPRINT_RAM` | The range overlaps System RAM, kernel memory, a reserved or an ACPI range. | Check the peripheral's physical address; RAM is not MMIO. For debugging DMA scatter-gather descriptors in coherent RAM/reserved carveouts, expose the carveout as a UIO map or pass `--force` with a `debug` build. |

## Hardware Pre-Flight

| Code | Meaning | Remediation |
| :--- | :--- | :--- |
| `E_ZOMBIE_DT_STATUS` | `of_node/status` is not `okay` (e.g. `disabled`). | Enable the node (`status = "okay";`) or apply an overlay. |
| `E_ZOMBIE_RUNTIME_PM` | `power/runtime_status` is `suspended`, `suspending` or `error`. | `echo on \| sudo tee /sys/bus/platform/devices/<dev>/power/control`, or let its driver keep it active. |
| `E_ZOMBIE_SUPPLIER_PM` | A `supplier:*` (clock, reset, regulator, power domain) is suspended or in error. | Bring the supplier named in the message up first. |
| `E_ZOMBIE_CLOCK_GATED` | A clock of the device is off in hardware (`clk_summary` hardware enable `N`). | Enable the clock through its driver or Device Tree. |
| `E_ZOMBIE_FPGA_STATE` | The device is on an FPGA fabric and an FPGA manager is not `operating`. | Load the bitstream (e.g. `sudo fpgautil -b design.bit.bin`) and check `/sys/class/fpga_manager/fpga0/state`. |

## Safety Profiles

| Code | Meaning | Remediation |
| :--- | :--- | :--- |
| `E_VALIDATION_HOOK_FAILED` | A profile check, or a Python instance file's `checks` entry, read a value that does not match `expected` under `mask`. | Satisfy the prerequisite named in the message (clock, reset, isolation). |
| `E_VALIDATION_PROFILE_PARSE` | The profile file is unreadable or invalid JSON (unknown key, missing key, no `"checks"`, number too large). | Fix the file; see the format in [Safety & Hardware Pre-Flight](05-safety-and-preflight.md#stage-3-safety-profiles---validate-profilejson). |

## Backends & UIO

| Code | Meaning | Remediation |
| :--- | :--- | :--- |
| `E_DEVMEM_OPEN` | `/dev/mem` cannot be opened (usually permission denied). | Run with `sudo`, grant `CAP_SYS_RAWIO` (`sudo setcap cap_sys_rawio+ep $(which mmreg)`), or bind the device to a UIO driver. |
| `E_DEVMEM_MMAP` | `mmap` of `/dev/mem` failed; `Operation not permitted` comes from `CONFIG_STRICT_DEVMEM` / `CONFIG_IO_STRICT_DEVMEM`. | Expose the peripheral through UIO (`compatible = "generic-uio"`). |
| `E_DEBUG_DISABLED` | The address is in no UIO region, and this build has no `/dev/mem` access. | Bind the peripheral to UIO, or use an `mmreg` built with `--features debug` (development images). |
| `E_UIO_NOT_FOUND` | No UIO device in `/sys/class/uio/uio*/name` matches the requested name (lists the available UIO names). | Check `/sys/class/uio/uio*/name`, verify the Device Tree node has `compatible = "generic-uio"` and `status = "okay"`, and ensure `uio_pdrv_genirq.of_id=generic-uio` is set. |
| `E_UIO_AMBIGUOUS` | More than one `/dev/uioN` device has the same `/sys/class/uio/uioN/name`. | Give each Device Tree node a unique `linux,uio-name` property, or open a specific device path via `Device::open_uio_path("/dev/uioN")`. |
| `E_UIO_OPEN` | `/dev/uioN` cannot be opened. | Check `ls -l /dev/uio*` and the `udev` rule / permissions (`0660` for read-write, `0640` for read-only). |
| `E_UIO_MMAP` | `mmap` of `/dev/uioN` failed. | Check the region's `/sys/class/uio/uioN/maps/mapM/size`. |
| `E_UIO_ADDRESS` | With an explicit UIO device/region, the range is outside the region's sysfs `addr`/`size`, or those files are unreadable. | Keep the access inside the region, check `.region(index)`, or use `MapperType::Auto`. |
| `E_UIO_REQUIRED` | `irq_unmask` or `irq_wait` was called on a `Device` not backed by `/dev/uioN`. | Open the peripheral through UIO (`Device::open_uio` or `Device::open_uio_path`) to use UIO interrupts. |
| `E_UIO_IRQ` | UIO interrupt `poll`, `read`, or unmask `write` failed on `/dev/uioN`. | Verify the UIO device has an `interrupts` property in the Device Tree and is bound to an interrupt-capable UIO driver such as `uio_pdrv_genirq`. |

## Layout, Permissions, and Locking

| Code | Meaning | Remediation |
| :--- | :--- | :--- |
| `E_UNALIGNED_ACCESS` | The address or byte offset is not naturally aligned to the access width (`1`, `2`, `4`, or `8` bytes). | Align the address and offset to a multiple of the access width in bytes. |
| `E_OUT_OF_BOUNDS` | `offset + access_bytes` exceeds the mapped window size (or `size == 0`). | Increase the window size or fix the byte offset. |
| `E_READ_ONLY` | A register write, `update_u32`, or `irq_unmask` was attempted on a `Device` opened with `.read_only()`. | Open the `Device` without `.read_only()` and ensure write permission (`0660`) on `/dev/uioN`. |
| `E_LOCK_OPEN` | The lock file cannot be opened: `/dev/uioN` (UIO devices) or the page lock file (`/dev/mem`). | For UIO, give the user read access to `/dev/uioN`; for `/dev/mem`, pre-create `/run/lock/mmreg` with mode `1777` (`sudo mkdir -m 1777 -p /run/lock/mmreg`) if `/run/lock` is root-only (`0755`). |
| `E_LOCK_ACQUIRE` | `flock` on the lock file failed. C / Python: `mmreg_unlock` (leaving `with d.lock():`) without a matching lock, or from a thread that does not hold it. | Check the filesystem supports `flock`; unlock on the thread that locked. |
| `E_LOCK_RANGE` | `dev.lock()` on a `/dev/mem` or custom-mapper window covering more than 64 pages (one open lock file per page). | Open a smaller `Device` covering only the registers of the sequence. |

## API

| Code | Meaning | Remediation |
| :--- | :--- | :--- |
| `E_REG_NOT_FOUND` | No register of that name in the `Interface` (Python: no `define()`d register or attribute of that name on the `Device`). | Use one of the names listed in the message, or `define()` the register first. |
| `E_SUBREG_NOT_FOUND` | No subregister of that name in the register (Python: a field, or a `w1c=` name that is not one of the register's fields). | Use one of the names listed in the message. |
| `E_SUBREG_RANGE` | A subregister has `msb < lsb` or `msb > 31`. Python: a field bit specification that is not `"7:4"`, `"[7:4]"`, `"0"`, `0` or `(7, 4)`, or fields that overlap. | Fix the bit range. |

## C & Python Bindings

These codes come from the C ABI (`mmreg_last_error_code()`) and the Python package
(`MmregError.code`) only; every other code above is shared with Rust and the CLI.

| Code | Meaning | Remediation |
| :--- | :--- | :--- |
| `E_INVALID_ARG` | A `NULL` pointer, unknown flag bits, `MMREG_O_MOCK` without `MMREG_O_FORCE`, `MMREG_REGION(n)` on `mmreg_open_addr`, or a mock window over 16 MiB. Python also: a value, offset, mask or timeout that is not an integer in range (`bool` only for 1-bit fields, no `float`), a string with a NUL byte, an invalid register or field name (not an identifier, starts with `_`, or shadows an attribute such as `size`), `fields` / `w1c` of the wrong type, a closed `Device`, an enum name the field's enumeration does not have, an array index that is not an integer, or assigning an attribute whose name is also a register or field (`d.size = 1` with a register `SIZE`: write `d["size"] = 1`). | Fix the argument named in the message. |
| `E_BUFFER_TOO_SMALL` | `mmreg_find_uio`'s buffer cannot hold the `/dev/uioN` path and its NUL. | Pass a larger buffer (the message gives the size needed; 64 bytes is plenty). |
| `E_INTERNAL_PANIC` | A bug in mmreg panicked inside a C ABI call; the panic was caught at the boundary. | Report it with the message; the handle is still valid but the failed call had no effect or an unknown one. |
| `E_LIB_LOAD` | Python: `libmmreg` was not found, cannot be loaded (built for another CPU or C library), or has a different major version than the package. The error is also an `OSError`. | Install a wheel that bundles the library, install `libmmreg.so`, or set `MMREG_LIB_PATH`; see [Installation](02-installation.md#python-package). |
| `E_NOT_COPYABLE` | Python: `copy.copy`, `copy.deepcopy` or `pickle` of a `Device`, which owns its C handle. The error is also a `TypeError`. | Share the `Device` object instead of copying it. |
| `E_BLOCK_VERSION` | Python: `format` in a block or instance file is not a supported `mmreg-block/1.<minor>` or `mmreg-instances/1.<minor>`. | Upgrade `mmreg` or generate a `1.0` file; see [Block Files](07-block-files.md#versioning). |
| `E_BLOCK_PARSE` | Python: invalid JSON, unknown property key or value, misaligned register, overlapping registers or fields, out-of-bounds offset or bit range, or invalid enum in a block or instance file. | Fix the file, register and field named in the message (`python -m mmreg check <file.json>`). |
| `E_BLOCK_NOT_FOUND` | Python: block or instance file cannot be found on the search path (`$MMREG_BLOCKS`, `/etc/mmreg/blocks`, `/usr/share/mmreg/blocks`). The error is also an `OSError`. | Install the `<compatible>.json` block file or set `MMREG_BLOCKS`. |
| `E_INSTANCE_DUPLICATE` | Python: two instance files define the same instance name. | Remove or rename the duplicate entry named in the message. |
| `E_WRITE_ONLY` | Python: reading a write-only (`sw: "w"` / `"w1"`) register or field, or attempting a field read-modify-write on a register containing a plain write-only field. | Write the whole register (`d.R = value`) instead of reading or modifying a single field. |
| `E_RMW_UNSAFE` | Python: attempting a field read-modify-write on a register containing a `wclr` or `wset` field, which any write modifies. | Write the whole register (`d.R = value`). |
| `E_READ_SIDE_EFFECT` | Python: reading or field-modifying a register with `onread: "rclr"` or `"rset"` without `side_effects=True`. | Pass `side_effects=True` (`d.R.read(side_effects=True)` or `d.R.F.write(v, side_effects=True)`), or write the whole register. |
| `E_ACCESS_DENIED` | Python: reading or writing a reserved (`sw: "na"`) field or register. | Do not access reserved fields. |
| `E_UNKNOWN` | Python: a C call failed without setting an error code (should not happen). | Report it. |

In Python every exception the package raises is an `MmregError`. A missing register or field is
also an `AttributeError`, so `hasattr(d, "CTRL")` and `getattr(d, "CTRL", None)` work.

## CLI

| Code | Meaning |
| :--- | :--- |
| `E_CLI_INVALID_ADDR` / `E_CLI_INVALID_OFFSET` / `E_CLI_INVALID_VAL` | The address (64-bit), byte offset (32-bit), or value (`8`/`16`/`32`/`64`-bit) is not a valid number for its width. |
| `E_CLI_MISSING_WIDTH` / `E_CLI_INVALID_WIDTH` | `-w` / `--width` was given without a value or with a value other than `8`, `16`, `32`, or `64`. |
| `E_CLI_MISSING_CMD` | No command (`read` or `write`) given. |
| `E_CLI_MISSING_ADDR` | `read` or `write` without an address or UIO name. |
| `E_CLI_MISSING_VAL` | `write` without a value. |
| `E_CLI_MISSING_PROFILE` | `--validate` without a file. |
| `E_CLI_UNKNOWN_OPT` | Unrecognized `-` / `--` option. |
| `E_CLI_UNKNOWN_CMD` | Command other than `read` / `write`. |
| `E_CLI_EXTRA_ARGS` | More operands than the command takes. |
| `E_CLI_NO_PROFILE` | `--validate` on a build without the `profile` feature. |

---

## FAQ

**A read works with `sudo` but not without.** Non-root users see no addresses in `/proc/iomem` and
cannot open `/dev/mem`. Bind the device to UIO and give your user access to `/dev/uioN`.

**Two processes still clobber a bitfield.** The lock is advisory: only programs that take it are
excluded. For UIO devices that includes any program holding `flock(LOCK_EX)` on its own
`/dev/uioN` descriptor; a kernel driver, `devmem`, or a program mapping the device without locking
is not excluded.

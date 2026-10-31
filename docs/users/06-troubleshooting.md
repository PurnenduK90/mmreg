# 6. Troubleshooting & Error Reference

Every error (CLI and library) starts with a stable `[E_*]` code (`MmregError::code()` in Rust),
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
| `E_VALIDATION_HOOK_FAILED` | A profile check read a value that does not match `expected` under `mask`. | Satisfy the prerequisite named in the message (clock, reset, isolation). |
| `E_VALIDATION_PROFILE_PARSE` | The profile file is unreadable or invalid JSON (unknown key, missing key, no `"checks"`, number too large). | Fix the file; see the format in [Safety & Hardware Pre-Flight](05-safety-and-preflight.md#stage-3-safety-profiles---validate-profilejson). |

## Backends

| Code | Meaning | Remediation |
| :--- | :--- | :--- |
| `E_DEVMEM_OPEN` | `/dev/mem` cannot be opened (usually permission denied). | Run with `sudo`, grant `CAP_SYS_RAWIO` (`sudo setcap cap_sys_rawio+ep $(which mmreg)`), or bind the device to a UIO driver. |
| `E_DEVMEM_MMAP` | `mmap` of `/dev/mem` failed; `Operation not permitted` comes from `CONFIG_STRICT_DEVMEM` / `CONFIG_IO_STRICT_DEVMEM`. | Expose the peripheral through UIO (`compatible = "generic-uio"`). |
| `E_DEBUG_DISABLED` | The address is in no UIO region, and this build has no `/dev/mem` access. | Bind the peripheral to UIO, or use an `mmreg` built with `--features debug` (development images). |
| `E_UIO_OPEN` | `/dev/uioN` cannot be opened. | Check `ls -l /dev/uio*` and the udev rule / permissions. |
| `E_UIO_MMAP` | `mmap` of `/dev/uioN` failed. | Check the region's `/sys/class/uio/uioN/maps/mapM/size`. |
| `E_UIO_ADDRESS` | With an explicit `MapperType::Uio`, the range is outside the region's sysfs `addr`/`size`, or those files are unreadable. | Keep the `Interface` inside the region, or use `MapperType::Auto`. |

## Layout and Locking

| Code | Meaning | Remediation |
| :--- | :--- | :--- |
| `E_UNALIGNED_ACCESS` | The base address or a register offset is not a multiple of 4. | Use 4-byte-aligned addresses and offsets. |
| `E_OUT_OF_BOUNDS` | A register's `offset + 4` exceeds the `Interface` size. | Increase the size or fix the offset. |
| `E_LOCK_OPEN` | The lock file cannot be opened: `/dev/uioN` (UIO devices) or the page lock file (`/dev/mem`). | For UIO, give the user read access to `/dev/uioN`; for `/dev/mem`, pre-create `/run/lock/mmreg` with mode `1777` (`sudo mkdir -m 1777 -p /run/lock/mmreg`) if `/run/lock` is root-only (`0755`). |
| `E_LOCK_ACQUIRE` | `flock` on the lock file failed. | Check the filesystem supports `flock`. |

## API

| Code | Meaning | Remediation |
| :--- | :--- | :--- |
| `E_REG_NOT_FOUND` | No register of that name in the `Interface`. | Use one of the names listed in the message. |
| `E_SUBREG_NOT_FOUND` | No subregister of that name in the register. | Use one of the names listed in the message. |
| `E_SUBREG_RANGE` | A subregister has `msb < lsb` or `msb > 31`. | Fix the bit range. |

## CLI

| Code | Meaning |
| :--- | :--- |
| `E_CLI_INVALID_ADDR` / `E_CLI_INVALID_VAL` | The address (64-bit) or value (32-bit) is not a valid number. |
| `E_CLI_MISSING_CMD` | No command (`read` or `write`) given. |
| `E_CLI_MISSING_ADDR` | `read` or `write` without an address. |
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

---
name: mmreg
description: >-
  Safe, concurrent 32-bit memory-mapped I/O (MMIO) register access and hardware pre-flight debugging using `mmreg`. Use when reading or writing physical hardware registers on Linux/embedded/FPGA targets, integrating the `mmreg` Rust crate (`Interface`, `Register`, `SubRegister`, `MapperType`, `SafetyProfile`), or diagnosing `[E_BLUEPRINT_*]` and `[E_ZOMBIE_*]` hardware pre-flight errors.
---

# `mmreg` Skill — Safe MMIO Register Access & Pre-Flight Debugging

Use `mmreg` instead of raw `devmem2` whenever reading or writing 32-bit memory-mapped hardware registers on Linux, ARM/AArch64 SoCs, or FPGA platforms (such as Xilinx Zynq / ZynqMP / Kria).

---

## 1. CRITICAL: Target Machine vs. Development Machine

When assisting a user with `mmreg`, **the physical hardware registers live on the target embedded Linux machine, NOT on the development host PC**:

1. **Verify Target Connection First**:
   - Before running any `mmreg read` or `mmreg write` command, check whether you are executing on the development host or the embedded target (`uname -a`, or check if the user provided an SSH target such as `ubuntu@kria` or `root@192.168.x.x`).
   - If you are on the development host and do not yet know the target's SSH host/IP/user, **ask the user how to connect to the target board** before running any `mmreg` register command.
   - Execute target commands remotely via SSH:
     ```sh
     ssh <user>@<target> "sudo mmreg read 0xA000_0000"
     ```
2. **Always Use `sudo` on the Target Unless Using Non-Root UIO**:
   - Accessing `/dev/mem`, reading `/sys/kernel/debug/clk/clk_summary`, and viewing non-zero physical addresses in `/proc/iomem` on Linux require `root` (`sudo`) privileges.
   - Always run `sudo mmreg ...` on the target unless the device is exposed via `/dev/uioN` with non-root udev permissions.
3. **Never Default to `--force` (`with_force(true)`)**:
   - Always invoke `sudo mmreg read <addr>` or `read_register_at(addr, false)` with validation enabled first.
   - Bypassing validation on an unmapped, unclocked, or unprogrammed FPGA address causes an unrecoverable AXI bus hang and kernel RCU stall.

---

## 2. CLI Quick Reference (Run on Target via SSH)

```sh
# Safe read on target (validates blueprint + hardware pre-flight, prefers UIO over /dev/mem)
ssh <user>@<target> "sudo mmreg read 0x4000_0000"

# Safe write on target
ssh <user>@<target> "sudo mmreg write 0x4000_0000 0xDEADBEEF"

# Run with a declarative TOML Pre-Flight Safety Profile (--validate)
ssh <user>@<target> "sudo mmreg --validate zynqmp_safety.toml read 0xA000_0000"

# Force override (ONLY after verifying hardware clock/power/FPGA state manually)
ssh <user>@<target> "sudo mmreg --force read 0x4000_0000"
```

### Declarative Safety Profile (`zynqmp_safety.toml`)
```toml
# ZynqMP AXI Pre-Flight Safety Profile
name = "ZynqMP_AXI_Safety"

[[checks]]
name = "PCAP_DONE"
addr = 0xFFCA_3010
mask = 0x4
expected = 0x4

[[checks]]
name = "ISOLATION_REMOVED"
addr = 0xFF41_0000
mask = 0xF
expected = 0x0
```

---

## 3. Diagnostic Error Code Runbook (`[E_*]`)

| Error Code | Root Cause | Agent Remediation Workflow (on Target) |
| :--- | :--- | :--- |
| `[E_BLUEPRINT_UNMAPPED]` | Target address range is absent from `/sys/class/uio`, `/sys/bus/platform/devices/*/of_node/reg`, and `/proc/iomem`. | 1. Re-run with `sudo` (`/proc/iomem` hides addresses as `00000000-00000000` for non-root).<br>2. Inspect `ls /sys/bus/platform/devices/` and `sudo cat /proc/iomem` on target.<br>3. Only use `--force` if the IP block is intentionally omitted from the Device Tree. |
| `[E_BLUEPRINT_RAM]` | Target address range falls inside `System RAM`, `Kernel code`, `Kernel data`, or `reserved` in `/proc/iomem`. | Verify the physical peripheral base address; do not map system RAM or kernel memory as MMIO. |
| `[E_UNALIGNED_ACCESS]` | Base address or register offset is not 4-byte aligned (`% 4 != 0`). | Ensure base address and all 32-bit register offsets are multiples of 4 bytes. |
| `[E_OUT_OF_BOUNDS]` | Register access (`offset + 4`) exceeds the `Interface` mapped size. | Increase the `Interface` size so `offset + 4 <= size`. |
| `[E_ZOMBIE_FPGA_STATE]` | Target device resides on the FPGA fabric (`amba_pl`, `fpga-region`), and `/sys/class/fpga_manager/*/state` is not `"operating"`. | 1. Check `cat /sys/class/fpga_manager/fpga0/state` on target.<br>2. Program the bitstream (e.g., `sudo fpgautil -b design.bit.bin`) and verify state becomes `operating` before retrying. |
| `[E_ZOMBIE_CLOCK_GATED]` | Target device's clock in `/sys/kernel/debug/clk/clk_summary` has `hardware enable == N`. | Enable the peripheral clock via its Linux driver, device tree overlay, or clock controller before accessing registers. |
| `[E_ZOMBIE_RUNTIME_PM]` | Owning device's `power/runtime_status` in sysfs is `"suspended"`, `"suspending"`, or `"error"`. | 1. Wake the device: `echo on \| sudo tee /sys/bus/platform/devices/<dev>/power/control`.<br>2. Verify `cat .../power/runtime_status` reads `active`. |
| `[E_ZOMBIE_SUPPLIER_PM]` | Upstream clock/power/reset supplier (`supplier:*`) is `"suspended"`, `"suspending"`, or `"error"`. | Inspect the supplier device path reported in the error message and ensure its driver/clock controller is active. |
| `[E_ZOMBIE_DT_STATUS]` | Device Tree `of_node/status` is `"disabled"` or `"fail"`. | Enable the node (`status = "okay";`) in the Device Tree or overlay before accessing the peripheral. |
| `[E_VALIDATION_HOOK_FAILED]` | A register check in `--validate <profile.toml>` did not match `expected`. | Check the prerequisite PS/SLCR register reported in the error message (e.g., `PCAP_DONE` or `ISOLATION_REMOVED`). |
| `[E_DEVMEM_OPEN]` | Permission denied opening `/dev/mem`. | Run with `sudo`, grant `CAP_SYS_RAWIO` (`sudo setcap cap_sys_rawio+ep $(which mmreg)`), or bind the device to `generic-uio`. |
| `[E_DEVMEM_MMAP]` | `mmap` on `/dev/mem` failed (often `EPERM` from `CONFIG_STRICT_DEVMEM`). | Expose the peripheral via UIO (`compatible = "generic-uio"`) so `mmreg` maps `/dev/uioN` instead of `/dev/mem`. |
| `[E_UIO_OPEN]` / `[E_UIO_MMAP]` / `[E_UIO_ADDRESS]` | Cannot open/map `/dev/uioN`, or (explicit `MapperType::Uio`) the range is outside `/sys/class/uio/uioN/maps/mapM/{addr,size}`. | Check `ls -l /dev/uio*` permissions and `cat /sys/class/uio/uio*/maps/map*/{addr,size}`; prefer `MapperType::Auto`. |
| `[E_LOCK_OPEN]` / `[E_LOCK_ACQUIRE]` | The per-page lock file in `/run/lock/mmreg` (or `/tmp/mmreg-lock`) cannot be opened or locked. | Make the lock directory mode `1777` for all users of the device. |
| `[E_REG_NOT_FOUND]` / `[E_SUBREG_NOT_FOUND]` | Unknown register or bitfield name on `Interface`. | Read the `Available registers: [...]` or `Available subregisters: [...]` list included in the error message. |
| `[E_SUBREG_RANGE]` | A `SubRegister` has `msb < lsb` or `msb > 31`. | Fix the bit range. |
| `[E_CLI_*]` | Bad CLI usage (invalid number, missing value, extra arguments, `--validate` on a build without the `profile` feature). | Read the message; run `mmreg --help`. |

---

## 4. Rust Library Pattern

```rust
use mmreg::{Interface, MapperType, Register, SafetyProfile, SubRegister};

// SafetyProfile::parse needs the `profile` feature (default); without it, build the struct in code.
let safety = SafetyProfile::parse(r#"
name = "ZynqMP_AXI_Safety"
[[checks]]
name = "PCAP_DONE"
addr = 0xFFCA_3010
mask = 0x4
expected = 0x4
"#)?;

let mut iface = Interface::new(
    "axi_gpio",
    0xA000_0000,
    0x1000,
    vec![Register::new(
        "ctrl",
        0x00,
        vec![
            SubRegister::new("enable", 0, 0),
            SubRegister::new("mode", 7, 4),
        ],
    )],
)
.with_safety_profile(safety);

iface.map()?; // Runs SafetyProfile + Blueprint + Hardware Pre-Flight once
iface.write_subregister("ctrl", "mode", 0x3)?; // RMW under a per-page inter-process lock
iface.write_subregister("ctrl", "enable", 1)?;
let val = iface.read_register("ctrl")?;
iface.unmap(); // Automatically called on Drop as well
```

# 1. About `mmreg`

`mmreg` is a Rust library and command-line utility for **safe, concurrent 32-bit memory-mapped I/O (MMIO) register access** on Linux and embedded platforms (ARM, AArch64, x86_64, RISC-V, etc.).

---

## Why `mmreg` Exists

In embedded Linux development, engineers frequently need to inspect or modify hardware registers from user space—for board bring-up, debugging, telemetry, or lightweight control daemons.

Historically, tools like `devmem2` have been used for this purpose. However, raw `/dev/mem` utilities carry severe operational risks:

1. **Unmapped Address Accesses (Hard Failures & RCU Stalls)**:
   - Reading or writing an unmapped physical address on an AXI/AHB bus often causes the interconnect to wait indefinitely for a slave response or raise an unhandled bus fault.
   - This locks up the CPU core in an uninterruptible state, triggering Linux kernel **RCU stalls** (`rcu_sched detected stalls`) and whole-system freezes.
2. **Zombie Hardware States**:
   - Even if an address is listed in the Device Tree or `/proc/iomem`, the hardware block behind it might be powered down, held in reset, clock-gated, or reside in an unprogrammed FPGA fabric. Accessing a "zombie" peripheral hangs the system just like an unmapped address.
3. **Concurrent Access Races**:
   - Multiple processes or scripts performing read-modify-write operations on the same register bitfields via `/dev/mem` can silently clobber each other's state.

---

## How `mmreg` Solves These Problems

`mmreg` converts catastrophic **hard failures** (system hangs and panics) into recoverable **soft failures** (clear user-space error messages) through four layers of protection:

| Layer | Capability | Description |
| :--- | :--- | :--- |
| **1. Blueprint Validation** | `/sys/class/uio`, `/proc/iomem`, Device Tree `reg` | The range must be known to the running kernel, and must not be System RAM, kernel memory, reserved or ACPI memory. |
| **2. Hardware Pre-Flight** | DT `status`, runtime PM, supplier links, `clk_summary`, FPGA manager | The owning peripheral must be enabled (`"okay"`), runtime-active, have active suppliers and ungated clocks, and—if on an FPGA fabric—a programmed (`"operating"`) FPGA. |
| **3. Safe Backend Priority** | UIO (`/dev/uioN`) $\rightarrow$ `/dev/mem` | Prefers kernel-managed UIO mappings; with `/dev/mem`, holds the device runtime-active while mapped. |
| **4. Concurrency & Bitfields** | Per-page lock, aligned volatile access | Every access and read-modify-write holds an inter-process lock on its physical page; `Register` / `SubRegister` give names to registers and bitfields. |

---

## References & Background Reading

- [Why `devmem2` is a Loaded Gun: Solving RCU Stalls with `mmreg`](https://cadiora.com/library/mmreg/hard_vs_soft_failure/)
- [Beyond the Blueprint: Why `mmreg` Needs a Hardware Pre-Flight](https://cadiora.com/library/mmreg/zombie_hardware/)
- [Building Safe and Concurrent Memory-Mapped Register Access in Rust: The `mmreg` Journey](https://cadiora.com/library/mmreg/safe_mmio/)

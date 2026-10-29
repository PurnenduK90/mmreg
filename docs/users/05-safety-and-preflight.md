# 5. Safety & Hardware Pre-Flight

Before mapping `[address, address + size)`, `mmreg` checks that the range is a real, live device.
`--force` (`with_force(true)`) skips stages 1 and 2; alignment and bounds are always enforced, and
an attached safety profile (stage 3) always runs.

---

## Stage 1: Blueprint Validation

1. **UIO (`/sys/class/uio/uio*/maps/map*`)**: if a region's `addr`/`size` covers the range, the
   access goes through that `/dev/uioN` region, and the UIO device's parent is the owning device.
2. **Device Tree (`/sys/bus/platform/devices/*/of_node/reg`)**: the platform device whose `reg`
   covers the range is the owning device. 32- and 64-bit cell layouts and multi-entry `reg` arrays
   are supported.
3. **`/proc/iomem`**: a range inside `System RAM`, `Kernel code/data/rodata/bss`, `Crash kernel`,
   `reserved` or `ACPI ...` is refused (`[E_BLUEPRINT_RAM]`). Without an owning device from 1 or 2,
   the range must be listed here.

A range found nowhere is refused with `[E_BLUEPRINT_UNMAPPED]`. As non-root, `/proc/iomem` shows
every range as `00000000-00000000`, so only UIO and Device Tree can vouch for an address.

---

## Stage 2: Zombie Hardware Pre-Flight

Knowing the address does not mean the block behind it is awake. For the owning device:

1. **Device Tree status** (`of_node/status`): must be `"okay"` / `"ok"` when present
   (`[E_ZOMBIE_DT_STATUS]`).
2. **Runtime PM** (`power/runtime_status`): must not be `suspended`, `suspending` or `error`
   (`[E_ZOMBIE_RUNTIME_PM]`).
3. **Suppliers** (`supplier:*` device links to clock, reset, regulator and power-domain providers):
   same rule (`[E_ZOMBIE_SUPPLIER_PM]`).
4. **Clocks** (`/sys/kernel/debug/clk/clk_summary`, needs debugfs and root): a clock of the device
   (named in `of_node/clock-names` or listing the device as consumer) whose *hardware enable*
   column is `N` is refused (`[E_ZOMBIE_CLOCK_GATED]`). When the kernel has no hardware-enable
   column, an enable count of 0 only prints a warning: clocks of UIO devices and clocks kept by
   `clk_ignore_unused` run with a count of 0.
5. **FPGA manager**, only for devices on an FPGA fabric (path through `amba_pl`, `axi_pl`,
   `pl-bus`, `fpga-region`, `fpga-full`, `fpga-bus`, or an FPGA supplier link): every
   `/sys/class/fpga_manager/*/state` must be `operating` (`[E_ZOMBIE_FPGA_STATE]`). SoC peripherals
   are never blocked by an unprogrammed FPGA.

**While mapped**: a UIO device stays runtime-active while `/dev/uioN` is open. For `/dev/mem`,
`mmreg` sets the owning device's `power/control` to `on` and restores the previous value on unmap
(root only; otherwise a warning is printed).

---

## Stage 3: Safety Profiles (`--validate <profile.toml>`)

Sysfs does not expose every clock gate, reset line or isolation bit, especially on SoCs whose clock
and reset controllers are programmed directly (`CRL_APB`, `CRF_APB`, `PMU_GLOBAL` on Zynq
UltraScale+). A profile lists register checks that must hold first:
$$\text{read}(\text{addr}) \ \& \ \text{mask} = \text{expected} \ \& \ \text{mask}$$

### Example: Zynq UltraScale+ GEM3 Ethernet (`zynqmp_gem3.toml`)

Before touching GEM3 (`0xFF0E_0000`), its reference clock must be active and its reset deasserted
in the low-power-domain clock and reset controller (`CRL_APB`, `0xFF5E_0000`):

```toml
# zynqmp_gem3.toml
name = "ZynqMP_GEM3"

[[checks]]
name = "GEM3 reference clock active (CRL_APB.GEM3_REF_CTRL)"
addr = 0xFF5E_005C
mask = 0x0200_0000
expected = 0x0200_0000

[[checks]]
name = "GEM3 reset deasserted (CRL_APB.RST_LPD_IOU0)"
addr = 0xFF5E_0238
mask = 0x8
expected = 0x0
```

```sh
sudo mmreg --validate zynqmp_gem3.toml read 0xFF0E_0000
```

A failing check stops with `[E_VALIDATION_HOOK_FAILED]`, naming the check, its address, the value
read, the mask and the expected value; `0xFF0E_0000` is not touched.

**Format**: a top-level `name` and one or more `[[checks]]` tables with `name`, `addr`, `mask` and
`expected`. Numbers are TOML integers or strings (`"0xFF5E_005C"`, `"0d42"`, otherwise decimal).
Unknown keys, a missing key or no checks are errors (`[E_VALIDATION_PROFILE_PARSE]`). The check
reads themselves skip stages 1 and 2: they target always-on control registers.

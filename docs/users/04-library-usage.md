# 4. Library Usage

The `mmreg` crate provides three levels of API:
1. **[`Device`]** (recommended): a mapped peripheral register window with `&self` 8/16/32/64-bit
   access, `update_u32` read-modify-write, reentrant sequence locking (`dev.lock()`), and UIO
   interrupt control (`irq_wait`, `irq_unmask`). `Device` is `Send + Sync`.
2. **[`Interface`] / [`Register`] / [`SubRegister`]**: a named-register and bitfield layer built on
   top of `Device`.
3. **Single-register helpers** (`read_register_at`, `write_register_at`) for one-off 32-bit
   accesses.

Every fallible call returns `Result<_, MmregError>`, and the library never writes to `stderr`
(non-fatal pre-flight warnings are returned via `dev.warnings()` / `iface.warnings()`).

---

## 1. `Device`: Opening by UIO Name, Path, or Address

```rust
use mmreg::{uio, Device, MmregError};

fn main() -> Result<(), MmregError> {
    // Resolve a UIO name (/sys/class/uio/uio*/name) to its /dev/uioN path
    let path = uio::find("axi_gpio")?;
    println!("Found UIO device at {path}");

    // Open region 0 of a UIO device by name (validated and mapped once at open)
    let dev = Device::open_uio("axi_gpio")?;

    // Or configure with DeviceBuilder: read-only mode, non-zero region, safety profile, --force
    let ro_dev = Device::uio("axi_gpio")
        .region(0)
        .read_only()
        .open()?;

    // Read and write 8-, 16-, 32-, and 64-bit registers at byte offsets
    let status = ro_dev.read_u32(0x04)?;
    dev.write_u32(0x00, status | 1)?;

    // Atomic read-modify-write of a 32-bit register under one lock:
    // replaces bits in `mask` with `value & mask` and returns the new 32-bit register value
    let new_ctrl = dev.update_u32(0x00, 0x0000_00F0, 0x5 << 4)?;
    println!("Updated CTRL = 0x{new_ctrl:08X}");

    Ok(())
}
```

> [!NOTE]
> `read_u64` / `write_u64` are one instruction on 64-bit CPUs. On 32-bit CPUs (ARMv7, 32-bit
> RISC-V) the access may be split into two 32-bit bus accesses, so another bus master can observe
> half of a write. If the IP needs a 64-bit register accessed atomically, follow its latching rule
> (for example, two `u32` accesses in the documented order inside one `dev.lock()` sequence).

- **Opening methods:**
  - `Device::open_uio(name)` / `Device::uio(name)`: looks up `name` in `/sys/class/uio/uio*/name`
    (trimmed) and reads the region's physical `addr` and `size` from `maps/map<N>/{addr,size}`.
    Fails with `[E_UIO_NOT_FOUND]` (listing available names) if missing, or `[E_UIO_AMBIGUOUS]` if
    multiple UIO devices share `name`.
  - `Device::open_uio_path("/dev/uio0")` / `Device::uio_path(path)`: opens a specific `/dev/uioN`
    node directly.
  - `Device::open_addr(base, size)` / `Device::addr(base, size)`: uses a covering UIO region when
    present, or `/dev/mem` when built with the `debug` feature (`[E_DEBUG_DISABLED]` otherwise).
- **Read-only mode (`.read_only()`):** opens with `O_RDONLY` and maps with `PROT_READ`. Any call to
  `write_*`, `update_u32`, or `irq_unmask` fails with `[E_READ_ONLY]`.
- **Single-check open:** blueprint, hardware pre-flight, and attached [`SafetyProfile`] checks run
  once during `open()`, and the mapping and lock descriptor are kept open until the `Device` is
  dropped, making tight register loops fast and allocation-free.

---

## 2. Concurrency, Thread Safety, and Sequence Locks (`dev.lock()`)

`Device` is `Send + Sync` and can be shared across threads (for example in an `Arc<Device>`). Every
`read_*`, `write_*`, and `update_u32` call automatically acquires the device's lock:
- **Across processes:** exclusive `flock(2)` on `/dev/uioN` (or on the physical page lock file for
  `/dev/mem`). C programs that `flock(fd, LOCK_EX)` their own `/dev/uioN` descriptor participate in
  the exact same lock protocol.
- **Within a process:** all `Device` handles referring to the same `(st_dev, st_ino)` share one open
  lock descriptor in an internal registry and synchronize threads with a reentrant mutex, preventing
  both intra-process `flock` self-deadlock and multi-thread races.

To make a **multi-register sequence** atomic across both threads and processes, hold the reentrant
[`LockGuard`] returned by `dev.lock()`. On a `/dev/mem` window (`debug` builds) spanning several
pages, the guard takes every page's lock in ascending address order, so the whole window is
excluded for the sequence; the page lock files stay open only while the guard is held. Such a
window may cover at most `Device::MAX_SEQUENCE_PAGES` (64) pages for a sequence guard
(`[E_LOCK_RANGE]` otherwise): open a smaller `Device` covering just the sequence's registers.

```rust
use mmreg::{Device, MmregError};

fn transfer_word(dev: &Device, payload: u32) -> Result<u32, MmregError> {
    // Holds the lock across all nested reads/writes on this thread until `_guard` drops.
    let _guard = dev.lock()?;
    dev.write_u32(0x10, payload)?;
    dev.update_u32(0x00, 0x1, 0x1)?; // start transfer
    dev.read_u32(0x14)               // read result while still holding the lock
}
```

---

## 3. UIO Interrupts (`irq_wait` and `irq_unmask`)

For UIO devices wired to an interrupt line (such as `uio_pdrv_genirq`), `Device` provides
timeout-aware interrupt waiting (`poll(POLLIN)` + 4-byte `read`) and unmasking (4-byte `write` of
`1u32`):

```rust
use mmreg::{Device, MmregError};
use std::time::Duration;

fn handle_next_irq(dev: &Device) -> Result<(), MmregError> {
    // 1. Ensure the interrupt line is unmasked in the UIO driver
    dev.irq_unmask()?;

    // 2. Wait up to 500 ms for an interrupt
    if let Some(total_irqs) = dev.irq_wait(Duration::from_millis(500))? {
        println!("Interrupt arrived (cumulative kernel count = {total_irqs})");

        // 3. IMPORTANT (level-triggered interrupts): clear the interrupt cause in the
        //    device's own status register FIRST, before unmasking the kernel IRQ line!
        dev.write_u32(0x0C, 0x0000_0001)?; // e.g. write-1-to-clear (W1C) IRQ status bit

        // 4. Re-enable the interrupt line for the next event
        dev.irq_unmask()?;
    }
    Ok(())
}
```

> [!IMPORTANT]
> `uio_pdrv_genirq` masks the interrupt line in the interrupt controller as soon as the interrupt
> fires. For **level-triggered** interrupt sources (the norm on AXI/FPGA fabrics), you **must**
> clear the interrupt source inside the peripheral's registers before calling `dev.irq_unmask()`;
> otherwise the still-asserted interrupt line will re-fire immediately upon unmasking.

---

## 4. Generated-Code Guide (Rust Constants & C Headers)

In production embedded codebases, register offsets, bitmasks, and shifts are typically generated
from a hardware description (SystemRDL, IP-XACT, PeakRDL, or CSV/JSON) as zero-overhead Rust
constants or C `#define`s and paired with `Device`.

### Generated Constants Pattern

```rust
// Generated constants for peripheral `my_timer`
pub mod regs {
    pub const CTRL: u32 = 0x00;
    pub const CTRL_EN_MASK: u32 = 0x0000_0001;
    pub const CTRL_EN_SHIFT: u32 = 0;
    pub const CTRL_PRESCALE_MASK: u32 = 0x0000_00F0;
    pub const CTRL_PRESCALE_SHIFT: u32 = 4;

    pub const IRQ_STATUS: u32 = 0x08;       // W1C register
    pub const IRQ_STATUS_DONE_MASK: u32 = 0x0000_0001;
    pub const IRQ_STATUS_ERR_MASK: u32 = 0x0000_0002;
}

#[inline]
pub const fn field_get(reg_val: u32, mask: u32, shift: u32) -> u32 {
    (reg_val & mask) >> shift
}

#[inline]
pub const fn field_prep(field_val: u32, mask: u32, shift: u32) -> u32 {
    (field_val << shift) & mask
}
```

### Access-Type Rules for Code Generators

When a register generator emits helper functions or macros to write individual bitfields on top of
`Device`, it **must** choose the write strategy according to the field's hardware access type:

| Field Access Type | Hardware Semantics | Required Generated Writer Pattern |
| :--- | :--- | :--- |
| **`rw`** (Read-Write) in a pure-`rw` register | Retains written value | **Locked RMW:** `dev.update_u32(OFFSET, FIELD_MASK, (val << SHIFT) & FIELD_MASK)` |
| **`wo`** (Write-Only) | Reading returns `0` or bus error | **Direct write (no read):** `dev.write_u32(OFFSET, (val << SHIFT) & FIELD_MASK)` |
| **`w1c` / `rw1c`** (Write-1-to-Clear) | Writing `1` clears the bit; writing `0` has no effect | **Direct write (never RMW!):** `dev.write_u32(OFFSET, (val << SHIFT) & FIELD_MASK)` |
| **`w1p`** (Write-1-to-Pulse / self-clearing trigger) | Writing `1` triggers a one-cycle pulse; writing `0` is a no-op | **Direct write (never RMW!):** `dev.write_u32(OFFSET, (val << SHIFT) & FIELD_MASK)` |
| **`rw`** field sharing a register with **`w1c`** fields | Reading returns `1` on pending `w1c` bits | **Masked RMW:** clear both `FIELD_MASK` and all `W1C_MASK` bits so pending `w1c` bits are written back as `0`: `dev.update_u32(OFFSET, FIELD_MASK \| REG_W1C_MASK, (val << SHIFT) & FIELD_MASK)` |

> [!CAUTION]
> Never emit a plain read-modify-write (`update_u32(OFFSET, FIELD_MASK, ...)`) to clear a `w1c`
> interrupt status bit or to modify an `rw` field in a register that also contains `w1c` bits:
> reading the register and writing back the unmodified bits writes `1` to every other asserted `w1c`
> bit in that register, silently acknowledging and losing concurrent hardware events.

---

## 5. `Interface`, `Register` and `SubRegister`

When you want named registers and bitfields in Rust without code generation, use [`Interface`]
(which is layered directly on top of [`Device`]):

```rust
use mmreg::{Interface, MmregError, Register, SubRegister};

fn main() -> Result<(), MmregError> {
    let registers = vec![
        Register::new(
            "ctrl",
            0x00,
            vec![
                SubRegister::new("enable", 0, 0), // bit 0
                SubRegister::new("reset", 1, 1),  // bit 1
                SubRegister::new("mode", 7, 4),   // bits [7:4]
            ],
        ),
        Register::new(
            "status",
            0x04,
            vec![
                SubRegister::new("ready", 0, 0),
                SubRegister::new("error", 3, 1),
            ],
        ),
    ];

    // 0x1000-byte window at 0xA000_0000
    let mut axi_dev = Interface::new("axi_controller", 0xA000_0000, 0x1000, registers);

    // Optional: validate and map once for a batch of accesses.
    axi_dev.map()?;

    let raw_status = axi_dev.read_register("status")?;
    println!("Raw status: 0x{:08X}", raw_status);

    // Decode bitfields from the value just read, without another bus access.
    let is_ready = axi_dev.parse_subregister("status", "ready")?;
    let err_code = axi_dev.parse_subregister("status", "error")?;
    println!("Ready: {}, Error code: {}", is_ready, err_code);

    // Read-modify-write of one bitfield, atomic against other mmreg users.
    axi_dev.write_subregister("ctrl", "mode", 0x5)?;
    axi_dev.write_subregister("ctrl", "enable", 1)?;

    // Also happens when `axi_dev` is dropped.
    axi_dev.unmap();
    Ok(())
}
```

---

## 6. Selecting the Backend (`MapperType`) and Safety Profiles

Both [`Device::addr`] and [`Interface`] accept `.with_mapper_type(...)`, `.with_safety_profile(...)`,
`.with_force(...)`, and `.with_custom_mapper(...)`.

For off-target unit tests, `Device::uio(name)` / `Device::uio_path(path)` with
`.with_custom_mapper(mock)` and `.with_force(true)` open even when no such UIO device exists: the
window is then one page at address 0. Without `.with_force(true)` the device must exist.

```rust
use mmreg::{Device, MapperType, MmregError, SafetyProfile};

fn main() -> Result<(), MmregError> {
    let profile = SafetyProfile::parse(
        r#"{
            "name": "ZynqMP_GEM3",
            "checks": [
                {
                    "name": "GEM3 reference clock active (CRL_APB.GEM3_REF_CTRL)",
                    "addr": "0xFF5E_005C",
                    "mask": "0x0200_0000",
                    "expected": "0x0200_0000"
                }
            ]
        }"#,
    )?;

    let dev = Device::addr(0xFF0E_0000, 0x1000)
        .with_mapper_type(MapperType::Auto)
        .with_safety_profile(profile)
        .open()?;

    println!("GEM3 network_control: 0x{:08X}", dev.read_u32(0x00)?);
    Ok(())
}
```

---

## 7. Single-Register Helpers and Errors (`MmregError`)

For one-off 32-bit accesses by physical address:

```rust
use mmreg::{MmregError, read_register_at, write_register_at};

fn main() -> Result<(), MmregError> {
    let status = read_register_at(0xA000_0000, false)?;
    write_register_at(0xA000_0004, status | 1, false)?;
    Ok(())
}
```

[`MmregError`](../../src/error.rs) is an enum with one variant per failure. `err.code()` returns the
stable code (e.g. `"E_BLUEPRINT_UNMAPPED"`), and `Display` prints `[CODE]`, the failing resource or
path, and a `Remediation:` hint. All codes are listed in [Troubleshooting](06-troubleshooting.md).

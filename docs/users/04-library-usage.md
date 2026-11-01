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

**Runnable examples.** `just test` runs the mock-device examples and compiles the hardware
examples, so they stay in step with the API:

| | Mock device (runs anywhere) | UIO interrupt loop (hardware) |
| :--- | :--- | :--- |
| Rust | [`examples/mock_quickstart.rs`](../../examples/mock_quickstart.rs): `cargo run --example mock_quickstart` | [`examples/uio_irq.rs`](../../examples/uio_irq.rs): `cargo run --example uio_irq -- <uio name>` |
| C | [`bindings/c/examples/quickstart.c`](../../bindings/c/examples/quickstart.c) | [`bindings/c/examples/uio_irq.c`](../../bindings/c/examples/uio_irq.c) |
| Python | [`bindings/python/examples/quickstart.py`](../../bindings/python/examples/quickstart.py) | [`bindings/python/examples/uio_irq.py`](../../bindings/python/examples/uio_irq.py) |

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

const IRQ_STATUS: u32 = 0x0C;
const IRQ_STATUS_W1C_MASK: u32 = 0x0000_0003; // DONE (bit 0) and ERR (bit 1) are write-1-to-clear
const IRQ_STATUS_DONE: u32 = 0x0000_0001;

fn handle_next_irq(dev: &Device) -> Result<(), MmregError> {
    // 1. Ensure the interrupt line is unmasked in the UIO driver
    dev.irq_unmask()?;

    // 2. Wait up to 500 ms for an interrupt
    if let Some(total_irqs) = dev.irq_wait(Duration::from_millis(500))? {
        println!("Interrupt arrived (cumulative kernel count = {total_irqs})");

        // 3. IMPORTANT (level-triggered interrupts): clear the interrupt cause in the
        //    device's own status register FIRST, before unmasking the kernel IRQ line!
        //    Masked write: clears DONE only, keeps any rw bits, leaves a pending ERR alone.
        dev.update_u32(IRQ_STATUS, IRQ_STATUS_W1C_MASK, IRQ_STATUS_DONE)?;

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
    pub const IRQ_STATUS_W1C_MASK: u32 = 0x0000_0003; // all W1C bits, for masked writes
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

A bus write always carries the whole register: `write_u32(OFFSET, 1 << 8)` writes `0` to the other
31 bits. When a register generator emits helper functions or macros to write individual bitfields
on top of `Device`, every field writer is therefore a **masked read-modify-write** under the device
lock, whatever the field's access type:

```rust
// REG_W1C_MASK: every write-1-to-clear and write-1-to-pulse bit of the register (0 if none).
dev.update_u32(OFFSET, FIELD_MASK | REG_W1C_MASK, (val << SHIFT) & FIELD_MASK)?;
```

| Field Access Type | Hardware Semantics | What the masked write does |
| :--- | :--- | :--- |
| **`rw`** (Read-Write) | Retains written value | Writes the field; other `rw` and reserved bits keep their value |
| **`w1c` / `rw1c`** (Write-1-to-Clear) | Reads `1` while pending; writing `1` clears, `0` has no effect | Writes `1` to this bit only; other `w1c` bits are written `0` and stay pending; `rw` bits are kept |
| **`w1p`** (Write-1-to-Pulse / self-clearing trigger) | Writing `1` triggers a pulse; writing `0` is a no-op | Pulses this bit only; `rw` configuration in the same register is kept |
| **`wo`** (Write-Only register) | Reading returns `0` or bus error | Cannot be read, so cannot be read-modify-written: write the **whole register** with every field's value (`dev.write_u32(OFFSET, value)`) |

Clearing `T0INT` (bit 8, `w1c`) in an AXI Timer `TCSR0` with a direct `write_u32(TCSR0, 1 << 8)`
would also write `ENT0 = 0` and stop the timer; `update_u32(TCSR0, TCSR0_W1C_MASK, 1 << 8)` clears
only the interrupt.

> [!CAUTION]
> Put **every** `w1c` / `w1p` bit of the register in the mask, not just the field being written:
> `update_u32(OFFSET, FIELD_MASK, ...)` reads pending `w1c` bits as `1` and writes them back,
> silently acknowledging and losing concurrent hardware events. Registers with read side effects
> (read-to-clear status, FIFO data) must not be read-modify-written: write them as a whole register.

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

> [!WARNING]
> `SubRegister` has no access type: `write_subregister` masks only the field being written, so in a
> register that also holds write-1-to-clear bits it writes pending ones back as `1` and clears them.
> For such registers use `Device::update_u32` with all of the register's `w1c` bits in the mask
> (see [Access-Type Rules](#access-type-rules-for-code-generators)).

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

---

## 8. C ABI (`bindings/c`: `mmreg.h`, `libmmreg.a` / `libmmreg.so`)

The `mmreg-capi` crate in `bindings/c` builds a panic-free C ABI (`bindings/c/include/mmreg.h`
and `bindings/c/mmreg.pc`) that replaces project-local C UIO helper libraries while sharing the exact same lock
protocol and pre-flight validation.

### Building & Linking a C Program

```sh
# Build static (`libmmreg.a`) and shared (`libmmreg.so`) libraries
cargo build --release -p mmreg-capi

# Link statically without Cargo at link time (or via `pkg-config --cflags --libs --static mmreg`).
# The system libraries are those of glibc targets; musl targets need only -lpthread -lm -lc.
cc -O2 -Ibindings/c/include my_daemon.c target/release/libmmreg.a -lgcc_s -lutil -lrt -lpthread -lm -ldl -lc -o my_daemon
```

`mmreg_irq_wait(dev, MMREG_WAIT_FOREVER, &count)` waits without a timeout. Errors return `-1` (or
`NULL`) and set a per-thread code and message; the codes specific to C and Python are listed in
[Troubleshooting](06-troubleshooting.md#c--python-bindings).

### C Example

```c
#include "mmreg.h"
#include <stdio.h>

#define CTRL_REG        0x00u
#define CTRL_EN_MASK    0x00000001u
#define CTRL_EN_SHIFT   0u
#define STATUS_REG      0x04u
#define SCRATCH_REG     0x08u
#define IRQ_STATUS_REG  0x0Cu
#define IRQ_STATUS_W1C  0x00000003u /* DONE (bit 0) and ERR (bit 1) are write-1-to-clear */
#define IRQ_STATUS_DONE 0x00000001u

int main(void) {
    mmreg_dev *dev = mmreg_open_uio("axi_gpio", 0);
    if (!dev) {
        fprintf(stderr, "open failed [%s]: %s\n", mmreg_last_error_code(), mmreg_last_error());
        return 1;
    }

    /* Atomic read-modify-write under a single lock */
    uint32_t ctrl = 0;
    if (mmreg_update32(dev, CTRL_REG, CTRL_EN_MASK, MMREG_PREP(1u, CTRL_EN_MASK, CTRL_EN_SHIFT), &ctrl) != 0) {
        fprintf(stderr, "update failed: %s\n", mmreg_last_error());
        mmreg_close(dev);
        return 1;
    }
    printf("EN bit = %u\n", MMREG_FIELD(ctrl, CTRL_EN_MASK, CTRL_EN_SHIFT));

    /* Hold a reentrant lock across a multi-register sequence */
    if (mmreg_lock(dev) == 0) {
        uint32_t status = 0;
        mmreg_rd32(dev, STATUS_REG, &status);
        mmreg_wr32(dev, SCRATCH_REG, status);
        mmreg_unlock(dev);
    }

    /* Wait up to 500 ms for a UIO interrupt (1 = fired, 0 = timeout, -1 = error) */
    uint32_t irq_count = 0;
    int rc = mmreg_irq_wait(dev, 500, &irq_count);
    if (rc == 1) {
        /* Clear the cause in the device first: masked write, so only DONE is cleared */
        mmreg_update32(dev, IRQ_STATUS_REG, IRQ_STATUS_W1C, IRQ_STATUS_DONE, NULL);
        mmreg_irq_unmask(dev);               /* then unmask kernel IRQ line */
    }

    mmreg_close(dev);
    return 0;
}
```

---

## 9. Python Binding (`bindings/python/mmreg`)

The pure-Python package in `bindings/python/mmreg` uses standard-library `ctypes` over
`libmmreg.so`, so target boards need no compiler, PyO3, or wheel build.

```python
from mmreg import Device, find_uio

print("UIO node:", find_uio("axi_gpio"))

with Device.open_uio("axi_gpio") as d:
    # Offset reads, writes, and atomic update_u32
    status = d.read_u32(0x04)
    d.update_u32(0x00, 0x0000_00F0, 0x5 << 4)

    # Reentrant sequence lock context manager
    with d.lock():
        d.write_u32(0x10, 0x1234_5678)
        d.write_u32(0x14, status)

    # Inline register and bitfield definitions for interactive experiments without a block file
    d.define("CTRL", offset=0x08, fields={"EN": "0", "MODE": "7:4"})
    d.CTRL.EN = True         # 1-bit fields also accept bool
    d.CTRL.MODE = 5          # a value wider than the field raises [E_INVALID_ARG]
    print(f"CTRL = 0x{int(d.CTRL):08X}, MODE = {int(d.CTRL.MODE)}")

    # Every field write is a masked read-modify-write; write-1-to-clear bits are written as 0
    # unless they are the field being written
    d.define("ISR", offset=0x0C, fields={"DONE": "0", "ERR": "1", "MODE": "7:4"}, w1c={"DONE", "ERR"})
    if d.ISR.DONE == 1:
        d.ISR.DONE = 1       # clears DONE only; MODE is kept and a pending ERR is left alone

    # Wait for an interrupt (None waits without a timeout)
    if d.irq_wait(500) is not None:
        d.irq_unmask()
```

Hardware is read only when asked for: `int(d.CTRL)`, `d.CTRL.read()`, or `d.CTRL.EN == 1`.
`repr()` (and so a REPL or debugger showing `d.ISR`) prints the definition without reading the
register, so read-to-clear registers are not cleared by inspecting them. Register and field names
that would shadow existing attributes (`size`, `lock`, `read`, ...) are rejected. Values outside
the access width (`write_u8(0, 0x100)`, negative offsets or timeouts) raise `[E_INVALID_ARG]`
instead of being truncated; so do floats (`irq_wait(0.5)`) and `bool` anywhere except a 1-bit
field. A `Device` owns its C handle, so `copy.copy`, `copy.deepcopy` and `pickle` raise
`E_NOT_COPYABLE`: share the object instead. When it first loads `libmmreg`, the package checks that
it has the same major version (`mmreg_version()`).

Every exception the package raises is an `mmreg.MmregError` with a stable `.code`, and it survives
pickling, so `multiprocessing` workers return it intact. A missing register or field is also an
`AttributeError` (`hasattr(d, "CTRL")` works), a library that cannot be loaded (`E_LIB_LOAD`) also
an `OSError`, and `E_NOT_COPYABLE` also a `TypeError`. Field bit specifications are `"7:4"`,
`"[7:4]"`, `"0"`, `0` or `(7, 4)` (decimal or `0x` hex); anything else, or overlapping fields,
raises `E_SUBREG_RANGE`. Installing the package: [Installation](02-installation.md#python-package).

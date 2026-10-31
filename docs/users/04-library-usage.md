# 4. Library Usage

The `mmreg` crate has two levels of API:
1. **Single-register helpers** (`read_register_at`, `write_register_at`) for one-off accesses.
2. **Register banks** (`Interface`, `Register`, `SubRegister`) for peripherals with named registers
   and bitfields.

Every fallible call returns `Result<_, MmregError>`.

---

## 1. Single-Register Access

```rust
use mmreg::{MmregError, read_register_at, write_register_at};

fn main() -> Result<(), MmregError> {
    // Validated access (force = false)
    let status = read_register_at(0xA000_0000, false)?;
    println!("Status register: 0x{:08X}", status);
    write_register_at(0xA000_0004, 0x0000_0001, false)?;
    Ok(())
}
```

Each call validates, maps, locks, accesses and unmaps. For several accesses to one peripheral, use an
`Interface` and map it once.

---

## 2. `Interface`, `Register` and `SubRegister`

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

- **Layout rules** (checked when mapping): the base address and every register offset are multiples
  of 4, and `offset + 4 <= size` (`[E_UNALIGNED_ACCESS]`, `[E_OUT_OF_BOUNDS]`). A bitfield needs
  `31 >= msb >= lsb` (`[E_SUBREG_RANGE]`).
- **Mapping**: without `map()`, each access maps and unmaps around itself, repeating the checks.
  After `map()`, accesses reuse the mapping until `unmap()` or drop; `map()` on a mapped
  `Interface` does nothing.
- **Locking**: each access, and each `write_subregister` read-modify-write, holds an inter-process
  lock: `flock` on `/dev/uioN` for UIO devices, a lock file for the register's physical page with
  `/dev/mem`. Locks are not held between calls, so a sequence of calls is not atomic as a whole.
- **Unknown names** fail before any hardware access (`[E_REG_NOT_FOUND]`, `[E_SUBREG_NOT_FOUND]`,
  with the available names in the message).

---

## 3. Selecting the Backend (`MapperType`)

The default `MapperType::Auto` uses the UIO region covering the window when there is one and
`/dev/mem` otherwise. To choose explicitly:

```rust
use mmreg::{Interface, MapperType, Register};

// A specific UIO device and region; its physical range is read from
// /sys/class/uio/uio0/maps/map0/{addr,size}, and the window must lie inside it.
let uio_iface = Interface::new("dma", 0xA000_0000, 0x1000, vec![Register::new("ctrl", 0, vec![])])
    .with_mapper_type(MapperType::Uio {
        device_path: "/dev/uio0".to_string(),
        region_index: 0,
    });

// Always /dev/mem: needs the `debug` feature, [E_DEBUG_DISABLED] otherwise
let devmem_iface = Interface::new("gpio", 0x4000_0000, 0x1000, vec![Register::new("data", 0, vec![])])
    .with_mapper_type(MapperType::DevMem);
```

`with_custom_mapper` accepts any `MemoryMapper` implementation (for tests or special drivers); see
[Memory Mapper Backends](../developers/02-memory-mappers.md).

---

## 4. Safety Profiles

A `SafetyProfile` attached to an `Interface` runs before every mapping, including with
`with_force(true)`:

```rust
use mmreg::{Interface, MmregError, Register, SafetyProfile};

fn main() -> Result<(), MmregError> {
    // Needs the `profile` feature (default). SafetyProfile::from_file(path) reads a file.
    let profile = SafetyProfile::parse(
        r#"
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
        "#,
    )?;

    let mut gem3 = Interface::new(
        "zynqmp_gem3",
        0xFF0E_0000,
        0x1000,
        vec![Register::new("network_control", 0x00, vec![])],
    )
    .with_safety_profile(profile);

    let net_ctrl = gem3.read_register("network_control")?;
    println!("GEM3 network_control: 0x{:08X}", net_ctrl);
    Ok(())
}
```

Without the `profile` feature, build the profile in code:

```rust
use mmreg::{SafetyProfile, ValidationCheck};

let profile = SafetyProfile {
    name: "ZynqMP_GEM3".into(),
    checks: vec![ValidationCheck {
        name: "GEM3 reset deasserted".into(),
        addr: 0xFF5E_0238,
        mask: 0x8,
        expected: 0x0,
    }],
};
```

---

## 5. Errors (`MmregError`)

[`MmregError`](../../src/error.rs) is an enum with one variant per failure. `err.code()` returns the
stable code (e.g. `"E_BLUEPRINT_UNMAPPED"`), and `Display` prints `[CODE]`, the failing resource or
path, and a `Remediation:` hint. The codes are listed in [Troubleshooting](06-troubleshooting.md).

# 7. Block Files (`mmreg-block/1`)

> [!NOTE]
> **Draft specification, not implemented yet** (planned for 1.3, see [TODO](../../TODO.md)).
> Comments on the format are welcome until it ships; after that, `/1` only changes additively.

A block file describes one IP block: its registers, bitfields, enumerations, and how each field
behaves on read and write. The **mmreg Python package** reads block files to give register access
by name for debugging, bring-up and experiments:

```python
import mmreg

with mmreg.open("gpio0") as d:     # UIO name → Device Tree compatible → block file
    print(d.CTRL)                  # decoded fields
    d.CTRL.MODE = "OPEN_DRAIN"     # enum names accepted
    d.IRQ_STATUS.CHANGE = 0x01     # write-1-to-clear: writes only this bit
```

**Block files are for interactive use only.** Production programs never read them: they use the C
header or Rust code that the register generator produces from the same source, with
`libmmreg` / the mmreg crate for access. The Rust core, the C library and the CLI do not parse
block files; the CLI accesses registers by address (`mmreg read <address>`).

**Easy to write by hand.** Keys are a small subset of SystemRDL's software properties, spelled as in
the SystemRDL-style JSON that register generators commonly use (`at`, `bits`, `sw`, `onwrite`,
`desc`, ...). A field with default access needs only `bits`. For a quick experiment without a
block file, define registers inline in Python (`d.define(...)`, see [Python API](#python-api)).

The format belongs to `mmreg` and is tied to no register generator, description language or SoC.
A generator emits it as one more output (dropping hardware-only keys such as ports and counters),
an importer converts CMSIS-SVD (later IP-XACT), or a person writes it.

---

## Example

`acme,gpio-1.0.json`:

```json
{
  "format": "mmreg-block/1.0",
  "compatible": "acme,gpio-1.0",
  "enum": {
    "mode_e": {
      "PUSH_PULL":  { "value": 0, "desc": "Drive high and low" },
      "OPEN_DRAIN": { "value": 1, "desc": "Drive low only" }
    }
  },
  "gpio": {
    "desc": "8-pin GPIO controller",
    "VERSION": { "at": "0x0", "sw": "r",
      "MINOR": { "bits": "15:0" },
      "MAJOR": { "bits": "31:16" }
    },
    "CTRL": { "at": "0x8", "desc": "Control",
      "EN":      { "bits": "0", "desc": "Block enable" },
      "MODE":    { "bits": "2:1", "encode": "mode_e" },
      "RESTART": { "bits": "3", "sw": "w", "singlepulse": true, "desc": "Write 1: restart" }
    },
    "IN": { "at": "0x14", "sw": "r", "hw": "w" },
    "IRQ_STATUS": { "at": "0x18",
      "CHANGE": { "bits": "7:0", "onwrite": "woclr", "hw": "w", "desc": "Pin changed; write 1 to clear" }
    },
    "DEBOUNCE": { "at": "0x24", "dim": 8,
      "CYCLES": { "bits": "7:0", "reset": 4 }
    }
  }
}
```

```python
d.VERSION.MAJOR          # read-only field
d.CTRL.RESTART = 1       # pulse: written alone, never written back
d.IN                     # register without fields: one value
d.DEBOUNCE[3].CYCLES = 10
```

---

## Structure

Names are object keys, as in SystemRDL-style JSON. Inside every object, lowercase keys from the
tables below are properties; every other key is a child (a register, register group or field), and
its value must be an object. A key starting with `x-` is ignored.

| Object | Recognized by | Children |
|---|---|---|
| file | top level | exactly one block, keyed by its name |
| block | the one non-property key of the file | registers and register groups |
| register | has `at`; its children (if any) have `bits` | fields |
| register group | has `at`; its children have `at` | registers (offsets relative to the group) |
| field | has `bits` | none |

Child names match `[A-Za-z_][A-Za-z0-9_]*` and must not be a property key. Lookups are
case-insensitive, so names must also be unique ignoring case.

### File

| Key | Required | Meaning |
|---|---|---|
| `format` | yes | `"mmreg-block/1.<minor>"` (see [Versioning](#versioning)) |
| `compatible` | yes | Identity of the IP version: matches the Device Tree `compatible` and the file name |
| `enum` | no | Shared enumerations: `{ name: { VALUE_NAME: { "value": n, "desc": "..." } } }` |

### Block

| Key | Default | Meaning |
|---|---|---|
| `desc` | | Description |
| `name` | | Display name |
| `size` | the UIO region size | Size of the register window in bytes; every register must fit inside it |
| `regwidth` | `32` | Default register width in bits: 8, 16, 32 or 64 |
| `accesswidth` | `regwidth` | Widest single bus access; wider registers are split (see [Wide registers](#wide-registers)) |

### Register and register group

| Key | Default | Meaning |
|---|---|---|
| `at` | required | Byte offset from the block (or group) start, aligned to `regwidth / 8` |
| `desc`, `name` | | Description, display name |
| `regwidth`, `accesswidth` | block's | As for the block |
| `dim` | | Array of `dim` copies, addressed `NAME[i]` |
| `stride` | size of one copy | Bytes between copies |

A register may carry the field keys below except `bits` (`sw`, `onwrite`, `onread`, `hw`, ...) as
defaults for its fields, so `"VERSION": { "at": 0, "sw": "r", ... }` makes every field read-only. A
register without fields is one field covering all its bits.

### Field

| Key | Default | Meaning |
|---|---|---|
| `bits` | required | `"msb:lsb"` or `"n"` (`"7:0"`, `"31"`), inside `regwidth` |
| `sw` | `"rw"` | Software access: `rw`, `r`, `w`, `rw1`, `w1` (write once), `na` (none) |
| `onwrite` | | Write side effect: `woclr`, `woset`, `wot`, `wzc`, `wzs`, `wzt`, `wclr`, `wset` |
| `onread` | | Read side effect: `rclr`, `rset` |
| `singlepulse` | `false` | With `sw: "w"`: writing 1 makes a one-cycle pulse; the field reads 0 |
| `hw` | `"r"` | Hardware access; `w` / `rw` means hardware changes the value (status, counters): shown as volatile by `watch` and `dump` |
| `reset` | `0` | Reset value; `dump(diff_reset=True)` compares against it |
| `encode` | | Name of an enumeration in `enum` |
| `desc`, `name` | | Description, display name |

Fields of one register must not overlap. Bits covered by no field are **reserved**: read as found,
written back unchanged by read-modify-write, never written alone.

### Numbers

JSON integers (`4096`) or strings: `"0x1000"`, `"0b1010"`, `"4096"`, with `_` allowed between
digits (`"0xA001_0000"`).

---

## Access Behaviour

| Field keys | Read | Write | In read-modify-write of another field |
|---|---|---|---|
| `sw: "rw"` (default), `"rw1"` | value | value | written back as read |
| `sw: "r"` | value | refused | written as 0 |
| `sw: "w"`, `"w1"` | undefined (shown as `-`) | value | written as 0 |
| `sw: "w"`, `singlepulse` | 0 | 1 starts a pulse | written as 0 |
| `sw: "na"` | refused | refused | written back as read |
| `onwrite: "woclr"` | value | 1 clears the bit, 0 no effect | written as 0 |
| `onwrite: "woset"` | value | 1 sets the bit, 0 no effect | written as 0 |
| `onwrite: "wot"` | value | 1 toggles the bit, 0 no effect | written as 0 |
| `onwrite: "wzc"` | value | 0 clears the bit, 1 no effect | written as 1 |
| `onwrite: "wzs"` | value | 0 sets the bit, 1 no effect | written as 1 |
| `onwrite: "wzt"` | value | 0 toggles the bit, 1 no effect | written as 1 |
| `onwrite: "wclr"` | value | any write clears | (register never read-modify-written) |
| `onwrite: "wset"` | value | any write sets | (register never read-modify-written) |
| `onread: "rclr"` | value, then cleared | as `sw` | read side effect, see below |
| `onread: "rset"` | value, then set | as `sw` | read side effect, see below |

These are SystemRDL's properties, and CMSIS-SVD's `access`, `modifiedWriteValues` and `readAction`
map onto them one-to-one, so an SVD import loses nothing. Rules the Python layer enforces:

1. **Field writes write only what was asked.** Writing field F of register R:
   - When every other field of R has no effect when written as shown above (`sw: "r"`,
     `sw: "w"`, `woclr`, `woset`, `wot`, `wzc`, `wzs`, `wzt`), F is written **without reading R
     first**.
   - Otherwise R is read and written back as shown above, under the device lock, so `rw` and
     reserved bits keep their value and pending `woclr` bits are not cleared by accident.
   - A register with a `wclr` / `wset` field is never read-modify-written: writing one of its
     fields needs the whole register (`d.R = value`).
2. **No accidental read side effects.** Reading a register with `rclr` / `rset` fields, including
   the read of a read-modify-write, needs `side_effects=True`. `dump()` skips such registers and
   says so.
3. **Refused accesses:** writing an `sw: "r"` field, reading a register whose fields are all
   `sw: "w"`, any access to an `sw: "na"` field. Raw-value writes of a whole register
   (`d.R = value`) are always allowed.

Generators that emit C headers or Rust code should apply the same rules to the field writers they
generate, so production code and debug sessions behave alike.

## Wide Registers

A register whose `regwidth` exceeds `accesswidth` is accessed as `regwidth / accesswidth` bus words:
read lowest address first, written highest address last, under one lock. That is the order in which
hardware usually latches multi-word registers.

---

## How a Name Is Resolved

`mmreg.open(name)` finds the block and the hardware without any address in a file:

```text
1. instance files            an instance called `name`?  → its block and its uio / at
2. /sys/class/uio/*/name     a UIO device called `name`? → uioN
3. uioN/device/of_node/compatible   → e.g. "acme,gpio-1.0", "generic-uio"
4. <block dir>/acme,gpio-1.0.json   first compatible with a block file wins
5. uioN/maps/map0/{addr,size}       → the register window
```

For step 3 the Device Tree node lists an IP-specific `compatible` before `generic-uio`;
`uio_pdrv_genirq` still binds, because it matches any entry against `of_id=generic-uio`:

```dts
gpio0@a0010000 {
    compatible = "acme,gpio-1.0", "generic-uio";
    reg = <0x0 0xa0010000 0x0 0x10000>;
    linux,uio-name = "gpio0";
};
```

Every instance of an IP then works with no per-board file, a new IP version (new `compatible`)
cannot be decoded with a stale block file, and lookup reads a few sysfs files and one block file.

**Search paths**, first match wins:
- block files: `$MMREG_BLOCKS` (`:`-separated directories), `/etc/mmreg/blocks/`,
  `/usr/share/mmreg/blocks/`;
- instance files: `$MMREG_INSTANCES` (`:`-separated files), then every `/etc/mmreg/instances.d/*.json`.

IP recipes install block files into `/usr/share/mmreg/blocks/`; local overrides go into
`/etc/mmreg/blocks/`. Both belong in development images only.

## Instance Files

Optional. Needed for devices whose Device Tree node has no IP-specific `compatible`, for addresses
without UIO, and to attach prerequisite checks. Instances are object keys, as in block files:

```json
{
  "format": "mmreg-instances/1.0",
  "gpio1": { "block": "acme,gpio-1.0", "uio": "gpio1" },
  "debug_uart": { "block": "ns16550a", "at": "0xFF000000",
    "checks": [
      { "name": "UART clock enabled", "addr": "0xFF5E0074", "mask": "0x01000000",
        "expected": "0x01000000" }
    ]
  }
}
```

| Key | Required | Meaning |
|---|---|---|
| `block` | yes | `compatible` of the block file |
| `uio` | one of `uio` / `at` | UIO device name; window from sysfs |
| `uio_map` | no (`0`) | Region index of the UIO device (`maps/mapN`) |
| `at` | one of `uio` / `at` | Physical address; access through `/dev/mem`, which needs a `debug` build of `libmmreg` |
| `checks` | no | Prerequisites run before the block is touched: same keys as a safety profile's `[[checks]]` (`name`, `addr`, `mask`, `expected`) |

An instance in a file takes precedence over a UIO device of the same name. Two files defining one
instance name is an error (`[E_INSTANCE_DUPLICATE]`).

---

## Python API

Planned shape (1.3); the package talks to the hardware through `libmmreg` (1.2), so its locking
and interrupt handling are the same as for C and Rust programs.

```python
import mmreg

d = mmreg.open("gpio0")                    # or mmreg.open(block="acme,gpio-1.0", uio="gpio0")
d.CTRL                                     # register: read, decoded on print
d.CTRL.MODE                                # field value (enum name when it has one)
d.CTRL.MODE = "OPEN_DRAIN"                 # field write, access rules applied
d.CTRL = 0x0000_0001                       # whole-register write
d.DEBOUNCE[3]                              # array element
d.dump(diff_reset=True)                    # every register, skipping read side effects
with d.lock():                             # several accesses under one lock
    ...
d.wait_irq(timeout=1.0)                    # UIO interrupt, None on timeout

scratch = d.define("SCRATCH", at=0x40, fields={"LO": "15:0", "HI": "31:16"})
scratch.HI = 0xBEEF                        # inline definition, no block file needed
```

`python -m mmreg check <file.json>` validates block and instance files without hardware, for
generators' CI.

---

## Versioning

`"format": "mmreg-block/1.<minor>"` and `"mmreg-instances/1.<minor>"`.

- Within `/1`, changes are additive: new optional keys or values raise the minor version. A reader
  rejects a file whose minor version is newer than it knows (`[E_BLOCK_VERSION]`), naming the
  version it supports, instead of misreading it.
- A misspelled property is an error (`[E_BLOCK_PARSE]`): its value is not an object, so it cannot
  be a child, and a typo such as `"onwrit": "woclr"` cannot silently turn a write-1-to-clear field
  into a read-modify-written one. Unknown property values (`"sw": "wr"`) are errors too.
- Keys starting with `x-` are ignored everywhere: producers put their own data there.
- A future `/2` may change meaning; readers will keep reading `/1`.

# 7. Block Files (`mmreg-block/1`)

> [!NOTE]
> **Draft specification, not implemented yet** (planned for 2.2, see [TODO](../../TODO.md)).
> Comments on the format are welcome until it ships; after that, `/1` only changes additively.

A block file describes one IP block: its registers, bitfields, enumerations, and how each field
behaves on read and write. The **mmreg Python package** reads block files to give register access
by name for debugging, bring-up and experiments:

```python
import mmreg

with mmreg.open("dbg_ctrl0") as d:    # UIO name → Device Tree compatible → block file
    print(d.CTRL)                            # decoded fields
    d.CTRL.PIN0_DIR = "OUT"                  # enum names accepted
    d.CTRL.IRQ_PENDING = 1                   # w1c: writes only this bit
```

**Block files are for interactive use only.** Production programs never read them: they use the C
header or Rust code that the register generator produces from the same source, with
`libmmreg` / the mmreg crate for access. The Rust core, the C library and the CLI do not parse
block files; the CLI accesses registers by address (`mmreg read <address>`).

The format belongs to `mmreg` and is tied to no register generator, description language or SoC.
Anything can produce it: a register generator as one more output, an importer (CMSIS-SVD, later
IP-XACT), or a person. For a quick experiment without a block file, define registers inline in
Python (`d.define(...)`, see [Python API](#python-api)).

---

## How a Name Is Resolved

`mmreg.open(name)` finds the block and the hardware without any address in a file:

```text
1. instance files            an instance called `name`?  → its block and its uio / base
2. /sys/class/uio/*/name     a UIO device called `name`? → uioN
3. uioN/device/of_node/compatible   → e.g. "acme,dbg-ctrl-1.0", "generic-uio"
4. <block dir>/acme,dbg-ctrl-1.0.json   first compatible with a block file wins
5. uioN/maps/map0/{addr,size}        → the register window
```

For step 3 the Device Tree node lists an IP-specific `compatible` before `generic-uio`;
`uio_pdrv_genirq` still binds, because it matches any entry against `of_id=generic-uio`:

```dts
dbg_ctrl0@a0030000 {
    compatible = "acme,dbg-ctrl-1.0", "generic-uio";
    reg = <0x0 0xa0030000 0x0 0x10000>;
    linux,uio-name = "dbg_ctrl0";
};
```

Then every instance of an IP works with no per-board file, a new IP version (new `compatible`)
cannot be decoded with a stale block file, and lookup reads a few sysfs files and one block file.
Instance files ([below](#instance-files)) cover the rest: devices whose node has only
`generic-uio`, and addresses without UIO (`/dev/mem`, `debug` builds of `libmmreg`).

**Search paths**, first match wins:
- block files: `$MMREG_BLOCKS` (`:`-separated directories), `/etc/mmreg/blocks/`,
  `/usr/share/mmreg/blocks/`;
- instance files: `$MMREG_INSTANCES` (`:`-separated files), then every `/etc/mmreg/instances.d/*.json`.

IP recipes install block files into `/usr/share/mmreg/blocks/`; local overrides go into
`/etc/mmreg/blocks/`. Both belong in development images only.

---

## Block File

One JSON file per IP version, named `<compatible>.json`.

```json
{
  "format": "mmreg-block/1.0",
  "compatible": "acme,dbg-ctrl-1.0",
  "name": "dbg_ctrl",
  "description": "Debug control block",
  "size": "0x10000",
  "enums": {
    "direction": [
      { "name": "IN",  "value": 0, "description": "Pin is an input" },
      { "name": "OUT", "value": 1, "description": "Pin is driven" }
    ]
  },
  "registers": [
    { "name": "VERSION", "offset": "0x0", "access": "ro", "description": "IP version",
      "fields": [
        { "name": "MINOR", "bits": "15:0" },
        { "name": "MAJOR", "bits": "31:16" }
      ] },
    { "name": "CTRL", "offset": "0x8", "reset": "0x0",
      "fields": [
        { "name": "SOFT_RST",    "bits": "0", "access": "wo", "description": "Write 1: reset the block" },
        { "name": "IRQ_PENDING", "bits": "2", "access": "w1c", "volatile": true },
        { "name": "PIN0_DIR",    "bits": "8", "enum": "direction" }
      ] },
    { "name": "DATA[{n}]", "offset": "0x40", "dim": 32, "dim_increment": 4,
      "description": "Data words" }
  ],
  "x-generator": "any producer data goes into x- keys"
}
```

| Key | Type | Required | Meaning |
|---|---|---|---|
| `format` | string | yes | `"mmreg-block/1.<minor>"` (see [Versioning](#versioning)) |
| `compatible` | string | yes | Identity of the IP version; matches the Device Tree `compatible` and the file name |
| `name` | identifier | yes | Short name, shown by `dump` |
| `description` | string | no | |
| `size` | integer | yes | Size of the register window in bytes; every register must fit inside it |
| `bus_width` | 8, 16, 32, 64 | no (`32`) | Widest single access the bus supports; wider registers are split (see [Wide registers](#wide-registers)) |
| `enums` | object | no | Shared enumerations: name → list of `{ name, value, description }` |
| `registers` | array | yes | At least one |

### Register

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | identifier | required | Unique within the block. With `dim`, contains `{n}` |
| `description` | string | | |
| `offset` | integer | required | Byte offset from the block start, aligned to `width / 8` |
| `width` | 8, 16, 32, 64 | `32` | Register width in bits |
| `access` | access type | `"rw"` | Default access of its fields |
| `reset` | integer | | Reset value; `dump(diff_reset=True)` compares against it |
| `volatile` | bool | `false` | Default `volatile` of its fields |
| `dim` | integer ≥ 1 | | Register array: `dim` copies |
| `dim_increment` | integer | `width / 8` | Byte distance between copies |
| `fields` | array | | Without fields, the register is one field named `VALUE` covering all bits |

Register arrays are addressed `NAME[i]` (`d.DATA[3]`), with `{n}` in `name` marking the index.

### Field

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | identifier | required | Unique within the register |
| `description` | string | | |
| `bits` | string | required | `"msb:lsb"` or `"bit"` (`"7:0"`, `"31"`), inside `width` |
| `access` | access type | register's | See below |
| `volatile` | bool | register's | Hardware changes the value (status, counters). Informational |
| `reset` | integer | | Field reset value, overrides the bits of the register's `reset` |
| `enum` | string | | Name of a shared enumeration |
| `values` | array | | Inline enumeration: `{ name, value, description }` |

Fields of one register must not overlap. Bits covered by no field are **reserved**: read as found,
written back unchanged by read-modify-write, never written alone.

Identifiers match `[A-Za-z_][A-Za-z0-9_]*`. Lookups are case-insensitive, so names must also be
unique ignoring case.

### Numbers

JSON has no hexadecimal literals, so numbers are JSON integers (`4096`) or strings in the CLI
number syntax (`"0x1_0000"`, `"0d4096"`; unprefixed strings are decimal).

---

## Access Types

| `access` | Read | Write | In read-modify-write of another field |
|---|---|---|---|
| `rw` | value | value | written back as read |
| `ro` | value | ignored; writes of this field are refused | written as 0 |
| `wo` | undefined (shown as `-`) | value | written as 0 |
| `w1c` | value | 1 clears the bit, 0 no effect | written as 0 |
| `w1s` | value | 1 sets the bit, 0 no effect | written as 0 |
| `w1t` | value | 1 toggles the bit, 0 no effect | written as 0 |
| `w0c` | value | 0 clears the bit, 1 no effect | written as 1 |
| `w0s` | value | 0 sets the bit, 1 no effect | written as 1 |
| `w0t` | value | 0 toggles the bit, 1 no effect | written as 1 |
| `wc` | value | any write clears | (register never read-modify-written) |
| `ws` | value | any write sets | (register never read-modify-written) |
| `rc` | value, then cleared | value | read side effect, see below |
| `rs` | value, then set | value | read side effect, see below |

The types map one-to-one onto CMSIS-SVD (`access`, `modifiedWriteValues`, `readAction`), so an
SVD import loses nothing. Rules the Python layer enforces:

1. **Field writes write only what was asked.** Writing field F of register R:
   - When every other field of R is "no effect when written as shown" (`ro`, `wo`, `w1*`, `w0*`),
     F is written **without reading R first**.
   - Otherwise R is read and written back with the column above, under the device lock, so `rw`
     and reserved bits keep their value and pending `w1c` bits are not cleared by accident.
   - A register with any `wc` / `ws` field is never read-modify-written: writing one of its fields
     needs the whole register (`d.R = value`).
2. **No accidental read side effects.** Reading a register with `rc` / `rs` fields, including the
   read of a read-modify-write, needs `side_effects=True`. `dump()` skips such registers and says
   so.
3. **Refused accesses:** writing an `ro` field, reading a register whose fields are all `wo`.
   Raw-value writes of a whole register (`d.R = value`) are always allowed.

Generators that emit C headers or Rust code should apply the same rules to the field writers they
generate, so production code and debug sessions behave alike.

## Wide Registers

A register wider than `bus_width` is accessed as `width / bus_width` bus words: read lowest address
first, written highest address last, under one lock. That is the order in which hardware usually
latches multi-word registers.

---

## Instance Files

Optional. Needed for devices whose Device Tree node has no IP-specific `compatible`, for addresses
without UIO, and to attach prerequisite checks.

```json
{
  "format": "mmreg-instances/1.0",
  "instances": [
    { "name": "dbg_ctrl1", "block": "acme,dbg-ctrl-1.0", "uio": "dbg_ctrl1" },
    { "name": "debug_uart", "block": "ns16550a", "base": "0xFF000000",
      "checks": [
        { "name": "UART clock enabled", "addr": "0xFF5E0074", "mask": "0x01000000",
          "expected": "0x01000000" }
      ] }
  ]
}
```

| Key | Type | Required | Meaning |
|---|---|---|---|
| `name` | identifier | yes | Name passed to `mmreg.open`; unique across all instance files |
| `block` | string | yes | `compatible` of the block file |
| `uio` | string | one of `uio` / `base` | UIO device name; window from sysfs |
| `uio_map` | integer | no (`0`) | Region index of the UIO device (`maps/mapN`) |
| `base` | integer | one of `uio` / `base` | Physical address; access through `/dev/mem`, which needs a `debug` build of `libmmreg` |
| `checks` | array | no | Prerequisites run before the block is touched: same keys as a safety profile's `[[checks]]` (`name`, `addr`, `mask`, `expected`) |

An instance in a file takes precedence over a UIO device of the same name. Two files defining one
instance name is an error (`[E_INSTANCE_DUPLICATE]`).

---

## Python API

Planned shape (2.2); the package talks to the hardware through `libmmreg` (2.1), so its locking
and interrupt handling are the same as for C and Rust programs.

```python
import mmreg

d = mmreg.open("dbg_ctrl0")          # or mmreg.open(block="acme,dbg-ctrl-1.0", uio=...)
d.CTRL                                      # register: read, decoded on print
d.CTRL.PIN0_DIR                             # field value (enum name when it has one)
d.CTRL.PIN0_DIR = "OUT"                     # field write, access rules applied
d.CTRL = 0x0000_0100                        # whole-register write
d.DATA[3]                                   # register array element
d.dump(diff_reset=True)                     # every register, skipping read side effects
with d.lock():                              # several accesses under one lock
    ...
d.wait_irq(timeout=1.0)                     # UIO interrupt, None on timeout

scratch = d.define("SCRATCH", offset=0x40, fields={"LO": "15:0", "HI": "31:16"})
scratch.HI = 0xBEEF                         # inline definition, no block file needed
```

`python -m mmreg check <file.json>` validates block and instance files without hardware, for
generators' CI.

---

## Versioning

`"format": "mmreg-block/1.<minor>"` and `"mmreg-instances/1.<minor>"`.

- Within `/1`, changes are additive: new optional keys or new access types raise the minor
  version. A reader rejects a file whose minor version is newer than it knows (`[E_BLOCK_VERSION]`),
  naming the version it supports, instead of misreading it.
- Unknown keys are errors (`[E_BLOCK_PARSE]`), so a typo such as `"acess": "w1c"` cannot silently
  turn a write-1-to-clear field into a read-modify-written one.
- Keys starting with `x-` are ignored everywhere. Producers put their own data there
  (`"x-generator"`, `"x-hw-access"`) without breaking readers.
- A future `/2` may change meaning; readers will keep reading `/1`.

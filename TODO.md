# mmreg — TODO

> Items within each version are in priority order. Completed items move to [changelog.md](changelog.md).

## Direction

mmreg stays a **generic** tool, tied to no register description language or generator. What it
gains is a documented **register map file** (`mmreg-map/1`) describing peripherals, registers,
fields with their access types, and the checks that must pass before a peripheral is touched.
Anything can produce that file:

- register generators can emit it as one more output;
- importers convert CMSIS-SVD (and later IP-XACT) files;
- people write it by hand for anything else.

The file format is the stable contract: mmreg stays MIT-licensed, depends on no other product, and
works for any SoC. The access-type rules it needs (write-1-to-clear, read-to-clear, read-only, wide
registers) are generic: SVD has them too. The existing `SafetyProfile` becomes the map's
per-peripheral `checks`.

Footprint stays a feature: mmreg competes with single-file `devmem2`. `libc` is the only mandatory
dependency; anything needing a parser (maps, importers) sits behind a cargo feature like
`profile`.

---

## 1.0.x — safety follow-ups

- [ ] **Leaf-only `/proc/iomem` match.** A range inside a bus window (`PCI Bus 0000:00`, `soc`,
  `amba`) passes the blueprint even when no device claims it. Accept only when the innermost
  (most indented) resource covering the range is not itself a parent of other resources, or when a
  UIO / Device Tree owner exists.
- [ ] **One lock directory for everyone.** A user who cannot create `/run/lock/mmreg` falls back to
  `/tmp/mmreg-lock` while root uses `/run/lock/mmreg`, and the two do not exclude each other. Either
  refuse instead of falling back when `/run/lock` exists, or always use one directory.
- [ ] **No stderr from the library.** Runtime-PM and clock warnings are `eprintln!`s inside the
  crate; return them to the caller (e.g. a warnings callback on `Interface`) and let the CLI print.
- [ ] **Docs example check.** Confirm the ZynqMP `GEM3_REF_CTRL` clock-active bit used in the
  docs (bit 25, `0x0200_0000`) against UG1087.

## 1.1 — register maps (`mmreg-map/1`)

- [ ] **Format** (TOML, behind a cargo feature), documented and versioned:
  `[[peripherals]]` (name, base, size, optional UIO device name, `checks` = today's safety
  profile) → `[[registers]]` (name, offset, width, reset) → `[[fields]]` (name, msb, lsb, access:
  `ro`, `rw`, `wo`, `w1c`, `w1s`, `w0c`, `rc`, `rs`, `wc`, ...; enumerated values; description).
  Additive changes only within `/1`.
- [ ] **Access by name:** `mmreg read uart0.STATUS`, `mmreg read uart0.STATUS.TX_FULL`,
  `mmreg write uart0.CTRL.ENABLE 1`, `mmreg dump uart0` (every register, fields decoded,
  enumerated values named). `--map <file>` or `MMREG_MAP`; several maps merge.
- [ ] **Safe by default, from the access types:**
  - reads of read-side-effect fields (`rc`, `rs`) need `--side-effects`; `dump` skips them;
  - writing a field of a register with `w1c`/`w1s` fields writes only that field's bits
    (other write-1 fields 0), never read-modify-write of the whole register;
  - writes to `ro` fields and reads of `wo` registers are refused;
  - fixes the current `write_subregister` clearing pending W1C bits.
- [ ] **Wide registers:** 8/16/64-bit accesses; registers wider than the bus read lowest word
  first and written highest word last (the usual order for registers latched on the last word). Aligned accesses
  stay within one page, so the per-page lock still covers them.
- [ ] **Library API from a map:** `Interface::from_map(&map, "uart0")`.
- [ ] **Importers:** CMSIS-SVD → `mmreg-map/1` (`mmreg import svd`); IP-XACT later.

## Later

- [ ] `mmreg watch <reg>`: poll and print changes (rate-limited).
- [ ] Interrupt wait through UIO (`read()` on `/dev/uioN`) for a field.
- [ ] MCP tool surface over the same API (read/write/dump by name, with the same side-effect
  rules), for agents debugging hardware.
- [ ] `mips*` / `powerpc-*-gnuspe` builds: no prebuilt Rust standard library, so only via nightly
  `-Zbuild-std`; add if a user needs one.

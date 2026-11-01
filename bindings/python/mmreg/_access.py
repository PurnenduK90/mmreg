"""Registers, fields, arrays and groups of a `Block`, bound to a `Device`, and `Device.dump()`.

Objects are created on access from the immutable specifications in `block`, so a device with a
large block holds only its top-level registers; arrays and groups bind their elements on demand.
"""

from __future__ import annotations

import operator
from typing import Any, Dict, Iterator, List, Optional, Union

from . import (
    Device,
    InlineRegister,
    MmregError,
    _api_attr_error,
    _assign,
    _check_uint,
    _HardwareValue,
    _MmregAttributeError,
    _reject_private_attr,
    _set_named,
)
from .block import Block, EnumValue, FieldSpec, GroupSpec, RegisterSpec, _err

BoundChild = Union["BlockRegister", "BlockRegisterArray", "BlockRegisterGroup", "BlockRegisterGroupArray"]


class _MmregIndexError(MmregError, IndexError):
    """Out-of-range or non-integer register or group array index."""


def _spec_attr(name: str, doc: str = "") -> property:
    """Read-only attribute served from the object's specification (no per-object copy)."""
    return property(lambda self: getattr(self._spec, name), doc=doc)


def _format_hex_value(value: int, regwidth: int) -> str:
    digits = regwidth // 4
    text = f"{value & ((1 << regwidth) - 1):0{digits}x}"
    # Group 32- and 64-bit values in 16-bit chunks: 0x0000_0001.
    if regwidth >= 32:
        text = "_".join(text[i : i + 4] for i in range(0, digits, 4))
    return f"0x{text}"


def _format_offset(offset: int) -> str:
    return f"0x{offset:02x}" if offset < 0x100 else f"0x{offset:04x}"


def _display_name(spec: FieldSpec, name: str) -> str:
    """`name`, followed by `~` when hardware changes the value (`hw: "w"` / `"rw"`)."""
    return f"{name}~" if spec.is_volatile else name


def _format_field(spec: FieldSpec, raw: int) -> str:
    name = _display_name(spec, spec.name)
    if spec.is_write_only:
        return f"{name}=-"
    value = 0 if spec.reads_zero else (raw & spec.mask) >> spec.lsb
    decoded = spec.enum_spec.decode(value) if spec.enum_spec is not None else value
    if isinstance(decoded, EnumValue):
        return f"{name}={decoded.name}"
    if spec.enum_spec is None and (value < 10 or spec.width == 1):
        return f"{name}={value}"
    return f"{name}=0x{value:x}"


class _BoundView:
    """A view of hardware through a `Device`: immutable, so `copy.copy()` returns the view itself."""

    __slots__ = ()

    def __copy__(self) -> _BoundView:
        return self

    def __setattr__(self, key: str, value: object) -> None:
        raise _api_attr_error(self, key, None)


class BlockField(_HardwareValue, _BoundView):
    """Field of a `BlockRegister`, enforcing `sw`, `onwrite`, `onread`, `singlepulse` and `encode`."""

    __slots__ = ("_register", "_spec")

    def __init__(self, register: BlockRegister, spec: FieldSpec) -> None:
        object.__setattr__(self, "_register", register)
        object.__setattr__(self, "_spec", spec)

    name = _spec_attr("name")
    msb = _spec_attr("msb")
    lsb = _spec_attr("lsb")
    width = _spec_attr("width")
    mask = _spec_attr("mask")
    sw = _spec_attr("sw")
    onwrite = _spec_attr("onwrite")
    onread = _spec_attr("onread")
    singlepulse = _spec_attr("singlepulse")
    hw = _spec_attr("hw")
    reset = _spec_attr("reset")
    encode = _spec_attr("encode")
    desc = _spec_attr("desc")

    @property
    def reg_name(self) -> str:
        return self._register.name

    @property
    def offset(self) -> int:
        return self._register.offset

    @property
    def w1c(self) -> bool:
        return self._spec.onwrite == "woclr"

    @property
    def _device(self) -> Device:
        return self._register._device

    def _error(self, code: str, detail: str) -> MmregError:
        reg = self._register
        return _err(code, detail, source=reg._source, reg=reg.name, field=self._spec.name)

    def read(self, *, side_effects: bool = False) -> Union[int, EnumValue]:
        """Reads this field's value (an `EnumValue` when it matches an enumeration entry)."""
        spec = self._spec
        if spec.sw == "na":
            raise self._error("E_ACCESS_DENIED", f"Field '{spec.name}' has sw='na' (reserved) and cannot be read.")
        if spec.is_write_only:
            raise self._error(
                "E_WRITE_ONLY", f"Field '{spec.name}' has sw={spec.sw!r} (write-only) and cannot be read."
            )
        raw = 0 if spec.reads_zero else (int(self._register.read(side_effects=side_effects)) & spec.mask) >> spec.lsb
        return spec.enum_spec.decode(raw) if spec.enum_spec is not None else raw

    def write(self, value: Union[int, str], *, side_effects: bool = False) -> int:
        """Writes `value` (integer or enum name) by a masked read-modify-write under the device lock.

        Returns the value written to the register.
        """
        spec = self._spec
        reg = self._register
        if spec.sw == "na":
            raise self._error("E_ACCESS_DENIED", f"Field '{spec.name}' has sw='na' (reserved) and cannot be written.")
        if spec.sw == "r":
            raise self._error("E_READ_ONLY", f"Field '{spec.name}' has sw='r' (read-only) and cannot be written.")
        if isinstance(value, str):
            if spec.enum_spec is None:
                raise self._error(
                    "E_INVALID_ARG",
                    f"Field '{spec.name}' has no enumeration ('encode'), cannot write string {value!r}.",
                )
            ival = spec.enum_spec.encode(value, source=reg._source, reg=reg.name, field=spec.name)
        else:
            ival = _check_uint(value, spec.width, f"Field {reg.name}.{spec.name} value", allow_bool=spec.width == 1)

        # The one implicit field of a register without fields is the whole register.
        if spec.implicit:
            reg._write_raw(ival)
            return ival
        refusal = reg._spec.rmw_refusal
        if refusal is not None:
            raise self._error(*refusal)
        if reg._spec.has_read_side_effect and not side_effects:
            raise self._error(
                "E_READ_SIDE_EFFECT",
                f"Field write on register '{reg.name}' requires reading a register with "
                f"onread={', '.join(reg._spec.onread_modes)}; call "
                f"`{reg.name}.{spec.name}.write(value, side_effects=True)` or write the whole register.",
            )
        with reg._device.lock():
            new_raw = (reg._read_raw() & ~spec.rmw_clear_mask) | spec.rmw_set_mask | ((ival << spec.lsb) & spec.mask)
            reg._write_raw(new_raw)
        return new_raw

    def __str__(self) -> str:
        return str(self.read())

    def __repr__(self) -> str:
        spec = self._spec
        bits = f"{spec.msb}:{spec.lsb}" if spec.msb != spec.lsb else f"{spec.lsb}"
        extras = [f"sw={spec.sw}"]
        extras += [f"{k}={getattr(spec, k)}" for k in ("onwrite", "onread", "encode") if getattr(spec, k)]
        if spec.singlepulse:
            extras.append("singlepulse")
        return f"<BlockField {self.reg_name}.{spec.name}[{bits}] {' '.join(extras)} @ +0x{self.offset:X}>"


class BlockRegister(_HardwareValue, _BoundView):
    """Register on a `Device` from an `mmreg-block/1` specification: `d.CTRL`, `d.CTRL.MODE = 1`."""

    __slots__ = ("_device", "_spec", "_source", "name", "offset")

    def __init__(self, device: Device, spec: RegisterSpec, *, offset: int, name: str, source: str) -> None:
        object.__setattr__(self, "_device", device)
        object.__setattr__(self, "_spec", spec)
        object.__setattr__(self, "_source", source)
        object.__setattr__(self, "name", name)
        object.__setattr__(self, "offset", offset)

    regwidth = _spec_attr("regwidth")
    accesswidth = _spec_attr("accesswidth")
    word_order = _spec_attr("word_order")
    desc = _spec_attr("desc")

    @property
    def at(self) -> int:
        return self.offset

    @property
    def reset(self) -> Optional[int]:
        """Reset value of the readable fields with a known reset, or `None` if there are none."""
        spec = self._spec
        return spec.reset_value if spec.reset_mask else None

    def _error(self, code: str, detail: str) -> MmregError:
        return _err(code, detail, source=self._source, reg=self.name)

    def _read_raw(self) -> int:
        spec = self._spec
        read_word = getattr(self._device, f"read_u{spec.accesswidth}")
        if spec.regwidth == spec.accesswidth:
            return int(read_word(self.offset))
        # Wide register: lowest address first (latching the upper words), under one lock.
        value = 0
        with self._device.lock():
            for idx in range(spec.regwidth // spec.accesswidth):
                value |= int(read_word(self.offset + idx * spec.accesswidth // 8)) << (idx * spec.accesswidth)
        return value

    def _write_raw(self, value: int) -> None:
        spec = self._spec
        checked = _check_uint(value, spec.regwidth, f"Register {self.name} value")
        write_word = getattr(self._device, f"write_u{spec.accesswidth}")
        if spec.regwidth == spec.accesswidth:
            write_word(self.offset, checked)
            return
        # Wide register: highest address first by default (the low word commits), under one lock.
        n_words = spec.regwidth // spec.accesswidth
        order = range(n_words) if spec.word_order == "lo_first" else range(n_words - 1, -1, -1)
        word_mask = (1 << spec.accesswidth) - 1
        with self._device.lock():
            for idx in order:
                write_word(self.offset + idx * spec.accesswidth // 8, (checked >> (idx * spec.accesswidth)) & word_mask)

    def _implicit_field(self) -> Optional[FieldSpec]:
        spec = self._spec
        return None if spec.has_explicit_fields else next(iter(spec.fields.values()))

    def _check_readable(self, side_effects: bool) -> None:
        spec = self._spec
        if spec.all_na:
            raise self._error(
                "E_ACCESS_DENIED", f"Register '{self.name}' has sw='na' on all fields and cannot be read."
            )
        if not spec.is_readable:
            raise self._error(
                "E_WRITE_ONLY",
                f"Register '{self.name}' is write-only (all fields have sw='w'/'w1') and cannot be read.",
            )
        if spec.has_read_side_effect and not side_effects:
            raise self._error(
                "E_READ_SIDE_EFFECT",
                f"Reading register '{self.name}' has hardware side effects (onread={', '.join(spec.onread_modes)}); "
                f"pass `side_effects=True` (`d.{self.name}.read(side_effects=True)`).",
            )

    def read(self, *, side_effects: bool = False) -> Union[int, EnumValue]:
        """Reads the register value (`side_effects=True` is required if a field has `onread`)."""
        self._check_readable(side_effects)
        only = self._implicit_field()
        if only is None:
            return self._read_raw()
        raw = 0 if only.reads_zero else self._read_raw()
        return only.enum_spec.decode(raw) if only.enum_spec is not None else raw

    def write(self, value: Union[int, str]) -> None:
        """Writes the whole register (an enum name if the register has no fields and an `encode`)."""
        spec = self._spec
        if spec.all_na:
            raise self._error(
                "E_ACCESS_DENIED", f"Register '{self.name}' has sw='na' on all fields and cannot be written."
            )
        if not spec.is_writable:
            raise self._error(
                "E_READ_ONLY", f"Register '{self.name}' is read-only (all fields have sw='r') and cannot be written."
            )
        if isinstance(value, str):
            only = self._implicit_field()
            if only is None or only.enum_spec is None:
                raise self._error(
                    "E_INVALID_ARG", f"Register '{self.name}' does not have a register-wide enumeration for {value!r}."
                )
            value = only.enum_spec.encode(value, source=self._source, reg=self.name, field=only.name)
        self._write_raw(value)

    def _format_decoded(self, raw: int) -> str:
        only = self._implicit_field()
        name = self.name if only is None else _display_name(only, self.name)
        head = f"{name} @ {_format_offset(self.offset)} = {_format_hex_value(raw, self._spec.regwidth)}"
        if only is not None:
            decoded = only.enum_spec.decode(raw) if only.enum_spec is not None else raw
            return f"{head}  ({decoded.name})" if isinstance(decoded, EnumValue) else head
        parts = [_format_field(f, raw) for f in self._spec.fields.values() if f.sw != "na"]
        return f"{head}  ({', '.join(parts)})"

    def format(self, *, side_effects: bool = False) -> str:
        """Reads the register and formats it with decoded fields."""
        return self._format_decoded(int(self.read(side_effects=side_effects)))

    def _dump_line(self, *, diff_reset: bool, side_effects: bool) -> Optional[str]:
        spec = self._spec
        head = f"{self.name} @ {_format_offset(self.offset)} = "
        if spec.all_na or not spec.is_readable:
            return (
                None
                if diff_reset
                else head + ("<skipped: reserved (sw: na)>" if spec.all_na else "<skipped: write-only>")
            )
        if spec.has_read_side_effect and not side_effects:
            # Listed even with `diff_reset`, so a diff never hides that a register was not read.
            return head + f"<skipped: read side effect ({', '.join(spec.onread_modes)})>"
        raw = self._read_raw()
        if diff_reset and spec.reset_mask and (raw & spec.reset_mask) == spec.reset_value:
            return None
        return self._format_decoded(raw)

    def _find(self, key: str) -> Optional[BlockField]:
        spec = self._spec
        found = spec.fields_by_upper.get(key.upper()) if isinstance(key, str) and spec.has_explicit_fields else None
        return None if found is None else BlockField(self, found)

    def _not_found(self, key: str) -> _MmregAttributeError:
        avail = ", ".join(self._spec.fields) if self._spec.has_explicit_fields else "<none>"
        return _MmregAttributeError(
            "E_SUBREG_NOT_FOUND",
            f"[E_SUBREG_NOT_FOUND] {self._source} (register '{self.name}', field '{key}'): "
            f"Field '{key}' not found in register '{self.name}'. Available: [{avail}]",
        )

    def _field(self, key: str) -> BlockField:
        found = self._find(key)
        if found is None:
            raise self._not_found(key)
        return found

    def __getattr__(self, item: str) -> BlockField:
        _reject_private_attr(self, item)
        return self._field(item)

    def __setattr__(self, key: str, value: object) -> None:
        _set_named(self, key, value)

    def __getitem__(self, key: str) -> BlockField:
        return self._field(key)

    def __setitem__(self, key: str, value: Union[int, str]) -> None:
        _assign(self._field(key), value)

    def __str__(self) -> str:
        return self.format()

    def __repr__(self) -> str:
        spec = self._spec
        if spec.has_explicit_fields:
            return f"<BlockRegister {self.name} @ +0x{self.offset:X} fields=[{', '.join(spec.fields)}]>"
        return f"<BlockRegister {self.name} @ +0x{self.offset:X} ({spec.regwidth}-bit)>"


def _check_index(idx: object, length: int, *, source: str, name: str) -> int:
    def error(code: str, detail: str) -> _MmregIndexError:
        return _MmregIndexError(code, f"[{code}] {source} (register '{name}', field '<none>'): {detail}")

    if isinstance(idx, bool):
        raise error("E_INVALID_ARG", "Array index must be an integer, not bool.")
    try:
        i = operator.index(idx)  # type: ignore[arg-type]
    except TypeError as exc:
        raise error("E_INVALID_ARG", f"Array index {idx!r} is not an integer.") from exc
    if not 0 <= i < length:
        raise error("E_OUT_OF_BOUNDS", f"Index {i} out of range for '{name}' (dim={length}).")
    return i


class _BoundArray(_BoundView):
    """`dim` copies of a register or group, bound on access: `len()`, iteration and `[i]`."""

    __slots__ = ("_device", "_spec", "_source", "name", "_shift")

    def __init__(
        self, device: Device, spec: Union[RegisterSpec, GroupSpec], *, shift: int, name: str, source: str
    ) -> None:
        assert spec.dim is not None
        object.__setattr__(self, "_device", device)
        object.__setattr__(self, "_spec", spec)
        object.__setattr__(self, "_source", source)
        object.__setattr__(self, "name", name)
        object.__setattr__(self, "_shift", shift)

    offset = property(lambda self: self._spec.offset + self._shift)
    stride = _spec_attr("stride")

    def _element(self, i: int) -> Any:
        raise NotImplementedError

    def __len__(self) -> int:
        return self._spec.dim  # type: ignore[no-any-return]

    def __iter__(self) -> Iterator[Any]:
        return (self._element(i) for i in range(len(self)))

    def __getitem__(self, idx: int) -> Any:
        return self._element(_check_index(idx, len(self), source=self._source, name=self.name))

    def __repr__(self) -> str:
        return f"<{type(self).__name__} {self.name}[0..{len(self) - 1}] @ +0x{self.offset:X} stride=0x{self.stride:X}>"


class BlockRegisterArray(_BoundArray):
    """Array (`dim`) of `BlockRegister`s: `d.DEBOUNCE[3]`, `d.DEBOUNCE[3] = 4`."""

    __slots__ = ()

    def _element(self, i: int) -> BlockRegister:
        spec = self._spec
        return BlockRegister(
            self._device, spec, offset=self.offset + i * spec.stride, name=f"{self.name}[{i}]", source=self._source
        )

    def __setitem__(self, idx: int, value: Union[int, str]) -> None:
        _assign(self[idx], value)


class BlockRegisterGroupArray(_BoundArray):
    """Array (`dim`) of `BlockRegisterGroup`s: `d.CH[0].CTRL`."""

    __slots__ = ()

    def _element(self, i: int) -> BlockRegisterGroup:
        return BlockRegisterGroup(
            self._device,
            self._spec,  # type: ignore[arg-type]
            shift=self._shift + i * self._spec.stride,
            name=f"{self.name}[{i}]",
            source=self._source,
        )


def _bind_child(
    device: Device, spec: Union[RegisterSpec, GroupSpec], *, shift: int, name: str, source: str
) -> BoundChild:
    if isinstance(spec, RegisterSpec):
        if spec.dim is not None:
            return BlockRegisterArray(device, spec, shift=shift, name=name, source=source)
        return BlockRegister(device, spec, offset=spec.offset + shift, name=name, source=source)
    if spec.dim is not None:
        return BlockRegisterGroupArray(device, spec, shift=shift, name=name, source=source)
    return BlockRegisterGroup(device, spec, shift=shift, name=name, source=source)


class BlockRegisterGroup(_BoundView):
    """Register group on a `Device`: `d.CH.CTRL`, `d.CH.CTRL = 1`."""

    __slots__ = ("_device", "_spec", "_source", "name", "_shift")

    def __init__(self, device: Device, spec: GroupSpec, *, shift: int, name: str, source: str) -> None:
        object.__setattr__(self, "_device", device)
        object.__setattr__(self, "_spec", spec)
        object.__setattr__(self, "_source", source)
        object.__setattr__(self, "name", name)
        object.__setattr__(self, "_shift", shift)

    offset = property(lambda self: self._spec.offset + self._shift)
    at = offset
    desc = _spec_attr("desc")

    def _find(self, key: str) -> Optional[BoundChild]:
        cspec = self._spec.children_by_upper.get(key.upper()) if isinstance(key, str) else None
        if cspec is None:
            return None
        return _bind_child(
            self._device, cspec, shift=self._shift, name=f"{self.name}.{cspec.name}", source=self._source
        )

    def _not_found(self, key: str) -> _MmregAttributeError:
        return _MmregAttributeError(
            "E_REG_NOT_FOUND",
            f"[E_REG_NOT_FOUND] {self._source} (register '{self.name}.{key}', field '<none>'): "
            f"Register '{key}' not found in group '{self.name}'. Available: [{', '.join(self._spec.children)}]",
        )

    def _child(self, key: str) -> BoundChild:
        found = self._find(key)
        if found is None:
            raise self._not_found(key)
        return found

    def __getattr__(self, item: str) -> BoundChild:
        _reject_private_attr(self, item)
        return self._child(item)

    def __setattr__(self, key: str, value: object) -> None:
        _set_named(self, key, value)

    def __getitem__(self, key: str) -> BoundChild:
        return self._child(key)

    def __setitem__(self, key: str, value: Union[int, str]) -> None:
        _assign(self._child(key), value)

    def __repr__(self) -> str:
        return f"<BlockRegisterGroup {self.name} @ +0x{self.offset:X} children=[{', '.join(self._spec.children)}]>"


def attach_block(device: Device, block: Block) -> None:
    """Checks that `block` fits the mapped window and makes its registers and groups `device` attributes."""
    dev_size = device.size
    last = max(block.flat_registers, key=lambda e: e.offset + e.spec.regwidth // 8)
    end = last.offset + last.spec.regwidth // 8
    if end > dev_size:
        raise _err(
            "E_OUT_OF_BOUNDS",
            f"Register at 0x{last.offset:X}..0x{end:X} exceeds mapped device window size 0x{dev_size:X}.",
            source=block.source,
            reg=last.full_name,
        )
    registers: Dict[str, Any] = {
        name.upper(): _bind_child(device, cspec, shift=0, name=name, source=block.source)
        for name, cspec in block.children.items()
    }
    object.__setattr__(device, "_block", block)
    object.__setattr__(device, "_registers", registers)


def format_device_dump(device: Device, *, diff_reset: bool = False, side_effects: bool = False) -> str:
    """`Device.dump()`: every block register (skipping `rclr` / `rset` ones unless `side_effects=True`),
    then the inline registers."""
    device._require_handle()
    lines: List[str] = []
    block: Optional[Block] = device.block
    if block is not None:
        for entry in block.flat_registers:
            reg = BlockRegister(device, entry.spec, offset=entry.offset, name=entry.full_name, source=block.source)
            line = reg._dump_line(diff_reset=diff_reset, side_effects=side_effects)
            if line is not None:
                lines.append(line)
    inline = [r for r in device._registers.values() if isinstance(r, InlineRegister)]
    for reg_obj in sorted(inline, key=lambda r: r.offset):
        raw = int(reg_obj.read())
        head = f"{reg_obj.name} @ {_format_offset(reg_obj.offset)} = {_format_hex_value(raw, 32)}"
        fields = reg_obj._fields.values()
        parts = ", ".join(f"{f.name}={(raw & f.mask) >> f.lsb}" for f in fields)
        lines.append(f"{head}  ({parts})" if parts else head)
    return "\n".join(lines)

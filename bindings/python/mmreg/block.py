"""
`mmreg-block/1` and `mmreg-instances/1` loader and validator (see `docs/users/07-block-files.md`).

Turns JSON into immutable specifications without touching hardware. `_access` binds a `Block` to a
`Device`; `_resolve` finds block and instance files on the search path.
"""

from __future__ import annotations

import dataclasses
import json
import re
from collections.abc import Iterator, Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple, Type, Union

from . import MmregError, _MmregOSError

SUPPORTED_BLOCK_MINOR = 0
SUPPORTED_INSTANCES_MINOR = 0

_IDENT_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
_INT_STR_RE = re.compile(r"^(?:0[xX][0-9a-fA-F](?:_?[0-9a-fA-F])*|0[bB][01](?:_?[01])*|[0-9](?:_?[0-9])*)$")
_BITS_RE = re.compile(r"^([0-9]+)(?::([0-9]+))?$")
_FORMAT_RE = re.compile(r"^(mmreg-block|mmreg-instances)/([0-9]+)\.([0-9]+)$")

_VALID_WIDTHS = (8, 16, 32, 64)
_VALID_WORD_ORDERS = ("hi_first", "lo_first")
_VALID_SW = ("rw", "r", "w", "rw1", "w1", "na")
_VALID_ONWRITE = ("woclr", "woset", "wot", "wzc", "wzs", "wzt", "wclr", "wset")
_VALID_ONREAD = ("rclr", "rset")
_VALID_HW = ("r", "w", "rw", "na")

# In a read-modify-write of another field: written as 1, or as 0 (spec, "Access Behaviour").
_RMW_WRITE_ONE = ("wzc", "wzs", "wzt")
_RMW_WRITE_ZERO_ONWRITE = ("woclr", "woset", "wot")
_RMW_WRITE_ZERO_SW = ("r", "w", "w1")

_FILE_SCALAR_KEYS = frozenset({"format", "compatible"})
_WIDTH_KEYS = frozenset({"regwidth", "accesswidth", "word_order"})
_BLOCK_SCALAR_KEYS = _WIDTH_KEYS | {"desc", "name", "size"}
_GROUP_SCALAR_KEYS = _WIDTH_KEYS | {"at", "desc", "name", "dim", "stride"}
_FIELD_DEFAULT_KEYS = frozenset({"sw", "onwrite", "onread", "singlepulse", "hw", "reset", "encode"})
_REGISTER_SCALAR_KEYS = _GROUP_SCALAR_KEYS | _FIELD_DEFAULT_KEYS
_FIELD_SCALAR_KEYS = _FIELD_DEFAULT_KEYS | {"bits", "desc", "name"}
_ENUM_ENTRY_KEYS = frozenset({"value", "desc", "name"})
_INSTANCE_KEYS = frozenset({"block", "desc", "uio", "uio_map", "at", "size", "checks"})
_CHECK_KEYS = frozenset({"name", "addr", "mask", "expected"})

_U32_MAX = 0xFFFF_FFFF
_U64_MAX = (1 << 64) - 1


def _err(
    code: str,
    detail: str,
    *,
    source: str,
    reg: str = "<block>",
    field: str = "<none>",
    cls: Type[MmregError] = MmregError,
) -> MmregError:
    """An `MmregError` (or subclass `cls`) whose message names the file, register and field."""
    return cls(code, f"[{code}] {source} (register '{reg}', field '{field}'): {detail}")


def _parse_err(detail: str, *, source: str, reg: str = "<block>", field: str = "<none>") -> MmregError:
    return _err("E_BLOCK_PARSE", detail, source=source, reg=reg, field=field)


def _props(
    obj: Mapping[str, Any], *, source: str, reg: str = "<block>", field: str = "<none>"
) -> Iterator[Tuple[str, Any]]:
    """Yields the items of `obj` except `x-*` extension keys, rejecting keys that are not strings."""
    for key, value in obj.items():
        if not isinstance(key, str):
            raise _parse_err(f"Key {key!r} must be a string.", source=source, reg=reg, field=field)
        if not key.startswith("x-"):
            yield key, value


def _check_keys(
    obj: Mapping[str, Any], allowed: frozenset, what: str, *, source: str, reg: str, field: str = "<none>"
) -> None:
    for key, _ in _props(obj, source=source, reg=reg, field=field):
        if key not in allowed:
            raise _parse_err(f"Unknown property {key!r} in {what}.", source=source, reg=reg, field=field)


def _check_ident(name: str, what: str, *, source: str, reg: str = "<block>", field: str = "<none>") -> None:
    if not _IDENT_RE.match(name):
        raise _parse_err(f"{what} {name!r} must match [A-Za-z_][A-Za-z0-9_]*.", source=source, reg=reg, field=field)


def _split_children(
    obj: Mapping[str, Any],
    scalar_keys: frozenset,
    what: str,
    *,
    source: str,
    reg: str,
    field: str = "<none>",
) -> Dict[str, Mapping[str, Any]]:
    """Returns the object-valued children of `obj` (identifiers, unique ignoring case), checking that
    every other key is one of `scalar_keys`. `field` names `obj` in errors when it is a field."""
    children: Dict[str, Mapping[str, Any]] = {}
    seen_upper: Dict[str, str] = {}
    for key, value in _props(obj, source=source, reg=reg, field=field):
        if isinstance(value, Mapping):
            _check_ident(key, f"{what} child name", source=source, reg=reg, field=key)
            prev = seen_upper.setdefault(key.upper(), key)
            if prev != key:
                raise _parse_err(
                    f"Child names {prev!r} and {key!r} of {what} '{reg}' differ only by case.",
                    source=source,
                    reg=reg,
                    field=key,
                )
            children[key] = value
        elif key not in scalar_keys:
            raise _parse_err(f"Unknown {what} property {key!r}.", source=source, reg=reg, field=field)
    return children


def _parse_int(
    val: Any,
    what: str,
    *,
    source: str,
    reg: str = "<block>",
    field: str = "<none>",
    min_val: int = 0,
    max_val: Optional[int] = None,
) -> int:
    if isinstance(val, bool) or not isinstance(val, (int, str)):
        raise _parse_err(f"{what} {val!r} must be an integer or numeric string.", source=source, reg=reg, field=field)
    if isinstance(val, str):
        text = val.strip()
        if not _INT_STR_RE.match(text):
            raise _parse_err(f"{what} {val!r} is not a valid integer literal.", source=source, reg=reg, field=field)
        ival = int(text, 0)
    else:
        ival = val
    if ival < min_val or (max_val is not None and ival > max_val):
        bound = f">= {min_val}" if max_val is None else f"in [{min_val}, 0x{max_val:X}]"
        raise _parse_err(
            f"{what} {val!r} ({ival}) is out of range (expected {bound}).", source=source, reg=reg, field=field
        )
    return ival


def _parse_str(
    val: Any,
    what: str,
    *,
    source: str,
    reg: str = "<block>",
    field: str = "<none>",
    non_empty: bool = False,
) -> str:
    if not isinstance(val, str) or (non_empty and not val.strip()):
        req = "a non-empty string" if non_empty else "a string"
        raise _parse_err(f"{what} must be {req}, got {val!r}.", source=source, reg=reg, field=field)
    return val


def _parse_choice(
    val: Any, what: str, choices: Tuple[str, ...], *, source: str, reg: str, field: str = "<none>"
) -> str:
    text = _parse_str(val, what, source=source, reg=reg, field=field)
    if text not in choices:
        raise _parse_err(
            f"Invalid '{what}' value {text!r} (expected one of {choices}).", source=source, reg=reg, field=field
        )
    return text


def _check_format(raw_format: Any, expected_kind: Optional[str], *, source: str) -> str:
    """Checks `format` and returns its kind (`"mmreg-block"` or `"mmreg-instances"`)."""
    if raw_format is None:
        raise _parse_err("Missing required 'format' key at file root.", source=source)
    supported = f"mmreg-block/1.{SUPPORTED_BLOCK_MINOR}, mmreg-instances/1.{SUPPORTED_INSTANCES_MINOR}"
    m = _FORMAT_RE.match(raw_format) if isinstance(raw_format, str) else None
    if m is None:
        raise _err("E_BLOCK_VERSION", f"Unsupported format {raw_format!r} (supported: {supported}).", source=source)
    kind, major, minor = m.group(1), int(m.group(2)), int(m.group(3))
    if expected_kind is not None and kind != expected_kind:
        raise _err("E_BLOCK_VERSION", f"Expected '{expected_kind}/1.0', found {raw_format!r}.", source=source)
    max_minor = SUPPORTED_BLOCK_MINOR if kind == "mmreg-block" else SUPPORTED_INSTANCES_MINOR
    if major != 1 or minor > max_minor:
        raise _err(
            "E_BLOCK_VERSION",
            f"Format version {raw_format!r} is not supported by this version of mmreg "
            f"(supported up to '{kind}/1.{max_minor}').",
            source=source,
        )
    return kind


def _load_json_text(text: str, *, source: str) -> Dict[str, Any]:
    def unique_pairs(pairs: List[Tuple[str, Any]]) -> Dict[str, Any]:
        out: Dict[str, Any] = {}
        for k, v in pairs:
            if k in out:
                raise _parse_err(f"Duplicate JSON key {k!r}.", source=source, reg=k)
            out[k] = v
        return out

    try:
        data = json.loads(text, object_pairs_hook=unique_pairs)
    except json.JSONDecodeError as exc:
        raise _parse_err(f"Invalid JSON at line {exc.lineno}, column {exc.colno}: {exc.msg}.", source=source) from exc
    if not isinstance(data, dict):
        raise _parse_err("Top-level JSON value must be an object.", source=source)
    return data


def _read_json_file(path: Union[str, Path]) -> Tuple[Dict[str, Any], str]:
    source = str(path)
    try:
        text = Path(path).read_text(encoding="utf-8")
    except OSError as exc:
        raise _err("E_BLOCK_NOT_FOUND", f"Cannot read file: {exc}", source=source, cls=_MmregOSError) from exc
    return _load_json_text(text, source=source), source


# ---------------------------------------------------------------------------
# Enumerations
# ---------------------------------------------------------------------------


class EnumValue(int):
    """Decoded enumeration value: an `int` that also carries `.name` and compares equal to it."""

    name: str
    desc: str

    def __new__(cls, value: int, name: str, desc: str = "") -> EnumValue:
        obj = super().__new__(cls, value)
        obj.name = name
        obj.desc = desc
        return obj

    @property
    def value(self) -> int:
        return int(self)

    def __str__(self) -> str:
        return self.name

    def __repr__(self) -> str:
        return f"<EnumValue {self.name}={int(self)}>"

    def __eq__(self, other: object) -> bool:
        if isinstance(other, str):
            return self.name.upper() == other.strip().upper()
        return super().__eq__(other)

    def __ne__(self, other: object) -> bool:
        return not self == other

    def __hash__(self) -> int:
        return super().__hash__()


@dataclass(frozen=True)
class EnumEntry:
    name: str
    value: int
    desc: str = ""


@dataclass(frozen=True)
class EnumSpec:
    name: str
    entries_by_upper: Dict[str, EnumEntry]
    entries_by_value: Dict[int, EnumEntry]

    def encode(self, value: Union[int, str], *, source: str, reg: str, field: str) -> int:
        if not isinstance(value, str):
            return value
        entry = self.entries_by_upper.get(value.strip().upper())
        if entry is None:
            valid = ", ".join(e.name for e in self.entries_by_upper.values())
            raise _err(
                "E_INVALID_ARG",
                f"Unknown enum value {value!r} for enumeration '{self.name}' (valid: [{valid}]).",
                source=source,
                reg=reg,
                field=field,
            )
        return entry.value

    def decode(self, raw: int) -> Union[int, EnumValue]:
        entry = self.entries_by_value.get(raw)
        return raw if entry is None else EnumValue(entry.value, entry.name, entry.desc)


def _parse_enums(raw_enums: Any, *, source: str) -> Dict[str, EnumSpec]:
    if raw_enums is None:
        return {}
    if not isinstance(raw_enums, Mapping):
        raise _parse_err("'enum' at file root must be a JSON object.", source=source)
    enums: Dict[str, EnumSpec] = {}
    for enum_name, raw_entries in _split_children(raw_enums, frozenset(), "enum", source=source, reg="enum").items():
        by_upper: Dict[str, EnumEntry] = {}
        by_val: Dict[int, EnumEntry] = {}
        for entry_name, entry_obj in _split_children(
            raw_entries, frozenset(), "enumeration", source=source, reg=enum_name
        ).items():
            ctx: Dict[str, str] = {"source": source, "reg": enum_name, "field": entry_name}
            _check_keys(entry_obj, _ENUM_ENTRY_KEYS, f"enum entry '{enum_name}.{entry_name}'", **ctx)
            if "value" not in entry_obj:
                raise _parse_err(f"Enum entry '{enum_name}.{entry_name}' is missing required 'value'.", **ctx)
            val = _parse_int(entry_obj["value"], f"Enum value for '{enum_name}.{entry_name}'", **ctx)
            if val in by_val:
                raise _parse_err(
                    f"Duplicate enum value {val} in '{enum_name}' ('{by_val[val].name}' and '{entry_name}').", **ctx
                )
            desc = _parse_str(entry_obj.get("desc", ""), "desc", **ctx)
            _parse_str(entry_obj.get("name", ""), "name", **ctx)
            by_upper[entry_name.upper()] = by_val[val] = EnumEntry(name=entry_name, value=val, desc=desc)
        if not by_upper:
            raise _parse_err(
                f"Enumeration '{enum_name}' must contain at least one value.", source=source, reg=enum_name
            )
        enums[enum_name] = EnumSpec(name=enum_name, entries_by_upper=by_upper, entries_by_value=by_val)
    return enums


# ---------------------------------------------------------------------------
# Specifications
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class FieldSpec:
    name: str
    display_name: str
    msb: int
    lsb: int
    width: int
    mask: int
    desc: str
    sw: str
    onwrite: Optional[str]
    onread: Optional[str]
    singlepulse: bool
    hw: str
    reset: Optional[int]
    encode: Optional[str]
    enum_spec: Optional[EnumSpec]
    implicit: bool
    # Read-modify-write of this field: `(old & ~rmw_clear_mask) | rmw_set_mask | value`.
    rmw_clear_mask: int = 0
    rmw_set_mask: int = 0

    @property
    def is_write_only(self) -> bool:
        """`sw: "w"` / `"w1"` without `singlepulse`: the value cannot be read back."""
        return self.sw in ("w", "w1") and not self.singlepulse

    @property
    def reads_zero(self) -> bool:
        """`sw: "w"` with `singlepulse`: reads 0 without touching hardware."""
        return self.sw == "w" and self.singlepulse

    @property
    def is_readable(self) -> bool:
        return self.sw != "na" and not self.is_write_only

    @property
    def is_volatile(self) -> bool:
        """`hw: "w"` / `"rw"`: hardware changes the value (status, counters), marked `~` when printed."""
        return self.hw in ("w", "rw")

    @property
    def is_writable(self) -> bool:
        return self.sw in ("rw", "w", "rw1", "w1")


@dataclass(frozen=True)
class RegisterSpec:
    name: str
    display_name: str
    full_name: str
    at: int
    offset: int
    desc: str
    regwidth: int
    accesswidth: int
    word_order: str
    dim: Optional[int]
    stride: int
    fields: Dict[str, FieldSpec]
    fields_by_upper: Dict[str, FieldSpec]
    has_explicit_fields: bool
    reset_value: int
    reset_mask: int
    onread_modes: Tuple[str, ...]
    all_na: bool
    is_readable: bool
    is_writable: bool
    rmw_refusal: Optional[Tuple[str, str]]  # (error code, reason) when field writes are refused

    @property
    def has_read_side_effect(self) -> bool:
        return bool(self.onread_modes)

    @property
    def span(self) -> int:
        return ((self.dim or 1) - 1) * self.stride + self.regwidth // 8


@dataclass(frozen=True)
class GroupSpec:
    name: str
    display_name: str
    full_name: str
    at: int
    offset: int
    desc: str
    regwidth: int
    accesswidth: int
    word_order: str
    dim: Optional[int]
    stride: int
    children: Dict[str, Union[RegisterSpec, GroupSpec]]
    children_by_upper: Dict[str, Union[RegisterSpec, GroupSpec]]

    @property
    def span(self) -> int:
        return (self.dim or 1) * self.stride


@dataclass(frozen=True)
class FlatRegisterEntry:
    """One concrete register instance at its expanded byte offset (for overlap, bounds and dump)."""

    full_name: str
    offset: int
    spec: RegisterSpec


# (regwidth, accesswidth, word_order) inherited from the enclosing block or group.
_Widths = Tuple[int, int, str]
# What a block inherits: accesswidth `None` defaults to the block's regwidth.
_BLOCK_DEFAULT_WIDTHS: Tuple[int, Optional[int], str] = (32, None, "hi_first")


def _parse_widths(
    obj: Mapping[str, Any], inherited: Tuple[int, Optional[int], str], *, source: str, reg: str
) -> _Widths:
    def width(key: str, default: int) -> int:
        if key not in obj:
            return default
        value = _parse_int(obj[key], key, source=source, reg=reg)
        if value not in _VALID_WIDTHS:
            raise _parse_err(f"'{key}' must be 8, 16, 32, or 64, got {value}.", source=source, reg=reg)
        return value

    regwidth = width("regwidth", inherited[0])
    # No inherited accesswidth, or one wider than this register's regwidth, falls back to the regwidth.
    inherited_access = inherited[1]
    accesswidth = width(
        "accesswidth", inherited_access if inherited_access is not None and inherited_access <= regwidth else regwidth
    )
    if accesswidth > regwidth or regwidth % accesswidth != 0:
        raise _parse_err(
            f"'accesswidth' ({accesswidth}) must divide 'regwidth' ({regwidth}) and not exceed it.",
            source=source,
            reg=reg,
        )
    word_order = inherited[2]
    if "word_order" in obj:
        word_order = _parse_choice(obj["word_order"], "word_order", _VALID_WORD_ORDERS, source=source, reg=reg)
    return regwidth, accesswidth, word_order


def _parse_offset(
    obj: Mapping[str, Any], parent_offset: int, reg_bytes: int, what: str, *, source: str, reg: str
) -> Tuple[int, int]:
    """Returns `(at, absolute offset)`, checking alignment to `regwidth / 8`."""
    at = _parse_int(obj["at"], "at", source=source, reg=reg)
    offset = parent_offset + at
    if offset % reg_bytes != 0:
        raise _parse_err(
            f"{what} offset 0x{offset:X} is not aligned to regwidth / 8 ({reg_bytes} bytes).", source=source, reg=reg
        )
    return at, offset


def _parse_dim(
    obj: Mapping[str, Any], copy_size: int, reg_bytes: int, what: str, *, source: str, reg: str
) -> Tuple[Optional[int], int]:
    """Returns `(dim, stride)`; `stride` defaults to, and must be at least, the size of one copy."""
    if "dim" not in obj:
        if "stride" in obj:
            raise _parse_err(f"{what} specifies 'stride' without 'dim'.", source=source, reg=reg)
        return None, copy_size
    dim = _parse_int(obj["dim"], "dim", source=source, reg=reg, min_val=1)
    stride = copy_size
    if "stride" in obj:
        stride = _parse_int(obj["stride"], "stride", source=source, reg=reg, min_val=copy_size)
    if stride % reg_bytes != 0:
        raise _parse_err(
            f"{what} 'stride' ({stride}) is not a multiple of regwidth / 8 ({reg_bytes} bytes).", source=source, reg=reg
        )
    return dim, stride


def _parse_bits(raw_bits: Any, regwidth: int, *, source: str, reg: str, field: str) -> Tuple[int, int]:
    m = _BITS_RE.match(raw_bits.strip()) if isinstance(raw_bits, str) else None
    if m is None:
        raise _parse_err(
            f'Invalid field \'bits\' specification {raw_bits!r} (expected "msb:lsb" or "n").',
            source=source,
            reg=reg,
            field=field,
        )
    msb = int(m.group(1))
    lsb = int(m.group(2)) if m.group(2) is not None else msb
    if msb < lsb or msb >= regwidth:
        raise _parse_err(
            f"Field bit range [{msb}:{lsb}] is outside register width ({regwidth} bits, valid 0..{regwidth - 1}) "
            "or has msb < lsb.",
            source=source,
            reg=reg,
            field=field,
        )
    return msb, lsb


def _parse_access(
    obj: Mapping[str, Any],
    defaults: Mapping[str, Any],
    *,
    msb: int,
    lsb: int,
    enums: Mapping[str, EnumSpec],
    check_enum_width: bool,
    source: str,
    reg: str,
    field: str,
) -> Dict[str, Any]:
    """Parses the field keys (`sw`, `onwrite`, ...) of `obj`, falling back to the register's `defaults`.

    Returns them as `FieldSpec` keyword arguments.
    """
    ctx: Dict[str, str] = {"source": source, "reg": reg, "field": field}
    width = msb - lsb + 1
    max_val = (1 << width) - 1

    def choice(key: str, choices: Tuple[str, ...], default: Optional[str]) -> Optional[str]:
        raw = obj.get(key, defaults.get(key, default))
        return None if raw is None else _parse_choice(raw, key, choices, **ctx)

    sw = choice("sw", _VALID_SW, "rw")
    singlepulse = obj.get("singlepulse", defaults.get("singlepulse", False))
    if not isinstance(singlepulse, bool):
        raise _parse_err(f"'singlepulse' must be a boolean, got {singlepulse!r}.", **ctx)
    if singlepulse and sw not in ("w", "rw"):
        raise _parse_err(f"'singlepulse: true' is only valid with sw 'w' or 'rw', not {sw!r}.", **ctx)

    reset: Optional[int] = 0
    if "reset" in obj:
        raw_reset = obj["reset"]
        if isinstance(raw_reset, str) and raw_reset.strip().lower() == "x":
            reset = None
        else:
            reset = _parse_int(raw_reset, "reset", **ctx, max_val=max_val)
    elif "reset" in defaults:
        reg_reset = defaults["reset"]
        reset = None if reg_reset is None else (reg_reset >> lsb) & max_val

    encode: Optional[str] = None
    enum_spec: Optional[EnumSpec] = None
    raw_encode = obj.get("encode", defaults.get("encode"))
    if raw_encode is not None:
        encode = _parse_str(raw_encode, "encode", **ctx, non_empty=True)
        enum_spec = enums.get(encode)
        if enum_spec is None:
            raise _parse_err(f"Field references unknown enumeration {encode!r}.", **ctx)
        too_wide = [e for e in enum_spec.entries_by_value.values() if e.value > max_val] if check_enum_width else []
        if too_wide:
            raise _parse_err(
                f"Enumeration '{encode}' entry '{too_wide[0].name}' value {too_wide[0].value} "
                f"does not fit in {width}-bit field (max {max_val}).",
                **ctx,
            )

    return {
        "sw": sw,
        "onwrite": choice("onwrite", _VALID_ONWRITE, None),
        "onread": choice("onread", _VALID_ONREAD, None),
        "singlepulse": singlepulse,
        "hw": choice("hw", _VALID_HW, "r"),
        "reset": reset,
        "encode": encode,
        "enum_spec": enum_spec,
    }


def _with_rmw_masks(fields: List[FieldSpec]) -> List[FieldSpec]:
    """Sets each field's read-modify-write masks from the other fields (spec rule 1): `wz*` fields are
    written as 1; `wo*`, `sw: "r"`, `"w"`, `"w1"` and `singlepulse` fields as 0; the rest keep their value."""
    one_mask = 0
    zero_mask = 0
    for f in fields:
        if f.onwrite in _RMW_WRITE_ONE:
            one_mask |= f.mask
        elif f.onwrite in _RMW_WRITE_ZERO_ONWRITE or f.sw in _RMW_WRITE_ZERO_SW or f.singlepulse:
            zero_mask |= f.mask
    return [
        dataclasses.replace(
            f,
            rmw_clear_mask=f.mask | ((one_mask | zero_mask) & ~f.mask),
            rmw_set_mask=one_mask & ~f.mask,
        )
        for f in fields
    ]


def _parse_register(
    name: str,
    full_name: str,
    obj: Mapping[str, Any],
    *,
    parent_offset: int,
    inherited: _Widths,
    enums: Mapping[str, EnumSpec],
    source: str,
) -> RegisterSpec:
    child_objs = _split_children(obj, _REGISTER_SCALAR_KEYS, "register", source=source, reg=full_name)
    regwidth, accesswidth, word_order = _parse_widths(obj, inherited, source=source, reg=full_name)
    reg_bytes = regwidth // 8
    at, offset = _parse_offset(obj, parent_offset, reg_bytes, "Register", source=source, reg=full_name)
    desc = _parse_str(obj.get("desc", ""), "desc", source=source, reg=full_name)
    display_name = _parse_str(obj.get("name", name), "name", source=source, reg=full_name)
    dim, stride = _parse_dim(obj, reg_bytes, reg_bytes, "Register", source=source, reg=full_name)

    # Register-level field keys: the defaults of its fields, or the one implicit field without fields.
    reg_access = _parse_access(
        obj,
        {},
        msb=regwidth - 1,
        lsb=0,
        enums=enums,
        check_enum_width=not child_objs,
        source=source,
        reg=full_name,
        field="<register>",
    )

    fields: List[FieldSpec] = []
    used_mask = 0
    for fname, fobj in child_objs.items():
        ctx: Dict[str, str] = {"source": source, "reg": full_name, "field": fname}
        if _split_children(fobj, _FIELD_SCALAR_KEYS, "field", **ctx):
            raise _parse_err("A field cannot contain child objects.", **ctx)
        if "bits" not in fobj:
            raise _parse_err("Field is missing required 'bits' property.", **ctx)
        msb, lsb = _parse_bits(fobj["bits"], regwidth, **ctx)
        fmask = ((1 << (msb - lsb + 1)) - 1) << lsb
        if fmask & used_mask:
            other = next(f.name for f in fields if f.mask & fmask)
            raise _parse_err(f"Field [{msb}:{lsb}] overlaps field '{other}'.", **ctx)
        used_mask |= fmask
        fields.append(
            FieldSpec(
                name=fname,
                display_name=_parse_str(fobj.get("name", fname), "name", **ctx),
                msb=msb,
                lsb=lsb,
                width=msb - lsb + 1,
                mask=fmask,
                desc=_parse_str(fobj.get("desc", ""), "desc", **ctx),
                implicit=False,
                **_parse_access(fobj, reg_access, msb=msb, lsb=lsb, enums=enums, check_enum_width=True, **ctx),
            )
        )
    if not fields:
        fields.append(
            FieldSpec(
                name=name,
                display_name=display_name,
                msb=regwidth - 1,
                lsb=0,
                width=regwidth,
                mask=(1 << regwidth) - 1,
                desc=desc,
                implicit=True,
                **reg_access,
            )
        )
    fields = _with_rmw_masks(fields)

    reset_val = 0
    reset_mask = 0
    for f in fields:
        if f.is_readable and f.reset is not None:
            reset_mask |= f.mask
            reset_val |= (f.reset << f.lsb) & f.mask

    write_only = [f.name for f in fields if f.is_write_only]
    write_any = [f.name for f in fields if f.onwrite in ("wclr", "wset")]
    rmw_refusal: Optional[Tuple[str, str]] = None
    if write_any:
        rmw_refusal = (
            "E_RMW_UNSAFE",
            f"Register '{full_name}' contains write-clear/set field(s) {write_any} and cannot be "
            f"read-modified-written; use a whole-register write (`d.{full_name} = value`).",
        )
    elif write_only:
        rmw_refusal = (
            "E_WRITE_ONLY",
            f"Register '{full_name}' contains write-only field(s) {write_only} (without singlepulse) "
            f"whose value cannot be read back; use a whole-register write (`d.{full_name} = value`).",
        )

    return RegisterSpec(
        name=name,
        display_name=display_name,
        full_name=full_name,
        at=at,
        offset=offset,
        desc=desc,
        regwidth=regwidth,
        accesswidth=accesswidth,
        word_order=word_order,
        dim=dim,
        stride=stride,
        fields={f.name: f for f in fields},
        fields_by_upper={f.name.upper(): f for f in fields},
        has_explicit_fields=bool(child_objs),
        reset_value=reset_val,
        reset_mask=reset_mask,
        onread_modes=tuple(sorted({f.onread for f in fields if f.onread is not None})),
        all_na=all(f.sw == "na" for f in fields),
        is_readable=any(f.is_readable for f in fields),
        is_writable=any(f.is_writable for f in fields),
        rmw_refusal=rmw_refusal,
    )


def _classify(name: str, obj: Mapping[str, Any], *, source: str) -> str:
    """Returns `'group'` or `'register'` from the children's tell-tale keys (`at` vs `bits`)."""
    if "bits" in obj:
        raise _parse_err(
            "Field object ('bits') is only valid inside a register, not directly in a block or group.",
            source=source,
            reg=name,
        )
    if "at" not in obj:
        raise _parse_err(f"Object '{name}' must have 'at' (register or register group).", source=source, reg=name)
    kinds = set()
    for key, value in _props(obj, source=source, reg=name):
        if not isinstance(value, Mapping):
            continue
        has_at, has_bits = "at" in value, "bits" in value
        if has_at == has_bits:
            raise _parse_err(
                f"Child object '{key}' must specify either 'at' (register) or 'bits' (field).",
                source=source,
                reg=name,
                field=key,
            )
        kinds.add("group" if has_at else "register")
    if len(kinds) > 1:
        raise _parse_err(
            f"Object '{name}' mixes 'at' children (registers) and 'bits' children (fields).", source=source, reg=name
        )
    return kinds.pop() if kinds else "register"


def _parse_children(
    child_objs: Mapping[str, Mapping[str, Any]],
    *,
    prefix: str,
    parent_offset: int,
    inherited: _Widths,
    enums: Mapping[str, EnumSpec],
    source: str,
) -> Dict[str, Union[RegisterSpec, GroupSpec]]:
    children: Dict[str, Union[RegisterSpec, GroupSpec]] = {}
    for cname, cobj in child_objs.items():
        full_name = f"{prefix}.{cname}" if prefix else cname
        parse = _parse_group if _classify(full_name, cobj, source=source) == "group" else _parse_register
        children[cname] = parse(
            cname, full_name, cobj, parent_offset=parent_offset, inherited=inherited, enums=enums, source=source
        )
    return children


def _parse_group(
    name: str,
    full_name: str,
    obj: Mapping[str, Any],
    *,
    parent_offset: int,
    inherited: _Widths,
    enums: Mapping[str, EnumSpec],
    source: str,
) -> GroupSpec:
    child_objs = _split_children(obj, _GROUP_SCALAR_KEYS, "register group", source=source, reg=full_name)
    widths = _parse_widths(obj, inherited, source=source, reg=full_name)
    reg_bytes = widths[0] // 8
    at, offset = _parse_offset(obj, parent_offset, reg_bytes, "Register group", source=source, reg=full_name)
    children = _parse_children(
        child_objs, prefix=full_name, parent_offset=offset, inherited=widths, enums=enums, source=source
    )
    copy_size = max(c.at + c.span for c in children.values())
    dim, stride = _parse_dim(obj, copy_size, reg_bytes, "Register group", source=source, reg=full_name)
    return GroupSpec(
        name=name,
        display_name=_parse_str(obj.get("name", name), "name", source=source, reg=full_name),
        full_name=full_name,
        at=at,
        offset=offset,
        desc=_parse_str(obj.get("desc", ""), "desc", source=source, reg=full_name),
        regwidth=widths[0],
        accesswidth=widths[1],
        word_order=widths[2],
        dim=dim,
        stride=stride,
        children=children,
        children_by_upper={k.upper(): v for k, v in children.items()},
    )


def _expand_flat_registers(
    children: Mapping[str, Union[RegisterSpec, GroupSpec]],
    *,
    base_shift: int = 0,
    name_prefix: str = "",
) -> Iterator[FlatRegisterEntry]:
    for child in children.values():
        prefix = f"{name_prefix}.{child.name}" if name_prefix else child.name
        copies = (
            [(prefix, 0)] if child.dim is None else [(f"{prefix}[{i}]", i * child.stride) for i in range(child.dim)]
        )
        for copy_name, shift in copies:
            if isinstance(child, RegisterSpec):
                yield FlatRegisterEntry(full_name=copy_name, offset=child.offset + base_shift + shift, spec=child)
            else:
                yield from _expand_flat_registers(child.children, base_shift=base_shift + shift, name_prefix=copy_name)


@dataclass(frozen=True)
class Block:
    """Validated `mmreg-block/1` specification."""

    key: str
    name: str
    compatible: str
    desc: str
    size: Optional[int]
    regwidth: int
    accesswidth: int
    word_order: str
    enums: Dict[str, EnumSpec]
    children: Dict[str, Union[RegisterSpec, GroupSpec]]
    children_by_upper: Dict[str, Union[RegisterSpec, GroupSpec]]
    flat_registers: Tuple[FlatRegisterEntry, ...]
    min_window_size: int
    source: str

    @classmethod
    def from_dict(cls, data: Mapping[str, Any], *, source: str = "<dict>") -> Block:
        if not isinstance(data, Mapping):
            raise _parse_err("Block data must be a mapping.", source=source)
        _check_format(data.get("format"), "mmreg-block", source=source)
        if "compatible" not in data:
            raise _parse_err("Missing required 'compatible' string at file root.", source=source)
        compatible = _parse_str(data["compatible"], "compatible", source=source, non_empty=True)
        enums = _parse_enums(data.get("enum"), source=source)

        block_candidates = [
            (k, v)
            for k, v in _split_children(data, _FILE_SCALAR_KEYS, "file", source=source, reg="<file>").items()
            if k != "enum"
        ]
        if len(block_candidates) != 1:
            raise _parse_err(
                f"File must contain exactly one block object alongside 'format'/'compatible', "
                f"found {len(block_candidates)} ({[k for k, _ in block_candidates]}).",
                source=source,
            )
        block_key, block_obj = block_candidates[0]
        child_objs = _split_children(block_obj, _BLOCK_SCALAR_KEYS, "block", source=source, reg=block_key)
        if not child_objs:
            raise _parse_err(f"Block '{block_key}' contains no registers.", source=source, reg=block_key)

        widths = _parse_widths(block_obj, _BLOCK_DEFAULT_WIDTHS, source=source, reg=block_key)
        size = None
        if "size" in block_obj:
            size = _parse_int(block_obj["size"], "size", source=source, reg=block_key, min_val=1)
        children = _parse_children(child_objs, prefix="", parent_offset=0, inherited=widths, enums=enums, source=source)
        flat = sorted(_expand_flat_registers(children), key=lambda e: (e.offset, e.full_name))

        # Sorted by offset, any overlap shows between neighbours.
        max_end = 0
        for prev, cur in zip([None, *flat], flat):
            cur_end = cur.offset + cur.spec.regwidth // 8
            span = f"Register at 0x{cur.offset:X}..0x{cur_end:X}"
            if size is not None and cur_end > size:
                raise _parse_err(f"{span} exceeds block size 0x{size:X}.", source=source, reg=cur.full_name)
            if prev is not None and cur.offset < prev.offset + prev.spec.regwidth // 8:
                raise _parse_err(
                    f"{span} overlaps '{prev.full_name}' at 0x{prev.offset:X}.", source=source, reg=cur.full_name
                )
            max_end = max(max_end, cur_end)

        return cls(
            key=block_key,
            name=_parse_str(block_obj.get("name", block_key), "name", source=source, reg=block_key),
            compatible=compatible,
            desc=_parse_str(block_obj.get("desc", ""), "desc", source=source, reg=block_key),
            size=size,
            regwidth=widths[0],
            accesswidth=widths[1],
            word_order=widths[2],
            enums=enums,
            children=children,
            children_by_upper={k.upper(): v for k, v in children.items()},
            flat_registers=tuple(flat),
            min_window_size=size if size is not None else max_end,
            source=source,
        )

    @classmethod
    def from_json(cls, text: str, *, source: str = "<json>") -> Block:
        return cls.from_dict(_load_json_text(text, source=source), source=source)

    @classmethod
    def from_file(cls, path: Union[str, Path]) -> Block:
        data, source = _read_json_file(path)
        return cls.from_dict(data, source=source)


# ---------------------------------------------------------------------------
# Instance files
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class CheckSpec:
    name: str
    addr: int
    mask: int
    expected: int


@dataclass(frozen=True)
class InstanceSpec:
    name: str
    block: str
    desc: str
    uio: Optional[str]
    uio_map: int
    at: Optional[int]
    size: Optional[int]
    checks: Tuple[CheckSpec, ...]
    source: str


def _parse_check(chk: Any, index: int, *, source: str, inst_name: str) -> CheckSpec:
    ctx: Dict[str, str] = {"source": source, "reg": inst_name, "field": f"checks[{index}]"}
    if not isinstance(chk, Mapping):
        raise _parse_err("Each entry in 'checks' must be a JSON object.", **ctx)
    _check_keys(chk, _CHECK_KEYS, "check", **ctx)
    missing = [k for k in ("name", "addr", "mask", "expected") if k not in chk]
    if missing:
        raise _parse_err(f"Check is missing required property {missing[0]!r}.", **ctx)
    ctx["field"] = _parse_str(chk["name"], "name", **ctx, non_empty=True)
    addr = _parse_int(chk["addr"], "addr", **ctx, max_val=_U64_MAX)
    if addr % 4 != 0:
        raise _parse_err(f"Check address 0x{addr:X} must be 4-byte aligned.", **ctx)
    return CheckSpec(
        name=ctx["field"],
        addr=addr,
        mask=_parse_int(chk["mask"], "mask", **ctx, max_val=_U32_MAX),
        expected=_parse_int(chk["expected"], "expected", **ctx, max_val=_U32_MAX),
    )


def _parse_instance(inst_name: str, inst_obj: Mapping[str, Any], *, source: str) -> InstanceSpec:
    ctx: Dict[str, str] = {"source": source, "reg": inst_name}
    _check_keys(inst_obj, _INSTANCE_KEYS, f"instance '{inst_name}'", **ctx)
    if "block" not in inst_obj:
        raise _parse_err(f"Instance '{inst_name}' is missing required 'block' key.", **ctx)
    if ("uio" in inst_obj) == ("at" in inst_obj):
        raise _parse_err(f"Instance '{inst_name}' must specify exactly one of 'uio' or 'at'.", **ctx)
    if "uio" in inst_obj and "size" in inst_obj:
        raise _parse_err(f"Instance '{inst_name}' specifies 'size' with 'uio' ('size' is only for 'at').", **ctx)
    if "at" in inst_obj and "uio_map" in inst_obj:
        raise _parse_err(f"Instance '{inst_name}' specifies 'uio_map' without 'uio'.", **ctx)

    raw_checks = inst_obj.get("checks", [])
    if not isinstance(raw_checks, list):
        raise _parse_err(f"'checks' in instance '{inst_name}' must be a JSON array.", **ctx)
    return InstanceSpec(
        name=inst_name,
        block=_parse_str(inst_obj["block"], "block", **ctx, non_empty=True),
        desc=_parse_str(inst_obj.get("desc", ""), "desc", **ctx),
        uio=_parse_str(inst_obj["uio"], "uio", **ctx, non_empty=True) if "uio" in inst_obj else None,
        uio_map=_parse_int(inst_obj.get("uio_map", 0), "uio_map", **ctx, max_val=255),
        at=_parse_int(inst_obj["at"], "at", **ctx, max_val=_U64_MAX) if "at" in inst_obj else None,
        size=_parse_int(inst_obj["size"], "size", **ctx, min_val=1) if "size" in inst_obj else None,
        checks=tuple(_parse_check(c, i, source=source, inst_name=inst_name) for i, c in enumerate(raw_checks)),
        source=source,
    )


def parse_instance_dict(data: Mapping[str, Any], *, source: str = "<instances>") -> Dict[str, InstanceSpec]:
    if not isinstance(data, Mapping):
        raise _parse_err("Instance file data must be a JSON object.", source=source)
    _check_format(data.get("format"), "mmreg-instances", source=source)
    instances: Dict[str, InstanceSpec] = {}
    seen_upper: Dict[str, str] = {}
    for inst_name, inst_obj in _props(data, source=source):
        if inst_name == "format":
            continue
        _check_ident(inst_name, "Instance name", source=source, reg=inst_name)
        prev = seen_upper.setdefault(inst_name.upper(), inst_name)
        if prev != inst_name:
            raise _parse_err(
                f"Instance names {prev!r} and {inst_name!r} differ only by case.", source=source, reg=inst_name
            )
        if not isinstance(inst_obj, Mapping):
            raise _parse_err(f"Instance '{inst_name}' must be a JSON object.", source=source, reg=inst_name)
        instances[inst_name] = _parse_instance(inst_name, inst_obj, source=source)
    return instances


def load_instance_file(path: Union[str, Path]) -> Dict[str, InstanceSpec]:
    data, source = _read_json_file(path)
    return parse_instance_dict(data, source=source)


def merge_instances(into: Dict[str, InstanceSpec], instances: Mapping[str, InstanceSpec]) -> None:
    """Adds `instances` to `into`, keyed by upper-case name, rejecting names `into` already has."""
    for name, spec in instances.items():
        prev = into.setdefault(name.upper(), spec)
        if prev is not spec:
            raise _err(
                "E_INSTANCE_DUPLICATE",
                f"Duplicate instance '{name}' defined in both '{prev.source}' and '{spec.source}'.",
                source=spec.source,
                reg=name,
            )


def check_file(path: Union[str, Path]) -> Union[Block, Dict[str, InstanceSpec]]:
    """Validates a block or instance JSON file without touching hardware."""
    data, source = _read_json_file(path)
    if _check_format(data.get("format"), None, source=source) == "mmreg-block":
        return Block.from_dict(data, source=source)
    return parse_instance_dict(data, source=source)

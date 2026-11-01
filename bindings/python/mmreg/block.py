"""
`mmreg-block/1` and `mmreg-instances/1` loader, validator, name resolver, and bound register/field
access layer for `mmreg` (see `docs/users/07-block-files.md`).
"""

from __future__ import annotations

from collections.abc import Iterable, Iterator, Mapping, Sequence
from dataclasses import dataclass
import json
import operator
import os
from pathlib import Path
import re
from typing import Any, Callable, Dict, List, Optional, Set, Tuple, Union

from . import (
    Device,
    MmregError,
    _MmregAttributeError,
    _MmregOSError,
    _check_uint,
    _reject_private_attr,
)

SUPPORTED_BLOCK_MINOR = 0
SUPPORTED_INSTANCES_MINOR = 0

_IDENT_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
_INT_STR_RE = re.compile(
    r"^(?:0[xX][0-9a-fA-F](?:_?[0-9a-fA-F])*|0[bB][01](?:_?[01])*|[0-9](?:_?[0-9])*)$"
)
_FORMAT_RE = re.compile(r"^(mmreg-block|mmreg-instances)/([0-9]+)\.([0-9]+)$")

_VALID_WIDTHS = (8, 16, 32, 64)
_VALID_WORD_ORDERS = ("hi_first", "lo_first")
_VALID_SW = ("rw", "r", "w", "rw1", "w1", "na")
_VALID_ONWRITE = ("woclr", "woset", "wot", "wzc", "wzs", "wzt", "wclr", "wset")
_VALID_ONREAD = ("rclr", "rset")
_VALID_HW = ("r", "w", "rw", "na")

_FILE_SCALAR_KEYS = frozenset({"format", "compatible"})
_BLOCK_SCALAR_KEYS = frozenset({"desc", "name", "size", "regwidth", "accesswidth", "word_order"})
_GROUP_SCALAR_KEYS = frozenset(
    {"at", "desc", "name", "regwidth", "accesswidth", "word_order", "dim", "stride"}
)
_FIELD_DEFAULT_KEYS = frozenset(
    {"sw", "onwrite", "onread", "singlepulse", "hw", "reset", "encode"}
)
_REGISTER_SCALAR_KEYS = _GROUP_SCALAR_KEYS | _FIELD_DEFAULT_KEYS
_FIELD_SCALAR_KEYS = _FIELD_DEFAULT_KEYS | frozenset({"bits", "desc", "name"})
_ENUM_ENTRY_KEYS = frozenset({"value", "desc", "name"})
_INSTANCE_SCALAR_KEYS = frozenset({"block", "desc", "uio", "uio_map", "at", "size", "checks"})
_CHECK_SCALAR_KEYS = frozenset({"name", "addr", "mask", "expected"})


class _MmregIndexError(MmregError, IndexError):
    """Out-of-range register or group array index."""


def _err(
    code: str,
    detail: str,
    *,
    source: str,
    reg: str = "<block>",
    field: str = "<none>",
) -> MmregError:
    return MmregError(
        code,
        f"[{code}] {source} (register '{reg}', field '{field}'): {detail}",
    )


def _check_ident(
    name: str,
    what: str,
    *,
    source: str,
    reg: str = "<block>",
    field: str = "<none>",
) -> None:
    if not isinstance(name, str) or not _IDENT_RE.match(name):
        raise _err(
            "E_BLOCK_PARSE",
            f"{what} {name!r} must match [A-Za-z_][A-Za-z0-9_]*.",
            source=source,
            reg=reg,
            field=field,
        )


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
    if isinstance(val, bool):
        raise _err(
            "E_BLOCK_PARSE",
            f"{what} {val!r} must be an integer or numeric string, not bool.",
            source=source,
            reg=reg,
            field=field,
        )
    if isinstance(val, int):
        ival = val
    elif isinstance(val, str):
        text = val.strip()
        if not _INT_STR_RE.match(text):
            raise _err(
                "E_BLOCK_PARSE",
                f"{what} {val!r} is not a valid integer literal.",
                source=source,
                reg=reg,
                field=field,
            )
        ival = int(text, 0)
    else:
        raise _err(
            "E_BLOCK_PARSE",
            f"{what} {val!r} must be an integer or numeric string.",
            source=source,
            reg=reg,
            field=field,
        )

    if ival < min_val or (max_val is not None and ival > max_val):
        bound = f">= {min_val}" if max_val is None else f"in [{min_val}, 0x{max_val:X}]"
        raise _err(
            "E_BLOCK_PARSE",
            f"{what} {val!r} ({ival}) is out of range (expected {bound}).",
            source=source,
            reg=reg,
            field=field,
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
        raise _err(
            "E_BLOCK_PARSE",
            f"{what} must be {req}, got {val!r}.",
            source=source,
            reg=reg,
            field=field,
        )
    return val


def _check_format(
    raw_format: Any,
    expected_kind: Optional[str],
    *,
    source: str,
) -> str:
    if raw_format is None:
        raise _err("E_BLOCK_PARSE", "Missing required 'format' key at file root.", source=source)
    if not isinstance(raw_format, str):
        raise _err(
            "E_BLOCK_VERSION",
            f"'format' must be a string, got {raw_format!r} (supported: mmreg-block/1.0, mmreg-instances/1.0).",
            source=source,
        )
    m = _FORMAT_RE.match(raw_format)
    if not m:
        raise _err(
            "E_BLOCK_VERSION",
            f"Unsupported format {raw_format!r} (supported: mmreg-block/1.{SUPPORTED_BLOCK_MINOR}, "
            f"mmreg-instances/1.{SUPPORTED_INSTANCES_MINOR}).",
            source=source,
        )
    kind, major_s, minor_s = m.group(1), m.group(2), m.group(3)
    if expected_kind is not None and kind != expected_kind:
        raise _err(
            "E_BLOCK_VERSION",
            f"Expected '{expected_kind}/1.0', found {raw_format!r}.",
            source=source,
        )
    major, minor = int(major_s), int(minor_s)
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
    def _unique_pairs(pairs: List[Tuple[str, Any]]) -> Dict[str, Any]:
        out: Dict[str, Any] = {}
        for k, v in pairs:
            if k in out:
                raise _err("E_BLOCK_PARSE", f"Duplicate JSON key {k!r}.", source=source, reg=k)
            out[k] = v
        return out

    try:
        data = json.loads(text, object_pairs_hook=_unique_pairs)
    except MmregError:
        raise
    except json.JSONDecodeError as exc:
        raise _err(
            "E_BLOCK_PARSE",
            f"Invalid JSON at line {exc.lineno}, column {exc.colno}: {exc.msg}.",
            source=source,
        ) from exc
    if not isinstance(data, dict):
        raise _err("E_BLOCK_PARSE", "Top-level JSON value must be an object.", source=source)
    return data


def _read_json_file(path: Union[str, Path]) -> Tuple[Dict[str, Any], str]:
    p = Path(path)
    source = str(p)
    try:
        text = p.read_text(encoding="utf-8")
    except OSError as exc:
        raise _MmregOSError(
            "E_BLOCK_NOT_FOUND",
            f"[E_BLOCK_NOT_FOUND] {source} (register '<block>', field '<none>'): Cannot read file: {exc}",
        ) from exc
    return _load_json_text(text, source=source), source


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
        if isinstance(other, str):
            return self.name.upper() != other.strip().upper()
        return super().__ne__(other)

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
    entries_by_name: Dict[str, EnumEntry]
    entries_by_upper: Dict[str, EnumEntry]
    entries_by_value: Dict[int, EnumEntry]

    def encode(self, value: Union[int, str], *, source: str, reg: str, field: str) -> int:
        if isinstance(value, str):
            entry = self.entries_by_upper.get(value.strip().upper())
            if entry is None:
                valid = ", ".join(self.entries_by_name.keys())
                raise _err(
                    "E_INVALID_ARG",
                    f"Unknown enum value {value!r} for enumeration '{self.name}' (valid: [{valid}]).",
                    source=source,
                    reg=reg,
                    field=field,
                )
            return entry.value
        return value

    def decode(self, raw: int) -> Union[int, EnumValue]:
        entry = self.entries_by_value.get(raw)
        if entry is not None:
            return EnumValue(entry.value, entry.name, entry.desc)
        return raw


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
    rmw_clear_mask: int = 0
    rmw_set_mask: int = 0

    @property
    def is_readable(self) -> bool:
        if self.sw == "na":
            return False
        if self.sw in ("w", "w1") and not self.singlepulse:
            return False
        return True

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
    has_read_side_effect: bool
    onread_modes: Tuple[str, ...]
    all_na: bool
    is_readable: bool
    is_writable: bool
    can_field_rmw: bool
    rmw_refusal_reason: Optional[str]


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


@dataclass(frozen=True)
class FlatRegisterEntry:
    """One concrete register instance at its expanded byte offset (for overlap/bounds/dump)."""

    full_name: str
    offset: int
    spec: RegisterSpec


def _parse_enums(raw_enums: Any, *, source: str) -> Dict[str, EnumSpec]:
    if raw_enums is None:
        return {}
    if not isinstance(raw_enums, dict):
        raise _err("E_BLOCK_PARSE", "'enum' at file root must be a JSON object.", source=source)
    enums: Dict[str, EnumSpec] = {}
    seen_enum_upper: Dict[str, str] = {}
    for enum_name, raw_entries in raw_enums.items():
        if enum_name.startswith("x-"):
            continue
        _check_ident(enum_name, "Enumeration name", source=source, reg=enum_name)
        up_enum = enum_name.upper()
        if up_enum in seen_enum_upper:
            raise _err(
                "E_BLOCK_PARSE",
                f"Enumeration names {seen_enum_upper[up_enum]!r} and {enum_name!r} differ only by case.",
                source=source,
                reg=enum_name,
            )
        seen_enum_upper[up_enum] = enum_name
        if not isinstance(raw_entries, dict):
            raise _err(
                "E_BLOCK_PARSE",
                f"Enumeration '{enum_name}' must be a JSON object mapping value names to objects.",
                source=source,
                reg=enum_name,
            )
        by_name: Dict[str, EnumEntry] = {}
        by_upper: Dict[str, EnumEntry] = {}
        by_val: Dict[int, EnumEntry] = {}
        for entry_name, entry_obj in raw_entries.items():
            if entry_name.startswith("x-"):
                continue
            _check_ident(
                entry_name,
                f"Enum entry name in '{enum_name}'",
                source=source,
                reg=enum_name,
                field=entry_name,
            )
            up_entry = entry_name.upper()
            if up_entry in by_upper:
                 prev = by_upper[up_entry].name
                 raise _err(
                     "E_BLOCK_PARSE",
                     f"Enum entries {prev!r} and {entry_name!r} in '{enum_name}' differ only by case.",
                     source=source,
                     reg=enum_name,
                     field=entry_name,
                 )
            if not isinstance(entry_obj, dict):
                raise _err(
                    "E_BLOCK_PARSE",
                    f"Enum entry '{enum_name}.{entry_name}' must be a JSON object.",
                    source=source,
                    reg=enum_name,
                    field=entry_name,
                )
            for k in entry_obj:
                if not k.startswith("x-") and k not in _ENUM_ENTRY_KEYS:
                    raise _err(
                        "E_BLOCK_PARSE",
                        f"Unknown property {k!r} in enum entry '{enum_name}.{entry_name}'.",
                        source=source,
                        reg=enum_name,
                        field=entry_name,
                    )
            if "value" not in entry_obj:
                raise _err(
                    "E_BLOCK_PARSE",
                    f"Enum entry '{enum_name}.{entry_name}' is missing required 'value'.",
                    source=source,
                    reg=enum_name,
                    field=entry_name,
                )
            val = _parse_int(
                entry_obj["value"],
                f"Enum value for '{enum_name}.{entry_name}'",
                source=source,
                reg=enum_name,
                field=entry_name,
                min_val=0,
            )
            if val in by_val:
                raise _err(
                    "E_BLOCK_PARSE",
                    f"Duplicate enum value {val} in '{enum_name}' ('{by_val[val].name}' and '{entry_name}').",
                    source=source,
                    reg=enum_name,
                    field=entry_name,
                )
            desc = _parse_str(
                entry_obj.get("desc", ""),
                "desc",
                source=source,
                reg=enum_name,
                field=entry_name,
            )
            if "name" in entry_obj:
                _parse_str(entry_obj["name"], "name", source=source, reg=enum_name, field=entry_name)
            entry = EnumEntry(name=entry_name, value=val, desc=desc)
            by_name[entry_name] = entry
            by_upper[up_entry] = entry
            by_val[val] = entry
        if not by_name:
            raise _err(
                "E_BLOCK_PARSE",
                f"Enumeration '{enum_name}' must contain at least one value.",
                source=source,
                reg=enum_name,
            )
        enums[enum_name] = EnumSpec(
            name=enum_name,
            entries_by_name=by_name,
            entries_by_upper=by_upper,
            entries_by_value=by_val,
        )
    return enums


def _parse_widths_and_order(
    obj: Mapping[str, Any],
    *,
    default_regwidth: int,
    default_accesswidth: Optional[int],
    default_word_order: str,
    source: str,
    reg: str,
) -> Tuple[int, int, str]:
    if "regwidth" in obj:
        regwidth = _parse_int(obj["regwidth"], "regwidth", source=source, reg=reg, min_val=8, max_val=64)
        if regwidth not in _VALID_WIDTHS:
            raise _err(
                "E_BLOCK_PARSE",
                f"'regwidth' must be 8, 16, 32, or 64, got {regwidth}.",
                source=source,
                reg=reg,
            )
    else:
        regwidth = default_regwidth

    if "accesswidth" in obj:
        accesswidth = _parse_int(
            obj["accesswidth"], "accesswidth", source=source, reg=reg, min_val=8, max_val=64
        )
        if accesswidth not in _VALID_WIDTHS:
            raise _err(
                "E_BLOCK_PARSE",
                f"'accesswidth' must be 8, 16, 32, or 64, got {accesswidth}.",
                source=source,
                reg=reg,
            )
    elif default_accesswidth is not None and default_accesswidth <= regwidth:
        accesswidth = default_accesswidth
    else:
        accesswidth = regwidth

    if accesswidth > regwidth or regwidth % accesswidth != 0:
        raise _err(
            "E_BLOCK_PARSE",
            f"'accesswidth' ({accesswidth}) must divide 'regwidth' ({regwidth}) and not exceed it.",
            source=source,
            reg=reg,
        )

    if "word_order" in obj:
        word_order = _parse_str(obj["word_order"], "word_order", source=source, reg=reg)
        if word_order not in _VALID_WORD_ORDERS:
            raise _err(
                "E_BLOCK_PARSE",
                f"'word_order' must be one of {_VALID_WORD_ORDERS}, got {word_order!r}.",
                source=source,
                reg=reg,
            )
    else:
        word_order = default_word_order

    return regwidth, accesswidth, word_order


def _parse_bits_string(
    raw_bits: Any,
    regwidth: int,
    *,
    source: str,
    reg: str,
    field: str,
) -> Tuple[int, int]:
    if not isinstance(raw_bits, str) or not raw_bits.strip():
        raise _err(
            "E_BLOCK_PARSE",
            f"Field 'bits' must be a string (\"msb:lsb\" or \"n\"), got {raw_bits!r}.",
            source=source,
            reg=reg,
            field=field,
        )
    text = raw_bits.strip()
    if ":" in text:
        parts = text.split(":")
        if len(parts) != 2 or not parts[0].isdigit() or not parts[1].isdigit():
            raise _err(
                "E_BLOCK_PARSE",
                f"Invalid field 'bits' specification {raw_bits!r} (expected \"msb:lsb\" or \"n\").",
                source=source,
                reg=reg,
                field=field,
            )
        msb, lsb = int(parts[0], 10), int(parts[1], 10)
    else:
        if not text.isdigit():
            raise _err(
                "E_BLOCK_PARSE",
                f"Invalid field 'bits' specification {raw_bits!r} (expected \"msb:lsb\" or \"n\").",
                source=source,
                reg=reg,
                field=field,
            )
        msb = lsb = int(text, 10)

    if lsb < 0 or msb < lsb or msb >= regwidth:
        raise _err(
            "E_BLOCK_PARSE",
            f"Field bit range [{msb}:{lsb}] is outside register width ({regwidth} bits, valid 0..{regwidth - 1}) "
            "or has msb < lsb.",
            source=source,
            reg=reg,
            field=field,
        )
    return msb, lsb


def _parse_field_props(
    obj: Mapping[str, Any],
    *,
    defaults: Mapping[str, Any],
    msb: int,
    lsb: int,
    enums: Mapping[str, EnumSpec],
    source: str,
    reg: str,
    field: str,
    is_reg_level_default: bool = False,
) -> Tuple[str, Optional[str], Optional[str], bool, str, Optional[int], Optional[str], Optional[EnumSpec]]:
    width = msb - lsb + 1
    max_val = (1 << width) - 1

    raw_sw = obj.get("sw", defaults.get("sw", "rw"))
    sw = _parse_str(raw_sw, "sw", source=source, reg=reg, field=field)
    if sw not in _VALID_SW:
        raise _err(
            "E_BLOCK_PARSE",
            f"Invalid 'sw' value {sw!r} (expected one of {_VALID_SW}).",
            source=source,
            reg=reg,
            field=field,
        )

    raw_onwrite = obj.get("onwrite", defaults.get("onwrite"))
    if raw_onwrite is not None:
        onwrite: Optional[str] = _parse_str(raw_onwrite, "onwrite", source=source, reg=reg, field=field)
        if onwrite not in _VALID_ONWRITE:
            raise _err(
                "E_BLOCK_PARSE",
                f"Invalid 'onwrite' value {onwrite!r} (expected one of {_VALID_ONWRITE}).",
                source=source,
                reg=reg,
                field=field,
            )
    else:
        onwrite = None

    raw_onread = obj.get("onread", defaults.get("onread"))
    if raw_onread is not None:
        onread: Optional[str] = _parse_str(raw_onread, "onread", source=source, reg=reg, field=field)
        if onread not in _VALID_ONREAD:
            raise _err(
                "E_BLOCK_PARSE",
                f"Invalid 'onread' value {onread!r} (expected one of {_VALID_ONREAD}).",
                source=source,
                reg=reg,
                field=field,
            )
    else:
        onread = None

    raw_sp = obj.get("singlepulse", defaults.get("singlepulse", False))
    if not isinstance(raw_sp, bool):
        raise _err(
            "E_BLOCK_PARSE",
            f"'singlepulse' must be a boolean, got {raw_sp!r}.",
            source=source,
            reg=reg,
            field=field,
        )
    singlepulse = raw_sp
    if singlepulse and sw not in ("w", "rw"):
        raise _err(
            "E_BLOCK_PARSE",
            f"'singlepulse: true' is only valid with sw 'w' or 'rw', not {sw!r}.",
            source=source,
            reg=reg,
            field=field,
        )

    raw_hw = obj.get("hw", defaults.get("hw", "r"))
    hw = _parse_str(raw_hw, "hw", source=source, reg=reg, field=field)
    if hw not in _VALID_HW:
        raise _err(
            "E_BLOCK_PARSE",
            f"Invalid 'hw' value {hw!r} (expected one of {_VALID_HW}).",
            source=source,
            reg=reg,
            field=field,
        )

    if "reset" in obj:
        raw_reset = obj["reset"]
        if isinstance(raw_reset, str) and raw_reset.strip().lower() == "x":
            reset: Optional[int] = None
        else:
            reset = _parse_int(
                raw_reset,
                "reset",
                source=source,
                reg=reg,
                field=field,
                min_val=0,
                max_val=max_val,
            )
    elif "reset" in defaults and not is_reg_level_default:
        reg_reset = defaults["reset"]
        if reg_reset is None:
            reset = None
        else:
            reset = (int(reg_reset) >> lsb) & max_val
    else:
        reset = 0

    raw_encode = obj.get("encode", defaults.get("encode"))
    if raw_encode is not None:
        encode: Optional[str] = _parse_str(
            raw_encode, "encode", source=source, reg=reg, field=field, non_empty=True
        )
        enum_spec = enums.get(encode)
        if enum_spec is None:
            raise _err(
                "E_BLOCK_PARSE",
                f"Field references unknown enumeration {encode!r}.",
                source=source,
                reg=reg,
                field=field,
            )
        if not is_reg_level_default:
            for entry in enum_spec.entries_by_name.values():
                if entry.value > max_val:
                    raise _err(
                        "E_BLOCK_PARSE",
                        f"Enumeration '{encode}' entry '{entry.name}' value {entry.value} "
                        f"does not fit in {width}-bit field (max {max_val}).",
                        source=source,
                        reg=reg,
                        field=field,
                    )
    else:
        encode = None
        enum_spec = None

    return sw, onwrite, onread, singlepulse, hw, reset, encode, enum_spec


def _parse_register(
    name: str,
    full_name: str,
    obj: Mapping[str, Any],
    *,
    parent_offset: int,
    default_regwidth: int,
    default_accesswidth: int,
    default_word_order: str,
    enums: Mapping[str, EnumSpec],
    source: str,
) -> RegisterSpec:
    child_objs: Dict[str, Mapping[str, Any]] = {}
    seen_child_upper: Dict[str, str] = {}
    for k, v in obj.items():
        if k.startswith("x-"):
            continue
        if isinstance(v, dict):
            _check_ident(k, "Field name", source=source, reg=full_name, field=k)
            up = k.upper()
            if up in seen_child_upper:
                raise _err(
                    "E_BLOCK_PARSE",
                    f"Field names {seen_child_upper[up]!r} and {k!r} differ only by case.",
                    source=source,
                    reg=full_name,
                    field=k,
                )
            seen_child_upper[up] = k
            child_objs[k] = v
        elif k not in _REGISTER_SCALAR_KEYS:
            raise _err(
                "E_BLOCK_PARSE",
                f"Unknown register property {k!r}.",
                source=source,
                reg=full_name,
            )

    if "at" not in obj:
        raise _err("E_BLOCK_PARSE", "Register is missing required 'at' byte offset.", source=source, reg=full_name)

    regwidth, accesswidth, word_order = _parse_widths_and_order(
        obj,
        default_regwidth=default_regwidth,
        default_accesswidth=default_accesswidth,
        default_word_order=default_word_order,
        source=source,
        reg=full_name,
    )
    reg_bytes = regwidth // 8

    at = _parse_int(obj["at"], "at", source=source, reg=full_name, min_val=0)
    offset = parent_offset + at
    if offset % reg_bytes != 0:
        raise _err(
            "E_BLOCK_PARSE",
            f"Register offset 0x{offset:X} is not aligned to regwidth / 8 ({reg_bytes} bytes).",
            source=source,
            reg=full_name,
        )

    desc = _parse_str(obj.get("desc", ""), "desc", source=source, reg=full_name)
    display_name = _parse_str(obj.get("name", name), "name", source=source, reg=full_name)

    if "dim" in obj:
        dim: Optional[int] = _parse_int(obj["dim"], "dim", source=source, reg=full_name, min_val=1)
        stride = (
            _parse_int(obj["stride"], "stride", source=source, reg=full_name, min_val=reg_bytes)
            if "stride" in obj
            else reg_bytes
        )
        if stride % reg_bytes != 0:
            raise _err(
                "E_BLOCK_PARSE",
                f"Register 'stride' ({stride}) is not a multiple of regwidth / 8 ({reg_bytes} bytes).",
                source=source,
                reg=full_name,
            )
    else:
        if "stride" in obj:
            raise _err(
                "E_BLOCK_PARSE",
                "Register specifies 'stride' without 'dim'.",
                source=source,
                reg=full_name,
            )
        dim = None
        stride = reg_bytes

    # Validate register-level field defaults
    reg_sw, reg_onwrite, reg_onread, reg_sp, reg_hw, reg_reset, reg_encode, reg_enum = _parse_field_props(
        obj,
        defaults={},
        msb=regwidth - 1,
        lsb=0,
        enums=enums,
        source=source,
        reg=full_name,
        field="<register>",
        is_reg_level_default= bool(child_objs),
    )
    reg_defaults: Dict[str, Any] = {
        "sw": reg_sw,
        "onwrite": reg_onwrite,
        "onread": reg_onread,
        "singlepulse": reg_sp,
        "hw": reg_hw,
        "reset": reg_reset,
        "encode": reg_encode,
    }

    raw_fields: List[FieldSpec] = []
    used_mask = 0
    used_fields: List[FieldSpec] = []

    if child_objs:
        has_explicit_fields = True
        for fname, fobj in child_objs.items():
            for fk, fv in fobj.items():
                if fk.startswith("x-"):
                    continue
                if isinstance(fv, dict):
                    raise _err(
                        "E_BLOCK_PARSE",
                        f"Field cannot contain child object {fk!r}.",
                        source=source,
                        reg=full_name,
                        field=fname,
                    )
                if fk not in _FIELD_SCALAR_KEYS:
                    raise _err(
                        "E_BLOCK_PARSE",
                        f"Unknown field property {fk!r}.",
                        source=source,
                        reg=full_name,
                        field=fname,
                    )
            if "bits" not in fobj:
                raise _err(
                    "E_BLOCK_PARSE",
                    "Field is missing required 'bits' property.",
                    source=source,
                    reg=full_name,
                    field=fname,
                )
            msb, lsb = _parse_bits_string(
                fobj["bits"], regwidth, source=source, reg=full_name, field=fname
            )
            fwidth = msb - lsb + 1
            fmask = ((1 << fwidth) - 1) << lsb
            if fmask & used_mask:
                other_name = next(
                    (prev.name for prev in used_fields if prev.mask & fmask),
                    "<unknown>",
                )
                raise _err(
                    "E_BLOCK_PARSE",
                    f"Field [{msb}:{lsb}] overlaps field '{other_name}'.",
                    source=source,
                    reg=full_name,
                    field=fname,
                )
            used_mask |= fmask

            fdesc = _parse_str(fobj.get("desc", ""), "desc", source=source, reg=full_name, field=fname)
            fdisplay = _parse_str(fobj.get("name", fname), "name", source=source, reg=full_name, field=fname)
            fsw, fonwrite, fonread, fsp, fhw, freset, fencode, fenum = _parse_field_props(
                fobj,
                defaults=reg_defaults,
                msb=msb,
                lsb=lsb,
                enums=enums,
                source=source,
                reg=full_name,
                field=fname,
            )
            fspec = FieldSpec(
                name=fname,
                display_name=fdisplay,
                msb=msb,
                lsb=lsb,
                width=fwidth,
                mask=fmask,
                desc=fdesc,
                sw=fsw,
                onwrite=fonwrite,
                onread=fonread,
                singlepulse=fsp,
                hw=fhw,
                reset=freset,
                encode=fencode,
                enum_spec=fenum,
                implicit=False,
            )
            raw_fields.append(fspec)
            used_fields.append(fspec)
    else:
        has_explicit_fields = False
        fmask = (1 << regwidth) - 1
        raw_fields.append(
            FieldSpec(
                name=name,
                display_name=display_name,
                msb=regwidth - 1,
                lsb=0,
                width=regwidth,
                mask=fmask,
                desc=desc,
                sw=reg_sw,
                onwrite=reg_onwrite,
                onread=reg_onread,
                singlepulse=reg_sp,
                hw=reg_hw,
                reset=reg_reset,
                encode=reg_encode,
                enum_spec=reg_enum,
                implicit=True,
            )
        )

    # Precompute RMW neutral masks for each field (Rule 1 of Access Behaviour):
    # - wzc / wzs / wzt other fields are written as 1 (in rmw_set_mask)
    # - woclr / woset / wot / r / w / w1 / singlepulse other fields are written as 0 (in rmw_clear_mask)
    # - rw / rw1 / na / unmodeled bits keep the value read
    fields_dict: Dict[str, FieldSpec] = {}
    fields_upper: Dict[str, FieldSpec] = {}
    for f in raw_fields:
        clear_mask = f.mask
        set_mask = 0
        for other in raw_fields:
            if other.name == f.name:
                continue
            if other.onwrite in ("wzc", "wzs", "wzt"):
                set_mask |= other.mask
                clear_mask |= other.mask
            elif (
                other.onwrite in ("woclr", "woset", "wot")
                or other.sw in ("r", "w", "w1")
                or other.singlepulse
            ):
                clear_mask |= other.mask
        finalized = FieldSpec(
            name=f.name,
            display_name=f.display_name,
            msb=f.msb,
            lsb=f.lsb,
            width=f.width,
            mask=f.mask,
            desc=f.desc,
            sw=f.sw,
            onwrite=f.onwrite,
            onread=f.onread,
            singlepulse=f.singlepulse,
            hw=f.hw,
            reset=f.reset,
            encode=f.encode,
            enum_spec=f.enum_spec,
            implicit=f.implicit,
            rmw_clear_mask=clear_mask,
            rmw_set_mask=set_mask,
        )
        fields_dict[finalized.name] = finalized
        fields_upper[finalized.name.upper()] = finalized

    reset_val = 0
    reset_mask = 0
    for f in fields_dict.values():
        if f.is_readable and f.reset is not None:
            reset_mask |= f.mask
            reset_val |= (f.reset << f.lsb) & f.mask

    onread_modes = tuple(sorted({f.onread for f in fields_dict.values() if f.onread is not None}))
    has_read_side_effect = bool(onread_modes)
    all_na = all(f.sw == "na" for f in fields_dict.values())
    is_readable = any(f.is_readable for f in fields_dict.values())
    is_writable = any(f.is_writable for f in fields_dict.values())

    plain_wo = [f.name for f in fields_dict.values() if f.sw in ("w", "w1") and not f.singlepulse]
    wclr_wset = [f.name for f in fields_dict.values() if f.onwrite in ("wclr", "wset")]
    if wclr_wset:
        can_field_rmw = False
        rmw_refusal_reason: Optional[str] = (
            f"Register '{full_name}' contains write-clear/set field(s) {wclr_wset} and cannot be "
            f"read-modified-written; use a whole-register write (`d.{full_name} = value`)."
        )
    elif plain_wo:
        can_field_rmw = False
        rmw_refusal_reason = (
            f"Register '{full_name}' contains write-only field(s) {plain_wo} (without singlepulse) "
            f"whose value cannot be read back; use a whole-register write (`d.{full_name} = value`)."
        )
    else:
        can_field_rmw = True
        rmw_refusal_reason = None

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
        fields=fields_dict,
        fields_by_upper=fields_upper,
        has_explicit_fields=has_explicit_fields,
        reset_value=reset_val,
        reset_mask=reset_mask,
        has_read_side_effect=has_read_side_effect,
        onread_modes=onread_modes,
        all_na=all_na,
        is_readable=is_readable,
        is_writable=is_writable,
        can_field_rmw=can_field_rmw,
        rmw_refusal_reason=rmw_refusal_reason,
    )


def _classify_reg_or_group(
    name: str,
    obj: Mapping[str, Any],
    *,
    source: str,
) -> str:
    """Returns `'group'` or `'register'` based on the tell-tale keys (`at` vs `bits`)."""
    if "bits" in obj:
        raise _err(
            "E_BLOCK_PARSE",
            "Field object ('bits') is only valid inside a register, not directly in a block or group.",
            source=source,
            reg=name,
        )
    if "at" not in obj:
        raise _err(
            "E_BLOCK_PARSE",
            f"Object '{name}' must have 'at' (register or register group).",
            source=source,
            reg=name,
        )
    has_at_children = False
    has_bits_children = False
    for k, v in obj.items():
        if k.startswith("x-") or not isinstance(v, dict):
            continue
        if "at" in v and "bits" in v:
            raise _err(
                "E_BLOCK_PARSE",
                f"Child '{k}' cannot specify both 'at' and 'bits'.",
                source=source,
                reg=name,
                field=k,
            )
        if "at" in v:
            has_at_children = True
        elif "bits" in v:
            has_bits_children = True
        else:
            raise _err(
                "E_BLOCK_PARSE",
                f"Child object '{k}' must specify either 'at' (register) or 'bits' (field).",
                source=source,
                reg=name,
                field=k,
            )
    if has_at_children and has_bits_children:
        raise _err(
            "E_BLOCK_PARSE",
            f"Object '{name}' mixes 'at' children (registers) and 'bits' children (fields).",
            source=source,
            reg=name,
        )
    return "group" if has_at_children else "register"


def _child_span_bytes(child: Union[RegisterSpec, GroupSpec]) -> int:
    if isinstance(child, RegisterSpec):
        reg_bytes = child.regwidth // 8
        count = child.dim if child.dim is not None else 1
        return (count - 1) * child.stride + reg_bytes
    count = child.dim if child.dim is not None else 1
    return (count - 1) * child.stride + child.stride


def _parse_group(
    name: str,
    full_name: str,
    obj: Mapping[str, Any],
    *,
    parent_offset: int,
    default_regwidth: int,
    default_accesswidth: int,
    default_word_order: str,
    enums: Mapping[str, EnumSpec],
    source: str,
) -> GroupSpec:
    child_objs: Dict[str, Mapping[str, Any]] = {}
    seen_child_upper: Dict[str, str] = {}
    for k, v in obj.items():
        if k.startswith("x-"):
            continue
        if isinstance(v, dict):
            _check_ident(k, "Child name", source=source, reg=full_name, field=k)
            up = k.upper()
            if up in seen_child_upper:
                raise _err(
                    "E_BLOCK_PARSE",
                    f"Child names {seen_child_upper[up]!r} and {k!r} in group '{full_name}' differ only by case.",
                    source=source,
                    reg=full_name,
                    field=k,
                )
            seen_child_upper[up] = k
            child_objs[k] = v
        elif k not in _GROUP_SCALAR_KEYS:
            raise _err(
                "E_BLOCK_PARSE",
                f"Unknown register group property {k!r}.",
                source=source,
                reg=full_name,
            )

    regwidth, accesswidth, word_order = _parse_widths_and_order(
        obj,
        default_regwidth=default_regwidth,
        default_accesswidth=default_accesswidth,
        default_word_order=default_word_order,
        source=source,
        reg=full_name,
    )
    reg_bytes = regwidth // 8
    at = _parse_int(obj["at"], "at", source=source, reg=full_name, min_val=0)
    offset = parent_offset + at
    if offset % reg_bytes != 0:
        raise _err(
            "E_BLOCK_PARSE",
            f"Register group offset 0x{offset:X} is not aligned to regwidth / 8 ({reg_bytes} bytes).",
            source=source,
            reg=full_name,
        )

    desc = _parse_str(obj.get("desc", ""), "desc", source=source, reg=full_name)
    display_name = _parse_str(obj.get("name", name), "name", source=source, reg=full_name)

    children: Dict[str, Union[RegisterSpec, GroupSpec]] = {}
    children_by_upper: Dict[str, Union[RegisterSpec, GroupSpec]] = {}
    max_rel_end = 0

    for cname, cobj in child_objs.items():
        cfull = f"{full_name}.{cname}"
        kind = _classify_reg_or_group(cfull, cobj, source=source)
        if kind == "group":
            cspec: Union[RegisterSpec, GroupSpec] = _parse_group(
                cname,
                cfull,
                cobj,
                parent_offset=offset,
                default_regwidth=regwidth,
                default_accesswidth=accesswidth,
                default_word_order=word_order,
                enums=enums,
                source=source,
            )
        else:
            cspec = _parse_register(
                cname,
                cfull,
                cobj,
                parent_offset=offset,
                default_regwidth=regwidth,
                default_accesswidth=accesswidth,
                default_word_order=word_order,
                enums=enums,
                source=source,
            )
        children[cname] = cspec
        children_by_upper[cname.upper()] = cspec
        rel_end = cspec.at + _child_span_bytes(cspec)
        if rel_end > max_rel_end:
            max_rel_end = rel_end

    if "dim" in obj:
        dim: Optional[int] = _parse_int(obj["dim"], "dim", source=source, reg=full_name, min_val=1)
        stride = (
            _parse_int(obj["stride"], "stride", source=source, reg=full_name, min_val=max_rel_end)
            if "stride" in obj
            else max_rel_end
        )
        if stride % reg_bytes != 0:
            raise _err(
                "E_BLOCK_PARSE",
                f"Register group 'stride' ({stride}) is not a multiple of regwidth / 8 ({reg_bytes} bytes).",
                source=source,
                reg=full_name,
            )
    else:
        if "stride" in obj:
            raise _err(
                "E_BLOCK_PARSE",
                "Register group specifies 'stride' without 'dim'.",
                source=source,
                reg=full_name,
            )
        dim = None
        stride = max_rel_end

    return GroupSpec(
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
        children=children,
        children_by_upper=children_by_upper,
    )


def _expand_flat_registers(
    children: Mapping[str, Union[RegisterSpec, GroupSpec]],
    *,
    base_shift: int = 0,
    name_prefix: str = "",
) -> List[FlatRegisterEntry]:
    entries: List[FlatRegisterEntry] = []
    for child in children.values():
        prefix = f"{name_prefix}.{child.name}" if name_prefix else child.name
        if isinstance(child, RegisterSpec):
            if child.dim is None:
                entries.append(
                    FlatRegisterEntry(
                        full_name=prefix,
                        offset=child.offset + base_shift,
                        spec=child,
                    )
                )
            else:
                for idx in range(child.dim):
                    entries.append(
                        FlatRegisterEntry(
                            full_name=f"{prefix}[{idx}]",
                            offset=child.offset + base_shift + idx * child.stride,
                            spec=child,
                        )
                    )
        else:
            if child.dim is None:
                entries.extend(
                    _expand_flat_registers(
                        child.children,
                        base_shift=base_shift,
                        name_prefix=prefix,
                    )
                )
            else:
                for idx in range(child.dim):
                    entries.extend(
                        _expand_flat_registers(
                            child.children,
                            base_shift=base_shift + idx * child.stride,
                            name_prefix=f"{prefix}[{idx}]",
                        )
                    )
    return entries


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
            raise _err("E_BLOCK_PARSE", "Block data must be a mapping.", source=source)

        _check_format(data.get("format"), "mmreg-block", source=source)

        if "compatible" not in data:
            raise _err(
                "E_BLOCK_PARSE",
                "Missing required 'compatible' string at file root.",
                source=source,
            )
        compatible = _parse_str(data["compatible"], "compatible", source=source, non_empty=True)

        enums = _parse_enums(data.get("enum"), source=source)

        block_candidates: List[Tuple[str, Mapping[str, Any]]] = []
        for k, v in data.items():
            if k.startswith("x-") or k == "enum":
                continue
            if k in _FILE_SCALAR_KEYS:
                if isinstance(v, dict):
                    raise _err("E_BLOCK_PARSE", f"'{k}' must be a string, not an object.", source=source)
                continue
            if isinstance(v, dict):
                block_candidates.append((k, v))
            else:
                raise _err(
                    "E_BLOCK_PARSE",
                    f"Unknown file-level property {k!r}.",
                    source=source,
                )

        if len(block_candidates) != 1:
            raise _err(
                "E_BLOCK_PARSE",
                f"File must contain exactly one block object alongside 'format'/'compatible', "
                f"found {len(block_candidates)} ({[k for k, _ in block_candidates]}).",
                source=source,
            )

        block_key, block_obj = block_candidates[0]
        _check_ident(block_key, "Block key", source=source, reg=block_key)

        child_objs: Dict[str, Mapping[str, Any]] = {}
        seen_child_upper: Dict[str, str] = {}
        for k, v in block_obj.items():
            if k.startswith("x-"):
                continue
            if isinstance(v, dict):
                _check_ident(k, "Register or group name", source=source, reg=k)
                up = k.upper()
                if up in seen_child_upper:
                    raise _err(
                        "E_BLOCK_PARSE",
                        f"Sibling names {seen_child_upper[up]!r} and {k!r} differ only by case.",
                        source=source,
                        reg=k,
                    )
                seen_child_upper[up] = k
                child_objs[k] = v
            elif k not in _BLOCK_SCALAR_KEYS:
                raise _err(
                    "E_BLOCK_PARSE",
                    f"Unknown block property {k!r}.",
                    source=source,
                    reg=block_key,
                )

        if not child_objs:
            raise _err(
                "E_BLOCK_PARSE",
                f"Block '{block_key}' contains no registers.",
                source=source,
                reg=block_key,
            )

        regwidth, accesswidth, word_order = _parse_widths_and_order(
            block_obj,
            default_regwidth=32,
            default_accesswidth=None,
            default_word_order="hi_first",
            source=source,
            reg=block_key,
        )
        desc = _parse_str(block_obj.get("desc", ""), "desc", source=source, reg=block_key)
        display_name = _parse_str(
            block_obj.get("name", block_key), "name", source=source, reg=block_key
        )
        size = (
            _parse_int(block_obj["size"], "size", source=source, reg=block_key, min_val=1)
            if "size" in block_obj
            else None
        )

        children: Dict[str, Union[RegisterSpec, GroupSpec]] = {}
        children_by_upper: Dict[str, Union[RegisterSpec, GroupSpec]] = {}
        for cname, cobj in child_objs.items():
            kind = _classify_reg_or_group(cname, cobj, source=source)
            if kind == "group":
                cspec: Union[RegisterSpec, GroupSpec] = _parse_group(
                    cname,
                    cname,
                    cobj,
                    parent_offset=0,
                    default_regwidth=regwidth,
                    default_accesswidth=accesswidth,
                    default_word_order=word_order,
                    enums=enums,
                    source=source,
                )
            else:
                cspec = _parse_register(
                    cname,
                    cname,
                    cobj,
                    parent_offset=0,
                    default_regwidth=regwidth,
                    default_accesswidth=accesswidth,
                    default_word_order=word_order,
                    enums=enums,
                    source=source,
                )
            children[cname] = cspec
            children_by_upper[cname.upper()] = cspec

        flat = _expand_flat_registers(children)
        flat.sort(key=lambda e: (e.offset, e.full_name))

        # Check register overlaps and block size bounds across all expanded elements.
        max_end = 0
        for i, cur in enumerate(flat):
            cur_bytes = cur.spec.regwidth // 8
            cur_end = cur.offset + cur_bytes
            if size is not None and cur_end > size:
                raise _err(
                    "E_BLOCK_PARSE",
                    f"Register at 0x{cur.offset:X}..0x{cur_end:X} ({cur_bytes} bytes) "
                    f"exceeds block size 0x{size:X}.",
                    source=source,
                    reg=cur.full_name,
                )
            if i > 0:
                prev = flat[i - 1]
                prev_bytes = prev.spec.regwidth // 8
                prev_end = prev.offset + prev_bytes
                if cur.offset < prev_end:
                    raise _err(
                        "E_BLOCK_PARSE",
                        f"Register at 0x{cur.offset:X}..0x{cur_end:X} overlaps '{prev.full_name}' "
                        f"at 0x{prev.offset:X}..0x{prev_end:X}.",
                        source=source,
                        reg=cur.full_name,
                    )
            if cur_end > max_end:
                max_end = cur_end

        return cls(
            key=block_key,
            name=display_name,
            compatible=compatible,
            desc=desc,
            size=size,
            regwidth=regwidth,
            accesswidth=accesswidth,
            word_order=word_order,
            enums=enums,
            children=children,
            children_by_upper=children_by_upper,
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


def parse_instance_dict(
    data: Mapping[str, Any],
    *,
    source: str = "<instances>",
) -> Dict[str, InstanceSpec]:
    if not isinstance(data, Mapping):
        raise _err("E_BLOCK_PARSE", "Instance file data must be a JSON object.", source=source)

    _check_format(data.get("format"), "mmreg-instances", source=source)

    instances: Dict[str, InstanceSpec] = {}
    seen_upper: Dict[str, str] = {}

    for inst_name, inst_obj in data.items():
        if inst_name.startswith("x-") or inst_name == "format":
            continue
        _check_ident(inst_name, "Instance name", source=source, reg=inst_name)
        up = inst_name.upper()
        if up in seen_upper:
            raise _err(
                "E_BLOCK_PARSE",
                f"Instance names {seen_upper[up]!r} and {inst_name!r} differ only by case.",
                source=source,
                reg=inst_name,
            )
        seen_upper[up] = inst_name

        if not isinstance(inst_obj, dict):
            raise _err(
                "E_BLOCK_PARSE",
                f"Instance '{inst_name}' must be a JSON object.",
                source=source,
                reg=inst_name,
            )
        for k in inst_obj:
            if not k.startswith("x-") and k not in _INSTANCE_SCALAR_KEYS:
                raise _err(
                    "E_BLOCK_PARSE",
                    f"Unknown property {k!r} in instance '{inst_name}'.",
                    source=source,
                    reg=inst_name,
                )

        if "block" not in inst_obj:
            raise _err(
                "E_BLOCK_PARSE",
                f"Instance '{inst_name}' is missing required 'block' key.",
                source=source,
                reg=inst_name,
            )
        block = _parse_str(inst_obj["block"], "block", source=source, reg=inst_name, non_empty=True)
        desc = _parse_str(inst_obj.get("desc", ""), "desc", source=source, reg=inst_name)

        has_uio = "uio" in inst_obj
        has_at = "at" in inst_obj
        if has_uio == has_at:
            raise _err(
                "E_BLOCK_PARSE",
                f"Instance '{inst_name}' must specify exactly one of 'uio' or 'at'.",
                source=source,
                reg=inst_name,
            )

        if has_uio:
            uio: Optional[str] = _parse_str(
                inst_obj["uio"], "uio", source=source, reg=inst_name, non_empty=True
            )
            uio_map = (
                _parse_int(inst_obj["uio_map"], "uio_map", source=source, reg=inst_name, min_val=0, max_val=255)
                if "uio_map" in inst_obj
                else 0
            )
            if "size" in inst_obj:
                raise _err(
                    "E_BLOCK_PARSE",
                    f"Instance '{inst_name}' specifies 'size' with 'uio' ('size' is only for 'at').",
                    source=source,
                    reg=inst_name,
                )
            at: Optional[int] = None
            size: Optional[int] = None
        else:
            if "uio_map" in inst_obj:
                raise _err(
                    "E_BLOCK_PARSE",
                    f"Instance '{inst_name}' specifies 'uio_map' without 'uio'.",
                    source=source,
                    reg=inst_name,
                )
            uio = None
            uio_map = 0
            at = _parse_int(
                inst_obj["at"], "at", source=source, reg=inst_name, min_val=0, max_val=(1 << 64) - 1
            )
            size = (
                _parse_int(inst_obj["size"], "size", source=source, reg=inst_name, min_val=1)
                if "size" in inst_obj
                else None
            )

        checks_list: List[CheckSpec] = []
        if "checks" in inst_obj:
            raw_checks = inst_obj["checks"]
            if not isinstance(raw_checks, list):
                raise _err(
                    "E_BLOCK_PARSE",
                    f"'checks' in instance '{inst_name}' must be a JSON array.",
                    source=source,
                    reg=inst_name,
                )
            for idx, chk in enumerate(raw_checks):
                cfield = f"checks[{idx}]"
                if not isinstance(chk, dict):
                    raise _err(
                        "E_BLOCK_PARSE",
                        "Each entry in 'checks' must be a JSON object.",
                        source=source,
                        reg=inst_name,
                        field=cfield,
                    )
                for ck in chk:
                    if not ck.startswith("x-") and ck not in _CHECK_SCALAR_KEYS:
                        raise _err(
                            "E_BLOCK_PARSE",
                            f"Unknown property {ck!r} in check.",
                            source=source,
                            reg=inst_name,
                            field=cfield,
                        )
                for req in ("name", "addr", "mask", "expected"):
                    if req not in chk:
                        raise _err(
                            "E_BLOCK_PARSE",
                            f"Check is missing required property {req!r}.",
                            source=source,
                            reg=inst_name,
                            field=cfield,
                        )
                cname = _parse_str(
                    chk["name"], "name", source=source, reg=inst_name, field=cfield, non_empty=True
                )
                caddr = _parse_int(
                    chk["addr"],
                    "addr",
                    source=source,
                    reg=inst_name,
                    field=cname,
                    min_val=0,
                    max_val=(1 << 64) - 1,
                )
                if caddr % 4 != 0:
                    raise _err(
                        "E_BLOCK_PARSE",
                        f"Check address 0x{caddr:X} must be 4-byte aligned.",
                        source=source,
                        reg=inst_name,
                        field=cname,
                    )
                cmask = _parse_int(
                    chk["mask"],
                    "mask",
                    source=source,
                    reg=inst_name,
                    field=cname,
                    min_val=0,
                    max_val=0xFFFF_FFFF,
                )
                cexpected = _parse_int(
                    chk["expected"],
                    "expected",
                    source=source,
                    reg=inst_name,
                    field=cname,
                    min_val=0,
                    max_val=0xFFFF_FFFF,
                )
                checks_list.append(CheckSpec(name=cname, addr=caddr, mask=cmask, expected=cexpected))

        instances[inst_name] = InstanceSpec(
            name=inst_name,
            block=block,
            desc=desc,
            uio=uio,
            uio_map=uio_map,
            at=at,
            size=size,
            checks=tuple(checks_list),
            source=source,
        )
    return instances


def load_instance_file(path: Union[str, Path]) -> Dict[str, InstanceSpec]:
    data, source = _read_json_file(path)
    return parse_instance_dict(data, source=source)


def check_file(path: Union[str, Path]) -> Union[Block, Dict[str, InstanceSpec]]:
    """Validates a block or instance JSON file without touching hardware."""
    data, source = _read_json_file(path)
    kind = _check_format(data.get("format"), None, source=source)
    if kind == "mmreg-block":
        return Block.from_dict(data, source=source)
    return parse_instance_dict(data, source=source)


# ---------------------------------------------------------------------------
# Runtime bound register, field, array, group, and dump objects
# ---------------------------------------------------------------------------


def _format_hex_value(value: int, regwidth: int) -> str:
    if regwidth == 8:
        return f"0x{value & 0xFF:02x}"
    if regwidth == 16:
        return f"0x{value & 0xFFFF:04x}"
    if regwidth == 32:
        v = value & 0xFFFF_FFFF
        return f"0x{(v >> 16) & 0xFFFF:04x}_{v & 0xFFFF:04x}"
    v = value & ((1 << 64) - 1)
    return (
        f"0x{(v >> 48) & 0xFFFF:04x}_{(v >> 32) & 0xFFFF:04x}_"
        f"{(v >> 16) & 0xFFFF:04x}_{v & 0xFFFF:04x}"
    )


def _format_offset(offset: int) -> str:
    return f"0x{offset:02x}" if offset < 0x100 else f"0x{offset:04x}"


class BlockField:
    """Bound field of a `BlockRegister`, enforcing `sw`, `onwrite`, `onread`, `singlepulse`, and `encode`."""

    def __init__(self, register: BlockRegister, spec: FieldSpec) -> None:
        self._register = register
        self._spec = spec
        self.name = spec.name
        self.reg_name = register.name
        self.offset = register.offset
        self.msb = spec.msb
        self.lsb = spec.lsb
        self.width = spec.width
        self.mask = spec.mask
        self.sw = spec.sw
        self.onwrite = spec.onwrite
        self.onread = spec.onread
        self.singlepulse = spec.singlepulse
        self.hw = spec.hw
        self.reset = spec.reset
        self.encode = spec.encode
        self.desc = spec.desc
        self.w1c = spec.onwrite == "woclr"

    def read(self, *, side_effects: bool = False) -> Union[int, EnumValue]:
        """Reads this field's value (returning an `EnumValue` when an enum matches)."""
        spec = self._spec
        reg = self._register
        source = reg._source
        if spec.sw == "na":
            raise _err(
                "E_ACCESS_DENIED",
                f"Field '{spec.name}' has sw='na' (reserved) and cannot be read.",
                source=source,
                reg=reg.name,
                field=spec.name,
            )
        if spec.sw in ("w", "w1") and not spec.singlepulse:
            raise _err(
                "E_WRITE_ONLY",
                f"Field '{spec.name}' has sw={spec.sw!r} (write-only) and cannot be read.",
                source=source,
                reg=reg.name,
                field=spec.name,
            )
        if spec.sw == "w" and spec.singlepulse:
            # Per spec table: sw: "w", singlepulse reads back 0.
            return spec.enum_spec.decode(0) if spec.enum_spec is not None else 0

        raw_reg = int(reg.read(side_effects=side_effects))
        raw_field = (raw_reg & spec.mask) >> spec.lsb
        if spec.enum_spec is not None:
            return spec.enum_spec.decode(raw_field)
        return raw_field

    def write(self, value: Union[int, str], *, side_effects: bool = False) -> int:
        """Writes `value` (integer or enum name) to this field via a masked RMW under the device lock."""
        spec = self._spec
        reg = self._register
        source = reg._source
        if spec.sw == "na":
            raise _err(
                "E_ACCESS_DENIED",
                f"Field '{spec.name}' has sw='na' (reserved) and cannot be written.",
                source=source,
                reg=reg.name,
                field=spec.name,
            )
        if spec.sw == "r":
            raise _err(
                "E_READ_ONLY",
                f"Field '{spec.name}' has sw='r' (read-only) and cannot be written.",
                source=source,
                reg=reg.name,
                field=spec.name,
            )

        if isinstance(value, str):
            if spec.enum_spec is None:
                raise _err(
                    "E_INVALID_ARG",
                    f"Field '{spec.name}' has no enumeration ('encode'), cannot write string {value!r}.",
                    source=source,
                    reg=reg.name,
                    field=spec.name,
                )
            ival = spec.enum_spec.encode(value, source=source, reg=reg.name, field=spec.name)
        else:
            ival = _check_uint(
                value,
                spec.width,
                f"Field {reg.name}.{spec.name} value",
                allow_bool=spec.width == 1,
            )

        # If this is the sole implicit full-width field, write the register directly.
        if spec.implicit:
            reg._write_raw(ival)
            return ival

        if not reg._spec.can_field_rmw:
            code = (
                "E_WRITE_ONLY"
                if any(f.sw in ("w", "w1") and not f.singlepulse for f in reg._spec.fields.values())
                else "E_RMW_UNSAFE"
            )
            raise _err(
                code,
                reg._spec.rmw_refusal_reason or "Register cannot be read-modified-written.",
                source=source,
                reg=reg.name,
                field=spec.name,
            )

        if reg._spec.has_read_side_effect and not side_effects:
            modes = ", ".join(reg._spec.onread_modes)
            raise _err(
                "E_READ_SIDE_EFFECT",
                f"Field write on register '{reg.name}' requires reading a register with onread={modes}; "
                f"call `{reg.name}.{spec.name}.write(value, side_effects=True)` or write the whole register.",
                source=source,
                reg=reg.name,
                field=spec.name,
            )

        shifted = (ival << spec.lsb) & spec.mask
        with reg._device.lock():
            raw = reg._read_raw()
            new_raw = (raw & ~spec.rmw_clear_mask) | spec.rmw_set_mask | shifted
            reg._write_raw(new_raw)
        return new_raw

    def __int__(self) -> int:
        return int(self.read())

    def __index__(self) -> int:
        return int(self.read())

    def __str__(self) -> str:
        return str(self.read())

    def __format__(self, format_spec: str) -> str:
        if not format_spec:
            return str(self)
        val = self.read()
        if format_spec.endswith("s"):
            return format(str(val), format_spec)
        return format(int(val), format_spec)

    def __eq__(self, other: object) -> bool:
        if isinstance(other, BlockField):
            return int(self) == int(other)
        if isinstance(other, (int, str)):
            return self.read() == other
        return NotImplemented

    def __ne__(self, other: object) -> bool:
        if isinstance(other, BlockField):
            return int(self) != int(other)
        if isinstance(other, (int, str)):
            return self.read() != other
        return NotImplemented

    __hash__ = None  # type: ignore[assignment]

    def __repr__(self) -> str:
        bits = f"{self.msb}:{self.lsb}" if self.msb != self.lsb else f"{self.lsb}"
        extras = [f"sw={self.sw}"]
        if self.onwrite:
            extras.append(f"onwrite={self.onwrite}")
        if self.onread:
            extras.append(f"onread={self.onread}")
        if self.singlepulse:
            extras.append("singlepulse")
        if self.encode:
            extras.append(f"encode={self.encode}")
        return f"<BlockField {self.reg_name}.{self.name}[{bits}] {' '.join(extras)} @ +0x{self.offset:X}>"


class BlockRegister:
    """Bound register on a `Device` from an `mmreg-block/1` specification."""

    def __init__(
        self,
        device: Device,
        spec: RegisterSpec,
        *,
        offset: Optional[int] = None,
        name: Optional[str] = None,
        source: str = "<block>",
    ) -> None:
        reg_offset = spec.offset if offset is None else offset
        reg_name = spec.full_name if name is None else name
        object.__setattr__(self, "_device", device)
        object.__setattr__(self, "_spec", spec)
        object.__setattr__(self, "_source", source)
        object.__setattr__(self, "name", reg_name)
        object.__setattr__(self, "offset", reg_offset)
        object.__setattr__(self, "at", reg_offset)
        object.__setattr__(self, "regwidth", spec.regwidth)
        object.__setattr__(self, "accesswidth", spec.accesswidth)
        object.__setattr__(self, "word_order", spec.word_order)
        object.__setattr__(self, "reset", spec.reset_value if spec.reset_mask else None)
        object.__setattr__(self, "desc", spec.desc)

        bound_fields: Dict[str, BlockField] = {}
        bound_upper: Dict[str, BlockField] = {}
        for fspec in spec.fields.values():
            bf = BlockField(self, fspec)
            bound_fields[fspec.name] = bf
            bound_upper[fspec.name.upper()] = bf
        object.__setattr__(self, "_fields", bound_fields)
        object.__setattr__(self, "_fields_upper", bound_upper)

    def _read_word(self, word_offset: int) -> int:
        aw = self._spec.accesswidth
        dev = self._device
        if aw == 8:
            return dev.read_u8(word_offset)
        if aw == 16:
            return dev.read_u16(word_offset)
        if aw == 32:
            return dev.read_u32(word_offset)
        return dev.read_u64(word_offset)

    def _write_word(self, word_offset: int, word_val: int) -> None:
        aw = self._spec.accesswidth
        dev = self._device
        if aw == 8:
            dev.write_u8(word_offset, word_val)
        elif aw == 16:
            dev.write_u16(word_offset, word_val)
        elif aw == 32:
            dev.write_u32(word_offset, word_val)
        else:
            dev.write_u64(word_offset, word_val)

    def _read_raw(self) -> int:
        spec = self._spec
        if spec.regwidth == spec.accesswidth:
            return self._read_word(self.offset)
        n_words = spec.regwidth // spec.accesswidth
        word_bytes = spec.accesswidth // 8
        value = 0
        with self._device.lock():
            for idx in range(n_words):
                w = self._read_word(self.offset + idx * word_bytes)
                value |= w << (idx * spec.accesswidth)
        return value

    def _write_raw(self, value: int) -> None:
        spec = self._spec
        checked = _check_uint(
            value,
            spec.regwidth,
            f"Register {self.name} value",
            allow_bool=spec.regwidth == 1,
        )
        if spec.regwidth == spec.accesswidth:
            self._write_word(self.offset, checked)
            return
        n_words = spec.regwidth // spec.accesswidth
        word_bytes = spec.accesswidth // 8
        word_mask = (1 << spec.accesswidth) - 1
        indices = range(n_words) if spec.word_order == "lo_first" else range(n_words - 1, -1, -1)
        with self._device.lock():
            for idx in indices:
                word_val = (checked >> (idx * spec.accesswidth)) & word_mask
                self._write_word(self.offset + idx * word_bytes, word_val)

    def read(self, *, side_effects: bool = False) -> Union[int, EnumValue]:
        """Reads the register value (requiring `side_effects=True` if `onread` is `rclr`/`rset`)."""
        spec = self._spec
        if spec.all_na:
            raise _err(
                "E_ACCESS_DENIED",
                f"Register '{self.name}' has sw='na' on all fields and cannot be read.",
                source=self._source,
                reg=self.name,
            )
        if not spec.is_readable:
            raise _err(
                "E_WRITE_ONLY",
                f"Register '{self.name}' is write-only (all fields have sw='w'/'w1') and cannot be read.",
                source=self._source,
                reg=self.name,
            )
        if spec.has_read_side_effect and not side_effects:
            modes = ", ".join(spec.onread_modes)
            raise _err(
                "E_READ_SIDE_EFFECT",
                f"Reading register '{self.name}' has hardware side effects (onread={modes}); "
                f"pass `side_effects=True` (`d.{self.name}.read(side_effects=True)`).",
                source=self._source,
                reg=self.name,
            )
        # Single implicit field with sw: "w" and singlepulse: true reads 0
        if not spec.has_explicit_fields:
            only = next(iter(spec.fields.values()))
            if only.sw == "w" and only.singlepulse:
                return only.enum_spec.decode(0) if only.enum_spec is not None else 0
            raw = self._read_raw()
            if only.enum_spec is not None:
                return only.enum_spec.decode(raw)
            return raw
        return self._read_raw()

    def write(self, value: Union[int, str]) -> None:
        """Performs a whole-register write to hardware."""
        spec = self._spec
        if spec.all_na:
            raise _err(
                "E_ACCESS_DENIED",
                f"Register '{self.name}' has sw='na' on all fields and cannot be written.",
                source=self._source,
                reg=self.name,
            )
        if not spec.is_writable:
            raise _err(
                "E_READ_ONLY",
                f"Register '{self.name}' is read-only (all fields have sw='r') and cannot be written.",
                source=self._source,
                reg=self.name,
            )
        if isinstance(value, str):
            if not spec.has_explicit_fields:
                only = next(iter(spec.fields.values()))
                if only.enum_spec is not None:
                    ival = only.enum_spec.encode(
                        value, source=self._source, reg=self.name, field=only.name
                    )
                    self._write_raw(ival)
                    return
            raise _err(
                "E_INVALID_ARG",
                f"Register '{self.name}' does not have a register-wide enumeration for {value!r}.",
                source=self._source,
                reg=self.name,
            )
        self._write_raw(value)

    def _format_decoded(self, raw_val: int) -> str:
        spec = self._spec
        hex_str = _format_hex_value(raw_val, spec.regwidth)
        off_str = _format_offset(self.offset)
        if not spec.has_explicit_fields:
            only = next(iter(spec.fields.values()))
            if only.enum_spec is not None:
                decoded = only.enum_spec.decode(raw_val)
                if isinstance(decoded, EnumValue):
                    return f"{self.name} @ {off_str} = {hex_str}  ({decoded.name})"
            return f"{self.name} @ {off_str} = {hex_str}"

        parts: List[str] = []
        for fspec in spec.fields.values():
            if fspec.sw == "na":
                continue
            if fspec.sw in ("w", "w1") and not fspec.singlepulse:
                parts.append(f"{fspec.name}=-")
                continue
            if fspec.sw == "w" and fspec.singlepulse:
                fval = 0
            else:
                fval = (raw_val & fspec.mask) >> fspec.lsb
            if fspec.enum_spec is not None:
                dec = fspec.enum_spec.decode(fval)
                if isinstance(dec, EnumValue):
                    parts.append(f"{fspec.name}={dec.name}")
                else:
                    parts.append(f"{fspec.name}=0x{fval:x}")
            elif fval < 10 or fspec.width == 1:
                parts.append(f"{fspec.name}={fval}")
            else:
                parts.append(f"{fspec.name}=0x{fval:x}")
        joined = ", ".join(parts)
        return f"{self.name} @ {off_str} = {hex_str}  ({joined})"

    def format(self, *, side_effects: bool = False) -> str:
        """Reads the register and formats it with decoded fields."""
        raw = int(self.read(side_effects=side_effects))
        return self._format_decoded(raw)

    def _field(self, key: str) -> BlockField:
        fields_upper: Optional[Dict[str, BlockField]] = self.__dict__.get("_fields_upper")
        if fields_upper is None:
            raise _MmregAttributeError(
                "E_SUBREG_NOT_FOUND",
                f"[E_SUBREG_NOT_FOUND] '{type(self).__name__}' object has no attribute '{key}'",
            )
        if isinstance(key, str):
            found = fields_upper.get(key.upper())
            if found is not None and self._spec.has_explicit_fields:
                return found
        avail = (
            ", ".join(self._fields.keys()) if self._spec.has_explicit_fields else "<none>"
        )
        raise _MmregAttributeError(
            "E_SUBREG_NOT_FOUND",
            f"[E_SUBREG_NOT_FOUND] {self._source} (register '{self.name}', field '{key}'): "
            f"Field '{key}' not found in register '{self.name}'. Available: [{avail}]",
        )

    def __getattr__(self, item: str) -> BlockField:
        _reject_private_attr(self, item)
        return self._field(item)

    def __setattr__(self, key: str, value: object) -> None:
        if key.startswith("_"):
            object.__setattr__(self, key, value)
            return
        field = self._field(key)
        if isinstance(value, BlockField):
            return
        field.write(value)  # type: ignore[arg-type]

    def __getitem__(self, key: str) -> BlockField:
        return self._field(key)

    def __setitem__(self, key: str, value: Union[int, str]) -> None:
        self._field(key).write(value)

    def __int__(self) -> int:
        return int(self.read())

    def __index__(self) -> int:
        return int(self.read())

    def __str__(self) -> str:
        return self.format(side_effects=False)

    def __format__(self, format_spec: str) -> str:
        if not format_spec:
            return str(self)
        val = self.read()
        if format_spec.endswith("s"):
            return format(str(val), format_spec)
        return format(int(val), format_spec)

    def __eq__(self, other: object) -> bool:
        if isinstance(other, BlockRegister):
            return int(self) == int(other)
        if isinstance(other, (int, str)):
            return self.read() == other
        return NotImplemented

    def __ne__(self, other: object) -> bool:
        if isinstance(other, BlockRegister):
            return int(self) != int(other)
        if isinstance(other, (int, str)):
            return self.read() != other
        return NotImplemented

    __hash__ = None  # type: ignore[assignment]

    def __repr__(self) -> str:
        if self._spec.has_explicit_fields:
            fnames = ", ".join(self._fields.keys())
            return f"<BlockRegister {self.name} @ +0x{self.offset:X} fields=[{fnames}]>"
        return f"<BlockRegister {self.name} @ +0x{self.offset:X} ({self.regwidth}-bit)>"


class BlockRegisterArray:
    """Bound array (`dim`) of `BlockRegister` instances (`d.DEBOUNCE[3]`)."""

    def __init__(
        self,
        device: Device,
        spec: RegisterSpec,
        *,
        base_offset: Optional[int] = None,
        name_prefix: Optional[str] = None,
        source: str = "<block>",
    ) -> None:
        assert spec.dim is not None
        base = spec.offset if base_offset is None else base_offset
        prefix = spec.full_name if name_prefix is None else name_prefix
        self._device = device
        self._spec = spec
        self._source = source
        self.name = prefix
        self.offset = base
        self.stride = spec.stride
        self._elements: Tuple[BlockRegister, ...] = tuple(
            BlockRegister(
                device,
                spec,
                offset=base + idx * spec.stride,
                name=f"{prefix}[{idx}]",
                source=source,
            )
            for idx in range(spec.dim)
        )

    def __len__(self) -> int:
        return len(self._elements)

    def __iter__(self) -> Iterator[BlockRegister]:
        return iter(self._elements)

    def _index(self, idx: int) -> int:
        if isinstance(idx, bool):
            raise _MmregIndexError(
                "E_INVALID_ARG",
                f"[E_INVALID_ARG] {self._source} (register '{self.name}', field '<none>'): "
                f"Array index must be an integer, not bool.",
            )
        try:
            i = operator.index(idx)
        except TypeError as exc:
            raise _MmregIndexError(
                "E_INVALID_ARG",
                f"[E_INVALID_ARG] {self._source} (register '{self.name}', field '<none>'): "
                f"Array index {idx!r} is not an integer.",
            ) from exc
        if i < 0 or i >= len(self._elements):
            raise _MmregIndexError(
                "E_OUT_OF_BOUNDS",
                f"[E_OUT_OF_BOUNDS] {self._source} (register '{self.name}', field '<none>'): "
                f"Index {i} out of range for '{self.name}' (dim={len(self._elements)}).",
            )
        return i

    def __getitem__(self, idx: int) -> BlockRegister:
        return self._elements[self._index(idx)]

    def __setitem__(self, idx: int, value: Union[int, str]) -> None:
        elem = self._elements[self._index(idx)]
        if isinstance(value, BlockRegister):
            return
        elem.write(value)

    def __repr__(self) -> str:
        return (
            f"<BlockRegisterArray {self.name}[0..{len(self._elements) - 1}] "
            f"@ +0x{self.offset:X} stride=0x{self.stride:X}>"
        )


BoundChild = Union[BlockRegister, BlockRegisterArray, "BlockRegisterGroup", "BlockRegisterGroupArray"]


def _bind_child(
    device: Device,
    spec: Union[RegisterSpec, GroupSpec],
    *,
    base_shift: int = 0,
    name_prefix: str = "",
    source: str = "<block>",
) -> BoundChild:
    full_name = f"{name_prefix}.{spec.name}" if name_prefix else spec.name
    offset = spec.offset + base_shift
    if isinstance(spec, RegisterSpec):
        if spec.dim is not None:
            return BlockRegisterArray(
                device, spec, base_offset=offset, name_prefix=full_name, source=source
            )
        return BlockRegister(device, spec, offset=offset, name=full_name, source=source)
    if spec.dim is not None:
        return BlockRegisterGroupArray(
            device, spec, base_shift=base_shift, name_prefix=full_name, source=source
        )
    return BlockRegisterGroup(device, spec, base_shift=base_shift, name_prefix=full_name, source=source)


class BlockRegisterGroup:
    """Bound register group on a `Device` (`d.CH.CTRL`)."""

    def __init__(
        self,
        device: Device,
        spec: GroupSpec,
        *,
        base_shift: int = 0,
        name_prefix: Optional[str] = None,
        source: str = "<block>",
    ) -> None:
        prefix = spec.full_name if name_prefix is None else name_prefix
        object.__setattr__(self, "_device", device)
        object.__setattr__(self, "_spec", spec)
        object.__setattr__(self, "_source", source)
        object.__setattr__(self, "name", prefix)
        object.__setattr__(self, "offset", spec.offset + base_shift)
        object.__setattr__(self, "at", spec.offset + base_shift)
        object.__setattr__(self, "desc", spec.desc)

        children: Dict[str, BoundChild] = {}
        children_upper: Dict[str, BoundChild] = {}
        for cspec in spec.children.values():
            bound = _bind_child(
                device, cspec, base_shift=base_shift, name_prefix=prefix, source=source
            )
            children[cspec.name] = bound
            children_upper[cspec.name.upper()] = bound
        object.__setattr__(self, "_children", children)
        object.__setattr__(self, "_children_upper", children_upper)

    def _child(self, key: str) -> BoundChild:
        children_upper: Optional[Dict[str, BoundChild]] = self.__dict__.get("_children_upper")
        if children_upper is None:
            raise _MmregAttributeError(
                "E_REG_NOT_FOUND",
                f"[E_REG_NOT_FOUND] '{type(self).__name__}' object has no attribute '{key}'",
            )
        if isinstance(key, str):
            found = children_upper.get(key.upper())
            if found is not None:
                return found
        avail = ", ".join(self._children.keys())
        raise _MmregAttributeError(
            "E_REG_NOT_FOUND",
            f"[E_REG_NOT_FOUND] {self._source} (register '{self.name}.{key}', field '<none>'): "
            f"Register '{key}' not found in group '{self.name}'. Available: [{avail}]",
        )

    def __getattr__(self, item: str) -> BoundChild:
        _reject_private_attr(self, item)
        return self._child(item)

    def __setattr__(self, key: str, value: object) -> None:
        if key.startswith("_"):
            object.__setattr__(self, key, value)
            return
        child = self._child(key)
        if isinstance(child, BlockRegister):
            if isinstance(value, BlockRegister):
                return
            child.write(value)  # type: ignore[arg-type]
            return
        raise _err(
            "E_INVALID_ARG",
            f"Cannot assign directly to group or array '{self.name}.{key}'.",
            source=self._source,
            reg=f"{self.name}.{key}",
        )

    def __getitem__(self, key: str) -> BoundChild:
        return self._child(key)

    def __setitem__(self, key: str, value: Union[int, str]) -> None:
        child = self._child(key)
        if not isinstance(child, BlockRegister):
            raise _err(
                "E_INVALID_ARG",
                f"Cannot assign directly to group or array '{self.name}.{key}'.",
                source=self._source,
                reg=f"{self.name}.{key}",
            )
        child.write(value)

    def __repr__(self) -> str:
        cnames = ", ".join(self._children.keys())
        return f"<BlockRegisterGroup {self.name} @ +0x{self.offset:X} children=[{cnames}]>"


class BlockRegisterGroupArray:
    """Bound array (`dim`) of `BlockRegisterGroup` instances (`d.CH[0].CTRL`)."""

    def __init__(
        self,
        device: Device,
        spec: GroupSpec,
        *,
        base_shift: int = 0,
        name_prefix: Optional[str] = None,
        source: str = "<block>",
    ) -> None:
        assert spec.dim is not None
        prefix = spec.full_name if name_prefix is None else name_prefix
        self.name = prefix
        self.offset = spec.offset + base_shift
        self.stride = spec.stride
        self._source = source
        self._elements: Tuple[BlockRegisterGroup, ...] = tuple(
            BlockRegisterGroup(
                device,
                spec,
                base_shift=base_shift + idx * spec.stride,
                name_prefix=f"{prefix}[{idx}]",
                source=source,
            )
            for idx in range(spec.dim)
        )

    def __len__(self) -> int:
        return len(self._elements)

    def __iter__(self) -> Iterator[BlockRegisterGroup]:
        return iter(self._elements)

    def __getitem__(self, idx: int) -> BlockRegisterGroup:
        if isinstance(idx, bool):
            raise _MmregIndexError(
                "E_INVALID_ARG",
                f"[E_INVALID_ARG] {self._source} (register '{self.name}', field '<none>'): "
                f"Group index must be an integer, not bool.",
            )
        try:
            i = operator.index(idx)
        except TypeError as exc:
            raise _MmregIndexError(
                "E_INVALID_ARG",
                f"[E_INVALID_ARG] {self._source} (register '{self.name}', field '<none>'): "
                f"Group index {idx!r} is not an integer.",
            ) from exc
        if i < 0 or i >= len(self._elements):
            raise _MmregIndexError(
                "E_OUT_OF_BOUNDS",
                f"[E_OUT_OF_BOUNDS] {self._source} (register '{self.name}', field '<none>'): "
                f"Index {i} out of range for group '{self.name}' (dim={len(self._elements)}).",
            )
        return self._elements[i]

    def __repr__(self) -> str:
        return (
            f"<BlockRegisterGroupArray {self.name}[0..{len(self._elements) - 1}] "
            f"@ +0x{self.offset:X} stride=0x{self.stride:X}>"
        )


def attach_block(device: Device, block: Block) -> None:
    """Validates window bounds and attaches `block`'s registers/groups to `device`."""
    dev_size = device.size
    for entry in block.flat_registers:
        reg_bytes = entry.spec.regwidth // 8
        end = entry.offset + reg_bytes
        if end > dev_size:
            raise _err(
                "E_OUT_OF_BOUNDS",
                f"Register at 0x{entry.offset:X}..0x{end:X} ({reg_bytes} bytes) "
                f"exceeds mapped device window size 0x{dev_size:X}.",
                source=block.source,
                reg=entry.full_name,
            )

    bound_children: Dict[str, BoundChild] = {}
    bound_upper: Dict[str, BoundChild] = {}
    for cspec in block.children.values():
        bound = _bind_child(device, cspec, source=block.source)
        bound_children[cspec.name] = bound
        bound_upper[cspec.name.upper()] = bound

    flat_bound: List[BlockRegister] = [
        BlockRegister(
            device,
            entry.spec,
            offset=entry.offset,
            name=entry.full_name,
            source=block.source,
        )
        for entry in block.flat_registers
    ]

    object.__setattr__(device, "_block", block)
    object.__setattr__(device, "_registers", bound_children)
    object.__setattr__(device, "_registers_upper", bound_upper)
    object.__setattr__(device, "_flat_block_registers", tuple(flat_bound))


def format_device_dump(
    device: Device,
    *,
    diff_reset: bool = False,
    side_effects: bool = False,
) -> str:
    """Formats all registers on `device` (skipping `rclr`/`rset` registers unless `side_effects=True`)."""
    device._require_handle()
    flat_regs: Tuple[BlockRegister, ...] = device.__dict__.get("_flat_block_registers", ())
    lines: List[str] = []

    if flat_regs:
        for reg in flat_regs:
            spec = reg._spec
            off_str = _format_offset(reg.offset)
            if spec.all_na:
                if not diff_reset:
                    lines.append(f"{reg.name} @ {off_str} = <skipped: reserved (sw: na)>")
                continue
            if not spec.is_readable:
                if not diff_reset:
                    lines.append(f"{reg.name} @ {off_str} = <skipped: write-only>")
                continue
            if spec.has_read_side_effect and not side_effects:
                modes = ", ".join(spec.onread_modes)
                lines.append(f"{reg.name} @ {off_str} = <skipped: read side effect ({modes})>")
                continue
            raw_val = reg._read_raw()
            if diff_reset and spec.reset_mask != 0:
                if (raw_val & spec.reset_mask) == spec.reset_value:
                    continue
            lines.append(reg._format_decoded(raw_val))
        return "\n".join(lines)

    # Fallback for inline-defined registers when no block file is attached.
    inline_regs: Dict[str, Any] = device.__dict__.get("_registers", {})
    for reg_obj in sorted(inline_regs.values(), key=lambda r: getattr(r, "offset", 0)):
        if isinstance(reg_obj, BlockRegister):
            raw_val = reg_obj._read_raw()
            lines.append(reg_obj._format_decoded(raw_val))
        else:
            raw_val = int(reg_obj.read())
            off_str = _format_offset(reg_obj.offset)
            hex_str = _format_hex_value(raw_val, 32)
            fields_map = getattr(reg_obj, "_fields", {})
            if fields_map:
                fparts = [
                    f"{fname}={(raw_val & f.mask) >> f.lsb}"
                    for fname, f in fields_map.items()
                ]
                lines.append(f"{reg_obj.name} @ {off_str} = {hex_str}  ({', '.join(fparts)})")
            else:
                lines.append(f"{reg_obj.name} @ {off_str} = {hex_str}")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Search paths, name resolution, `mmreg.open()`, and `mmreg.list()`
# ---------------------------------------------------------------------------


def _split_colon_paths(raw: str) -> List[Path]:
    return [Path(p) for p in raw.split(":") if p.strip()]


def get_block_search_dirs(
    blocks_path: Optional[Union[str, Path, Iterable[Union[str, Path]]]] = None,
) -> List[Path]:
    dirs: List[Path] = []
    if blocks_path is not None:
        if isinstance(blocks_path, (str, Path)):
            dirs.append(Path(blocks_path))
        else:
            dirs.extend(Path(p) for p in blocks_path)
    env_blocks = os.environ.get("MMREG_BLOCKS", "")
    if env_blocks:
        dirs.extend(_split_colon_paths(env_blocks))
    dirs.append(Path("/etc/mmreg/blocks"))
    dirs.append(Path("/usr/share/mmreg/blocks"))
    return dirs


def get_instance_search_files(
    instances_path: Optional[Union[str, Path, Iterable[Union[str, Path]]]] = None,
) -> List[Path]:
    files: List[Path] = []
    if instances_path is not None:
        if isinstance(instances_path, (str, Path)):
            p = Path(instances_path)
            if p.is_dir():
                files.extend(sorted(p.glob("*.json")))
            else:
                files.append(p)
        else:
            for item in instances_path:
                p = Path(item)
                if p.is_dir():
                    files.extend(sorted(p.glob("*.json")))
                else:
                    files.append(p)
        return files

    env_inst = os.environ.get("MMREG_INSTANCES", "")
    if env_inst:
        for p in _split_colon_paths(env_inst):
            if p.is_dir():
                files.extend(sorted(p.glob("*.json")))
            else:
                files.append(p)
    etc_dir = Path("/etc/mmreg/instances.d")
    if etc_dir.is_dir():
        files.extend(sorted(etc_dir.glob("*.json")))
    return files


def load_all_instances(
    instances_path: Optional[Union[str, Path, Iterable[Union[str, Path]]]] = None,
) -> Dict[str, InstanceSpec]:
    """Loads all instance files on the search path, rejecting duplicate instance names."""
    merged: Dict[str, InstanceSpec] = {}
    seen_upper: Dict[str, InstanceSpec] = {}
    for file_path in get_instance_search_files(instances_path):
        if not file_path.is_file():
            # Explicit non-existent file in $MMREG_INSTANCES or instances_path raises an error.
            raise _MmregOSError(
                "E_BLOCK_NOT_FOUND",
                f"[E_BLOCK_NOT_FOUND] {file_path} (register '<block>', field '<none>'): Instance file not found.",
            )
        file_instances = load_instance_file(file_path)
        for name, spec in file_instances.items():
            up = name.upper()
            if up in seen_upper:
                prev = seen_upper[up]
                raise _err(
                    "E_INSTANCE_DUPLICATE",
                    f"Duplicate instance '{name}' defined in both '{prev.source}' and '{spec.source}'.",
                    source=spec.source,
                    reg=name,
                )
            seen_upper[up] = spec
            merged[name] = spec
    return merged


def resolve_block(
    block: Union[str, Path, Block],
    *,
    blocks_path: Optional[Union[str, Path, Iterable[Union[str, Path]]]] = None,
) -> Block:
    """Resolves `block` (a `Block`, file path, or `compatible` name) via the block search path."""
    if isinstance(block, Block):
        return block
    if isinstance(block, Path):
        return Block.from_file(block)
    if not isinstance(block, str) or not block.strip():
        raise MmregError("E_INVALID_ARG", f"[E_INVALID_ARG] Invalid block specifier {block!r}.")

    # Treat explicit file paths or `.json` filenames as direct paths first if they exist or have separators.
    text = block.strip()
    direct = Path(text)
    if text.endswith(".json") or "/" in text or "\\" in text:
        if direct.is_file():
            return Block.from_file(direct)
        if not text.endswith(".json") or "/" in text or "\\" in text:
            return Block.from_file(direct)

    stem = text[:-5] if text.endswith(".json") else text
    search_dirs = get_block_search_dirs(blocks_path)
    for directory in search_dirs:
        candidate = directory / f"{stem}.json"
        if candidate.is_file():
            return Block.from_file(candidate)

    searched = ", ".join(str(d) for d in search_dirs)
    raise _MmregOSError(
        "E_BLOCK_NOT_FOUND",
        f"[E_BLOCK_NOT_FOUND] {stem}.json (register '<block>', field '<none>'): "
        f"Block file for '{stem}' not found in [{searched}].",
    )


@dataclass(frozen=True)
class SysfsUioEntry:
    uio_dir: Path
    dev_name: str  # e.g. "uio0"
    name: str  # contents of /sys/class/uio/uio0/name
    compatibles: Tuple[str, ...]

    def read_map(self, region: int) -> Optional[Tuple[int, int]]:
        map_dir = self.uio_dir / "maps" / f"map{region}"
        addr_file = map_dir / "addr"
        size_file = map_dir / "size"
        if not (addr_file.is_file() and size_file.is_file()):
            return None
        try:
            addr = int(addr_file.read_text(encoding="utf-8").strip(), 0)
            size = int(size_file.read_text(encoding="utf-8").strip(), 0)
            return addr, size
        except (OSError, ValueError):
            return None


def _scan_sysfs_uio(sysfs_uio: Union[str, Path] = "/sys/class/uio") -> List[SysfsUioEntry]:
    root = Path(sysfs_uio)
    if not root.is_dir():
        return []
    entries: List[SysfsUioEntry] = []
    for child in sorted(root.iterdir(), key=lambda p: p.name):
        if not child.name.startswith("uio"):
            continue
        name_file = child / "name"
        if not name_file.is_file():
            continue
        try:
            uio_name = name_file.read_text(encoding="utf-8").strip()
        except OSError:
            continue
        compat_file = child / "device" / "of_node" / "compatible"
        compatibles: Tuple[str, ...] = ()
        if compat_file.is_file():
            try:
                raw_bytes = compat_file.read_bytes()
                compatibles = tuple(
                    part.decode("utf-8", errors="replace").strip()
                    for part in raw_bytes.split(b"\x00")
                    if part.strip()
                )
            except OSError:
                compatibles = ()
        entries.append(
            SysfsUioEntry(
                uio_dir=child,
                dev_name=child.name,
                name=uio_name,
                compatibles=compatibles,
            )
        )
    return entries


def _find_sysfs_uio_by_name(
    uio_name: str,
    sysfs_uio: Union[str, Path] = "/sys/class/uio",
) -> Optional[SysfsUioEntry]:
    for entry in _scan_sysfs_uio(sysfs_uio):
        if entry.name == uio_name:
            return entry
    return None


def _find_block_for_compatibles(
    compatibles: Sequence[str],
    *,
    blocks_path: Optional[Union[str, Path, Iterable[Union[str, Path]]]] = None,
) -> Optional[Block]:
    search_dirs = get_block_search_dirs(blocks_path)
    for compat in compatibles:
        for directory in search_dirs:
            candidate = directory / f"{compat}.json"
            if candidate.is_file():
                return Block.from_file(candidate)
    return None


@dataclass(frozen=True)
class InstanceInfo:
    """Summary of a discovered hardware instance returned by `mmreg.list()`."""

    name: str
    block: str
    target: str
    present: bool
    source: str
    desc: str = ""

    def __str__(self) -> str:
        status = "present" if self.present else "missing"
        return f"{self.name:<16} block={self.block:<24} target={self.target:<18} [{status}]"


class InstanceList(list):  # type: ignore[type-arg]
    """List of `InstanceInfo` entries returned by `mmreg.list()`, with tabular `str()` formatting."""

    def __str__(self) -> str:
        if not self:
            return "<no instances found>"
        return "\n".join(str(item) for item in self)


def list_instances(
    *,
    blocks_path: Optional[Union[str, Path, Iterable[Union[str, Path]]]] = None,
    instances_path: Optional[Union[str, Path, Iterable[Union[str, Path]]]] = None,
    sysfs_uio: Union[str, Path] = "/sys/class/uio",
    dev_root: Union[str, Path] = "/dev",
) -> InstanceList:
    """Lists all instances from instance files and UIO sysfs, and whether each device is present."""
    inst_map = load_all_instances(instances_path)
    uio_entries = _scan_sysfs_uio(sysfs_uio)
    uio_by_name = {e.name: e for e in uio_entries}
    dev_root_path = Path(dev_root)
    custom_dev_root = Path(dev_root) != Path("/dev")

    results: List[InstanceInfo] = []
    covered_uio_names: Set[str] = set()

    for inst in inst_map.values():
        try:
            blk = resolve_block(inst.block, blocks_path=blocks_path)
            block_ok = True
            block_label = blk.compatible
        except MmregError:
            block_ok = False
            block_label = inst.block

        if inst.uio is not None:
            covered_uio_names.add(inst.uio)
            uio_entry = uio_by_name.get(inst.uio)
            dev_ok = uio_entry is not None and (
                not custom_dev_root or (dev_root_path / uio_entry.dev_name).exists()
            )
            target = f"uio:{inst.uio}" if inst.uio_map == 0 else f"uio:{inst.uio}[{inst.uio_map}]"
            present = block_ok and dev_ok
        else:
            assert inst.at is not None
            dev_ok = (dev_root_path / "mem").exists() if custom_dev_root else True
            target = f"at:0x{inst.at:X}"
            present = block_ok and dev_ok

        results.append(
            InstanceInfo(
                name=inst.name,
                block=block_label,
                target=target,
                present=present,
                source=inst.source,
                desc=inst.desc,
            )
        )

    # Add UIO devices from sysfs that are not already covered by an instance entry and have a matching block file.
    for uio_entry in uio_entries:
        if uio_entry.name in covered_uio_names or uio_entry.name in inst_map:
            continue
        blk_match = _find_block_for_compatibles(uio_entry.compatibles, blocks_path=blocks_path)
        if blk_match is None:
            continue
        dev_ok = not custom_dev_root or (dev_root_path / uio_entry.dev_name).exists()
        results.append(
            InstanceInfo(
                name=uio_entry.name,
                block=blk_match.compatible,
                target=f"uio:{uio_entry.name}",
                present=dev_ok,
                source=str(uio_entry.uio_dir),
                desc=blk_match.desc,
            )
        )

    return InstanceList(results)


def _run_instance_checks(
    checks: Sequence[CheckSpec],
    *,
    instance_name: str,
    source: str,
    force: bool,
    mock: bool,
    check_reader: Optional[Callable[[int], int]] = None,
) -> None:
    for chk in checks:
        if check_reader is not None:
            val = int(check_reader(chk.addr)) & 0xFFFF_FFFF
        else:
            page_base = chk.addr & ~0xFFF
            page_off = chk.addr - page_base
            with Device.open_addr(
                page_base,
                page_off + 4,
                read_only=True,
                force=force or mock,
                mock=mock,
            ) as probe:
                val = probe.read_u32(page_off)
        if (val & chk.mask) != chk.expected:
            raise MmregError(
                "E_VALIDATION_HOOK_FAILED",
                f"[E_VALIDATION_HOOK_FAILED] {source} (register '{instance_name}', field '{chk.name}'): "
                f"Pre-access check '{chk.name}' at 0x{chk.addr:X} failed: "
                f"read 0x{val:08X} (masked 0x{val & chk.mask:08X}), expected 0x{chk.expected:08X}.",
            )


def _open_uio_target(
    uio_name: str,
    *,
    uio_map: int,
    read_only: bool,
    force: bool,
    mock: bool,
    sysfs_uio: Union[str, Path],
    dev_root: Union[str, Path],
    fallback_size: Optional[int],
) -> Device:
    sysfs_entry = _find_sysfs_uio_by_name(uio_name, sysfs_uio)
    custom_sysfs = Path(sysfs_uio) != Path("/sys/class/uio")

    if mock:
        if sysfs_entry is not None:
            map_info = sysfs_entry.read_map(uio_map)
            if map_info is not None:
                addr, size = map_info
                return Device.open_addr(
                    addr, size, read_only=read_only, force=True, mock=True
                )
        if custom_sysfs and sysfs_entry is None:
            raise _MmregOSError(
                "E_UIO_NOT_FOUND",
                f"[E_UIO_NOT_FOUND] UIO device '{uio_name}' not found in '{sysfs_uio}'.",
            )
        if fallback_size is not None and sysfs_entry is not None:
            return Device.open_addr(
                0x4000_0000, fallback_size, read_only=read_only, force=True, mock=True
            )
        return Device.open_uio(
            uio_name, region=uio_map, read_only=read_only, force=True, mock=True
        )

    if uio_name.startswith("/"):
        return Device.open_uio_path(
            uio_name, region=uio_map, read_only=read_only, force=force, mock=False
        )
    if custom_sysfs:
        if sysfs_entry is None:
            raise _MmregOSError(
                "E_UIO_NOT_FOUND",
                f"[E_UIO_NOT_FOUND] UIO device '{uio_name}' not found in '{sysfs_uio}'.",
            )
        dev_node = Path(dev_root) / sysfs_entry.dev_name
        return Device.open_uio_path(
            str(dev_node), region=uio_map, read_only=read_only, force=force, mock=False
        )
    return Device.open_uio(
        uio_name, region=uio_map, read_only=read_only, force=force, mock=False
    )


def open_device(
    name: Optional[str] = None,
    *,
    block: Optional[Union[str, Path, Block]] = None,
    uio: Optional[str] = None,
    uio_map: int = 0,
    at: Optional[int] = None,
    size: Optional[int] = None,
    read_only: bool = False,
    force: bool = False,
    mock: bool = False,
    blocks_path: Optional[Union[str, Path, Iterable[Union[str, Path]]]] = None,
    instances_path: Optional[Union[str, Path, Iterable[Union[str, Path]]]] = None,
    sysfs_uio: Union[str, Path] = "/sys/class/uio",
    dev_root: Union[str, Path] = "/dev",
    check_reader: Optional[Callable[[int], int]] = None,
) -> Device:
    """Opens a device by instance/UIO `name`, or with explicit `block=` and `uio=` / `at=`."""
    if name is not None and (block is not None or uio is not None or at is not None):
        raise MmregError(
            "E_INVALID_ARG",
            "[E_INVALID_ARG] Pass either a positional `name` or keyword `block`/`uio`/`at`, not both.",
        )
    if uio is not None and at is not None:
        raise MmregError(
            "E_INVALID_ARG",
            "[E_INVALID_ARG] Specify either `uio` or `at`, not both.",
        )

    # Case 1: Resolve by `name` (1. instance files -> 2. /sys/class/uio/*/name -> compatible -> block file)
    if name is not None:
        if not isinstance(name, str) or not name.strip():
            raise MmregError("E_INVALID_ARG", f"[E_INVALID_ARG] Device name must be a non-empty string, got {name!r}.")
        instances = load_all_instances(instances_path)
        inst = instances.get(name)
        if inst is None:
            # Also check case-insensitive match in instance files
            up = name.upper()
            inst = next((v for k, v in instances.items() if k.upper() == up), None)

        if inst is not None:
            resolved_block = resolve_block(inst.block, blocks_path=blocks_path)
            if inst.checks:
                _run_instance_checks(
                    inst.checks,
                    instance_name=inst.name,
                    source=inst.source,
                    force=force,
                    mock=mock,
                    check_reader=check_reader,
                )
            if inst.uio is not None:
                dev = _open_uio_target(
                    inst.uio,
                    uio_map=inst.uio_map,
                    read_only=read_only,
                    force=force,
                    mock=mock,
                    sysfs_uio=sysfs_uio,
                    dev_root=dev_root,
                    fallback_size=resolved_block.min_window_size,
                )
            else:
                assert inst.at is not None
                win_size = inst.size or resolved_block.size or max(resolved_block.min_window_size, 4096)
                dev = Device.open_addr(
                    inst.at, win_size, read_only=read_only, force=force, mock=mock
                )
            try:
                attach_block(dev, resolved_block)
            except Exception:
                dev.close()
                raise
            return dev

        # Step 2: Check `/sys/class/uio/*/name`
        uio_entry = _find_sysfs_uio_by_name(name, sysfs_uio)
        if uio_entry is None:
            raise _MmregOSError(
                "E_UIO_NOT_FOUND",
                f"[E_UIO_NOT_FOUND] '{name}' was not found in instance files or UIO sysfs ('{sysfs_uio}').",
            )
        if not uio_entry.compatibles:
            raise _MmregOSError(
                "E_BLOCK_NOT_FOUND",
                f"[E_BLOCK_NOT_FOUND] UIO device '{name}' ({uio_entry.dev_name}) has no "
                "device/of_node/compatible property to locate a block file.",
            )
        matched_block = _find_block_for_compatibles(uio_entry.compatibles, blocks_path=blocks_path)
        if matched_block is None:
            searched = ", ".join(str(d) for d in get_block_search_dirs(blocks_path))
            compats = ", ".join(uio_entry.compatibles)
            raise _MmregOSError(
                "E_BLOCK_NOT_FOUND",
                f"[E_BLOCK_NOT_FOUND] No block file found for UIO device '{name}' "
                f"(compatible: [{compats}]) in [{searched}].",
            )
        dev = _open_uio_target(
            name,
            uio_map=uio_map,
            read_only=read_only,
            force=force,
            mock=mock,
            sysfs_uio=sysfs_uio,
            dev_root=dev_root,
            fallback_size=matched_block.min_window_size,
        )
        try:
            attach_block(dev, matched_block)
        except Exception:
            dev.close()
            raise
        return dev

    # Case 2: Explicit `uio=` or `at=`, with optional `block=`
    if uio is None and at is None:
        raise MmregError(
            "E_INVALID_ARG",
            "[E_INVALID_ARG] `mmreg.open()` requires either `name`, `uio=...`, or `at=...`.",
        )

    resolved_blk = resolve_block(block, blocks_path=blocks_path) if block is not None else None

    if uio is not None:
        dev = _open_uio_target(
            uio,
            uio_map=uio_map,
            read_only=read_only,
            force=force,
            mock=mock,
            sysfs_uio=sysfs_uio,
            dev_root=dev_root,
            fallback_size=resolved_blk.min_window_size if resolved_blk is not None else size,
        )
    else:
        assert at is not None
        win_size = size or (
            (resolved_blk.size or max(resolved_blk.min_window_size, 4096))
            if resolved_blk is not None
            else None
        )
        if win_size is None:
            raise MmregError(
                "E_INVALID_ARG",
                "[E_INVALID_ARG] `mmreg.open(at=...)` without a block file requires `size=...`.",
            )
        dev = Device.open_addr(at, win_size, read_only=read_only, force=force, mock=mock)

    if resolved_blk is not None:
        try:
            attach_block(dev, resolved_blk)
        except Exception:
            dev.close()
            raise
    return dev

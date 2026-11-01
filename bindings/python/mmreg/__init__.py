"""
mmreg — Safe, concurrent 8/16/32/64-bit memory-mapped IO (UIO and /dev/mem) register access.

Pure-Python `ctypes` binding over `libmmreg` (the C ABI in `bindings/c`, `cargo build -p mmreg-capi`).
Access by name from block files: `mmreg.open(...)` and `mmreg.list()` (not exported by
`from mmreg import *`, which would shadow the built-in `open` and `list`).
"""

from __future__ import annotations

import ctypes
import ctypes.util
import math
import os
import sys
from collections.abc import Mapping
from pathlib import Path
import operator
from typing import Any, Dict, Iterable, List, NoReturn, Optional, Set, Tuple, Union

__all__ = [
    "Block",
    "BlockField",
    "BlockRegister",
    "BlockRegisterArray",
    "BlockRegisterGroup",
    "BlockRegisterGroupArray",
    "Device",
    "EnumValue",
    "InlineField",
    "InlineRegister",
    "InstanceInfo",
    "InstanceList",
    "LockGuard",
    "MmregError",
    "MMREG_O_FORCE",
    "MMREG_O_MOCK",
    "MMREG_O_RDONLY",
    "check",
    "find_uio",
]

__version__ = "1.4.0"

MMREG_O_RDONLY: int = 1 << 0
MMREG_O_FORCE: int = 1 << 1
MMREG_O_MOCK: int = 1 << 2


_U64_MAX = (1 << 64) - 1
_SIZE_BITS = ctypes.sizeof(ctypes.c_size_t) * 8


def _check_uint(value: int, bits: int, what: str, *, allow_bool: bool = False) -> int:
    """Returns `operator.index(value)`, raising `E_INVALID_ARG` on float or out-of-range values.

    `bool` is rejected too unless `allow_bool` is set (1-bit fields, where `EN = True` reads naturally).
    """
    if isinstance(value, bool) and not allow_bool:
        raise MmregError(
            "E_INVALID_ARG",
            f"[E_INVALID_ARG] {what} {value!r} must be an integer, not bool.",
        )
    try:
        ivalue = operator.index(value)
    except TypeError as exc:
        raise MmregError(
            "E_INVALID_ARG",
            f"[E_INVALID_ARG] {what} {value!r} is not an integer.",
        ) from exc
    if not (0 <= ivalue < (1 << bits)):
        raise MmregError(
            "E_INVALID_ARG",
            f"[E_INVALID_ARG] {what} {value!r} does not fit in an unsigned {bits}-bit integer.",
        )
    return ivalue


def _check_c_str(value: str, what: str) -> bytes:
    """Encodes `value` as UTF-8 and rejects embedded NUL bytes with `E_INVALID_ARG`."""
    if not isinstance(value, str) or "\x00" in value:
        raise MmregError(
            "E_INVALID_ARG",
            f"[E_INVALID_ARG] {what} {value!r} must be a string without embedded NUL bytes.",
        )
    return value.encode("utf-8")


def _mmreg_region_flag(region: int) -> int:
    return _check_uint(region, 8, "Region index") << 8


class MmregError(Exception):
    """Structured error raised by mmreg operations, carrying a stable `E_*` `.code`.

    Every error the package raises is an `MmregError`. Where Python protocols expect a built-in
    type, the error is also one: a missing register or field is an `AttributeError` too (so
    `hasattr` and `getattr(obj, name, default)` work), a library that cannot be loaded an
    `OSError`, and copying a `Device` a `TypeError`.
    """

    def __init__(self, code: str, message: str) -> None:
        super().__init__(code, message)
        self.code: str = code
        self.message: str = message

    def __str__(self) -> str:
        return self.message

    def __reduce__(self) -> Tuple[type, Tuple[str, str]]:
        # Rebuilt from `(code, message)` by `pickle`, e.g. when `multiprocessing` returns a
        # worker's exception to the parent process.
        return (type(self), (self.code, self.message))


class _MmregAttributeError(MmregError, AttributeError):
    """A missing register, field or attribute."""


class _MmregOSError(MmregError, OSError):
    """`libmmreg` cannot be found, loaded or is an incompatible version."""


class _MmregTypeError(MmregError, TypeError):
    """An operation the object's type does not support (copying a `Device`)."""


def _candidate_library_paths() -> List[Path]:
    candidates: List[Path] = []
    env_path = os.environ.get("MMREG_LIB_PATH")
    if env_path:
        candidates.append(Path(env_path))

    if sys.platform.startswith("win"):
        names = ["mmreg.dll", "libmmreg.dll"]
    elif sys.platform == "darwin":
        names = ["libmmreg.dylib"]
    else:
        # `libmmreg.so.<major>` (SONAME) is what runtime packages install; `libmmreg.so` is the
        # development symlink or a plain `cargo build` output.
        names = [f"libmmreg.so.{__version__.split('.')[0]}", "libmmreg.so"]

    pkg_dir = Path(__file__).resolve().parent
    repo_root = pkg_dir.parent.parent.parent
    search_dirs = [
        pkg_dir,
        repo_root / "target" / "release",
        repo_root / "target" / "debug",
        Path("/usr/local/lib"),
        Path("/usr/lib"),
        Path("/usr/lib64"),  # multilib Yocto images (libdir = /usr/lib64)
    ]
    for directory in search_dirs:
        for name in names:
            candidates.append(directory / name)
    return candidates


def _lib_load_error(details: str) -> _MmregOSError:
    return _MmregOSError("E_LIB_LOAD", f"[E_LIB_LOAD] {details}")


def _load_library() -> ctypes.CDLL:
    path = next((p for p in _candidate_library_paths() if p.is_file()), None)
    target = str(path) if path is not None else ctypes.util.find_library("mmreg")
    if target is None:
        raise _lib_load_error(
            "Could not locate shared library 'libmmreg'. Install a wheel that bundles it, install "
            "libmmreg.so (`cargo build --release -p mmreg-capi`), or set MMREG_LIB_PATH=/path/to/libmmreg.so."
        )
    try:
        return ctypes.CDLL(target)
    except OSError as exc:
        # E.g. a library built for another CPU architecture or C library.
        raise _lib_load_error(f"Cannot load '{target}': {exc}") from exc


_LIB: Optional[ctypes.CDLL] = None


def _get_lib() -> ctypes.CDLL:
    global _LIB
    if _LIB is not None:
        return _LIB

    lib = _load_library()

    lib.mmreg_last_error.argtypes = []
    lib.mmreg_last_error.restype = ctypes.c_char_p

    lib.mmreg_last_error_code.argtypes = []
    lib.mmreg_last_error_code.restype = ctypes.c_char_p

    lib.mmreg_clear_error.argtypes = []
    lib.mmreg_clear_error.restype = None

    lib.mmreg_version.argtypes = []
    lib.mmreg_version.restype = ctypes.c_char_p
    lib_version = lib.mmreg_version().decode("ascii")
    if lib_version.split(".")[0] != __version__.split(".")[0]:
        raise _lib_load_error(
            f"libmmreg {lib_version} is not compatible with the mmreg Python package {__version__} "
            "(major versions differ)."
        )

    lib.mmreg_find_uio.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_size_t]
    lib.mmreg_find_uio.restype = ctypes.c_int

    lib.mmreg_open_uio.argtypes = [ctypes.c_char_p, ctypes.c_uint32]
    lib.mmreg_open_uio.restype = ctypes.c_void_p

    lib.mmreg_open_uio_path.argtypes = [ctypes.c_char_p, ctypes.c_uint32]
    lib.mmreg_open_uio_path.restype = ctypes.c_void_p

    lib.mmreg_open_addr.argtypes = [ctypes.c_uint64, ctypes.c_size_t, ctypes.c_uint32]
    lib.mmreg_open_addr.restype = ctypes.c_void_p

    lib.mmreg_close.argtypes = [ctypes.c_void_p]
    lib.mmreg_close.restype = None

    lib.mmreg_rd8.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.POINTER(ctypes.c_uint8)]
    lib.mmreg_rd8.restype = ctypes.c_int
    lib.mmreg_rd16.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.POINTER(ctypes.c_uint16)]
    lib.mmreg_rd16.restype = ctypes.c_int
    lib.mmreg_rd32.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.POINTER(ctypes.c_uint32)]
    lib.mmreg_rd32.restype = ctypes.c_int
    lib.mmreg_rd64.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.POINTER(ctypes.c_uint64)]
    lib.mmreg_rd64.restype = ctypes.c_int

    lib.mmreg_wr8.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint8]
    lib.mmreg_wr8.restype = ctypes.c_int
    lib.mmreg_wr16.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint16]
    lib.mmreg_wr16.restype = ctypes.c_int
    lib.mmreg_wr32.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint32]
    lib.mmreg_wr32.restype = ctypes.c_int
    lib.mmreg_wr64.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint64]
    lib.mmreg_wr64.restype = ctypes.c_int

    lib.mmreg_update32.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.c_uint32,
        ctypes.c_uint32,
        ctypes.POINTER(ctypes.c_uint32),
    ]
    lib.mmreg_update32.restype = ctypes.c_int

    lib.mmreg_lock.argtypes = [ctypes.c_void_p]
    lib.mmreg_lock.restype = ctypes.c_int

    lib.mmreg_unlock.argtypes = [ctypes.c_void_p]
    lib.mmreg_unlock.restype = ctypes.c_int

    lib.mmreg_irq_unmask.argtypes = [ctypes.c_void_p]
    lib.mmreg_irq_unmask.restype = ctypes.c_int

    lib.mmreg_irq_wait.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint64,
        ctypes.POINTER(ctypes.c_uint32),
    ]
    lib.mmreg_irq_wait.restype = ctypes.c_int

    lib.mmreg_base_address.argtypes = [ctypes.c_void_p]
    lib.mmreg_base_address.restype = ctypes.c_uint64

    lib.mmreg_size.argtypes = [ctypes.c_void_p]
    lib.mmreg_size.restype = ctypes.c_size_t

    lib.mmreg_warning_count.argtypes = [ctypes.c_void_p]
    lib.mmreg_warning_count.restype = ctypes.c_size_t

    lib.mmreg_warning.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
    lib.mmreg_warning.restype = ctypes.c_char_p

    _LIB = lib
    return lib


def _raise_last_error(lib: ctypes.CDLL) -> None:
    raw_code = lib.mmreg_last_error_code()
    raw_msg = lib.mmreg_last_error()
    code = raw_code.decode("utf-8", errors="replace") if raw_code else "E_UNKNOWN"
    msg = raw_msg.decode("utf-8", errors="replace") if raw_msg else f"[{code}] Unknown mmreg error"
    raise MmregError(code, msg)


def find_uio(name: str) -> str:
    """Resolves a UIO device name (`/sys/class/uio/uio*/name`) to its `/dev/uioN` path."""
    lib = _get_lib()
    c_name = _check_c_str(name, "UIO name")
    buf = ctypes.create_string_buffer(256)
    rc = lib.mmreg_find_uio(c_name, buf, len(buf))
    if rc != 0:
        _raise_last_error(lib)
    return buf.value.decode("utf-8")


def _parse_bit_spec(spec: Union[str, int, Tuple[int, int]]) -> Tuple[int, int]:
    """Parses a bit specification (`"0"`, `"7:4"`, `"[7:4]"`, `0`, or `(7, 4)`) into `(msb, lsb)`.

    String bit numbers are decimal (`"08"` is bit 8) or `0x` hex. `bool`, floats and anything
    else raise `E_SUBREG_RANGE`.
    """

    def index(bit: object) -> int:
        if isinstance(bit, bool):
            raise TypeError("bool is not a bit number")
        if isinstance(bit, str):
            text = bit.strip()
            return int(text, 16) if text.lower().startswith("0x") else int(text, 10)
        return operator.index(bit)

    try:
        if isinstance(spec, str):
            hi, sep, lo = spec.strip().strip("[]").partition(":")
            msb = index(hi)
            lsb = index(lo) if sep else msb
        elif isinstance(spec, tuple) and len(spec) == 2:
            msb, lsb = index(spec[0]), index(spec[1])
        else:
            msb = lsb = index(spec)
    except (TypeError, ValueError) as exc:
        raise MmregError(
            "E_SUBREG_RANGE",
            f'[E_SUBREG_RANGE] Invalid field bit specification {spec!r} (expected "7:4", "[7:4]", "0", 0 or (7, 4)).',
        ) from exc

    if lsb < 0 or msb > 31 or msb < lsb:
        raise MmregError(
            "E_SUBREG_RANGE",
            f"[E_SUBREG_RANGE] Invalid subregister bit range [{msb}:{lsb}] (expected 0 <= lsb <= msb <= 31).",
        )
    return msb, lsb


class _HardwareValue:
    """Value protocol of registers and fields: `int()`, `operator.index()`, `format()` and `==` read
    the hardware through `read()`.

    Reading hardware is always explicit; `repr()` never touches it, so REPLs and debuggers cannot
    clear read-to-clear bits. Defining `__eq__` makes instances unhashable; that is intended, as
    equality reads hardware.
    """

    __slots__ = ()

    def __int__(self) -> int:
        return int(self.read())  # type: ignore[attr-defined]

    def __index__(self) -> int:
        return int(self.read())  # type: ignore[attr-defined]

    def __format__(self, format_spec: str) -> str:
        if not format_spec:
            return str(self)
        value = self.read()  # type: ignore[attr-defined]
        return format(str(value), format_spec) if format_spec.endswith("s") else format(int(value), format_spec)

    def __eq__(self, other: object) -> bool:
        if isinstance(other, _HardwareValue):
            return int(self) == int(other)
        if isinstance(other, (int, str)):
            return bool(self.read() == other)  # type: ignore[attr-defined]
        return NotImplemented

    def __ne__(self, other: object) -> bool:
        equal = self.__eq__(other)
        return equal if equal is NotImplemented else not equal

    __hash__ = None  # type: ignore[assignment]

    def watch(
        self,
        *,
        interval: float = 0.1,
        count: Optional[int] = None,
        side_effects: bool = False,
        file: Any = None,
    ) -> None:
        """Polls this register or field every `interval` seconds (at least 1 ms), `count` times or
        until Ctrl-C, printing its value and then each change (`Device.watch` for several)."""
        watch_targets(
            self._device,  # type: ignore[attr-defined]
            (self,),
            interval=interval,
            count=count,
            side_effects=side_effects,
            file=file,
        )

    def _location(self) -> Tuple[int, int, int]:
        """The bits this object accesses: `(device, offset, mask)`; whole registers have mask -1."""
        return (id(self._device), self.offset, getattr(self, "mask", -1))  # type: ignore[attr-defined]


def _assign(target: Any, value: object) -> None:
    """`owner.NAME = value` (or `owner["NAME"] = value`) for register or field `target`.

    A register or field object as `value` is read and its value written, except `target` itself
    (`d.CTRL = d.CTRL`), which is a no-op instead of a write-back that could clear pending bits.
    Register arrays and groups cannot be assigned.
    """
    if not hasattr(target, "write"):
        raise MmregError(
            "E_INVALID_ARG",
            f"[E_INVALID_ARG] Cannot assign directly to register array or group '{target.name}'.",
        )
    if isinstance(value, _HardwareValue) and value._location() == target._location():
        return
    target.write(value)


def _is_api_attr(obj: object, key: str) -> bool:
    """Whether `key` is an attribute or method of `obj` (`name`, `read`, `size`, ...), not a register or field."""
    return key.startswith("_") or hasattr(type(obj), key) or key in getattr(obj, "__dict__", ())


def _api_attr_error(obj: object, key: str, shadowed: Optional[str]) -> MmregError:
    hint = f"; to write the register or field '{shadowed}', use [{shadowed!r}]" if shadowed else ""
    return MmregError(
        "E_INVALID_ARG",
        f"[E_INVALID_ARG] '{key}' is a read-only attribute of {type(obj).__name__}{hint}.",
    )


def _set_named(owner: Any, key: str, value: object) -> None:
    """`owner.key = value` on a register (fields) or device (registers), via `owner._find(key)`.

    Lookups are case-insensitive, so `reg.name = 1` would otherwise write a field called `NAME`
    while `reg.name` reads the register's name: attributes are refused, `reg["NAME"]` is not.
    """
    found = owner._find(key)
    if _is_api_attr(owner, key):
        raise _api_attr_error(owner, key, None if found is None else found.name)
    if found is None:
        raise owner._not_found(key)
    _assign(found, value)


class InlineField(_HardwareValue):
    """Bitfield `[msb:lsb]` of an inline-defined 32-bit register."""

    def __init__(
        self,
        device: Device,
        reg_name: str,
        offset: int,
        name: str,
        msb: int,
        lsb: int,
        w1c: bool = False,
    ) -> None:
        self._device = device
        self.reg_name = reg_name
        self.offset = offset
        self.name = name
        self.msb = msb
        self.lsb = lsb
        self.width = msb - lsb + 1
        self.mask = ((1 << self.width) - 1) << lsb
        self.w1c = w1c
        # Set by `InlineRegister`: the register's other write-1-to-clear bits.
        self._other_w1c_mask = 0

    def read(self) -> int:
        """Reads the parent 32-bit register and extracts this bitfield's value."""
        return (self._device.read_u32(self.offset) & self.mask) >> self.lsb

    def write(self, value: int) -> int:
        """Writes `value` into this bitfield and returns the 32-bit value written to the register.

        Every field, `w1c` or not, is a masked read-modify-write (`update_u32`) under the device
        lock: bits outside this field keep their value, and the register's other `w1c` bits are
        written as `0`, so writing this field neither changes other fields nor clears pending bits.
        """
        shifted = (
            _check_uint(value, self.width, f"Field {self.reg_name}.{self.name} value", allow_bool=self.width == 1)
            << self.lsb
        )
        return self._device.update_u32(self.offset, self.mask | self._other_w1c_mask, shifted)

    def __repr__(self) -> str:
        access = " w1c" if self.w1c else ""
        return f"<InlineField {self.reg_name}.{self.name}[{self.msb}:{self.lsb}]{access} @ +0x{self.offset:X}>"


class InlineRegister(_HardwareValue):
    """Inline-defined 32-bit register on a `Device` (`dev.define("CTRL", offset=0x8, fields={"EN": "0"})`)."""

    def __init__(
        self,
        device: Device,
        name: str,
        offset: int,
        fields: Optional[Dict[str, Union[str, int, Tuple[int, int]]]] = None,
        w1c: Iterable[str] = (),
    ) -> None:
        object.__setattr__(self, "_device", device)
        object.__setattr__(self, "name", name)
        object.__setattr__(self, "offset", int(offset))
        object.__setattr__(self, "at", int(offset))
        if fields is None:
            fields = {}
        if not isinstance(fields, Mapping):
            raise MmregError(
                "E_INVALID_ARG",
                f"[E_INVALID_ARG] Fields of register '{name}' must be a dict of name -> bits, not {fields!r}.",
            )
        w1c_names = _name_set(w1c, f"w1c fields of register '{name}'")
        unknown = sorted(w1c_names - set(fields))
        if unknown:
            raise MmregError(
                "E_SUBREG_NOT_FOUND",
                f"[E_SUBREG_NOT_FOUND] w1c field(s) {unknown} not defined in register '{name}'.",
            )
        # Keyed by upper-case name: lookups are case-insensitive.
        parsed_fields: Dict[str, InlineField] = {}
        used_mask = 0
        w1c_mask = 0
        for field_name, spec in fields.items():
            _check_attr_name(field_name, InlineRegister, f"Field name in register '{name}'")
            up_name = field_name.upper()
            if up_name in parsed_fields:
                raise MmregError(
                    "E_INVALID_ARG",
                    f"[E_INVALID_ARG] Field names '{parsed_fields[up_name].name}' and '{field_name}' "
                    f"in register '{name}' differ only by case.",
                )
            msb, lsb = _parse_bit_spec(spec)
            field = InlineField(device, name, int(offset), field_name, msb, lsb, w1c=field_name in w1c_names)
            if field.mask & used_mask:
                raise MmregError(
                    "E_SUBREG_RANGE",
                    f"[E_SUBREG_RANGE] Field '{field_name}' [{msb}:{lsb}] overlaps another field of register '{name}'.",
                )
            used_mask |= field.mask
            if field.w1c:
                w1c_mask |= field.mask
            parsed_fields[up_name] = field
        for field in parsed_fields.values():
            field._other_w1c_mask = w1c_mask & ~field.mask
        object.__setattr__(self, "_fields", parsed_fields)

    def read(self) -> int:
        """Reads the 32-bit register from hardware."""
        return self._device.read_u32(self.offset)

    def write(self, value: int) -> None:
        """Writes `value` to the 32-bit register in hardware."""
        self._device.write_u32(self.offset, value)

    def _find(self, key: str) -> Optional[InlineField]:
        fields: Optional[Dict[str, InlineField]] = self.__dict__.get("_fields")
        return fields.get(key.upper()) if fields is not None and isinstance(key, str) else None

    def _not_found(self, key: str) -> _MmregAttributeError:
        fields: Optional[Dict[str, InlineField]] = self.__dict__.get("_fields")
        if fields is None:
            return _no_attr(self, key)
        avail = ", ".join(sorted(f.name for f in fields.values()))
        return _MmregAttributeError(
            "E_SUBREG_NOT_FOUND",
            f"[E_SUBREG_NOT_FOUND] Field '{key}' not found in register '{self.name}'. Available: [{avail}]",
        )

    def _field(self, key: str) -> InlineField:
        found = self._find(key)
        if found is None:
            raise self._not_found(key)
        return found

    def __getattr__(self, item: str) -> InlineField:
        _reject_private_attr(self, item)
        return self._field(item)

    def __setattr__(self, key: str, value: object) -> None:
        _set_named(self, key, value)

    def __getitem__(self, key: str) -> InlineField:
        return self._field(key)

    def __setitem__(self, key: str, value: object) -> None:
        _assign(self._field(key), value)

    def __repr__(self) -> str:
        fields: Dict[str, InlineField] = self._fields
        return (
            f"<InlineRegister {self.name} @ +0x{self.offset:X} fields=[{', '.join(f.name for f in fields.values())}]>"
        )


class LockGuard:
    """Context manager holding a reentrant sequence lock on a `Device` (`with dev.lock():`)."""

    def __init__(self, device: Device) -> None:
        self._device = device
        self._active = False

    def __enter__(self) -> LockGuard:
        self._device._acquire_lock()
        self._active = True
        return self

    def __exit__(self, exc_type: object, exc_val: object, exc_tb: object) -> None:
        if self._active:
            self._active = False
            self._device._release_lock()


class Device:
    """A mapped peripheral register window with 8/16/32/64-bit access, sequence locking, and UIO IRQs."""

    _handle: int = 0

    def __init__(self, handle: int, lib: ctypes.CDLL) -> None:
        object.__setattr__(self, "_handle", handle)
        object.__setattr__(self, "_lib", lib)
        # Registers by upper-case name (lookups are case-insensitive): inline ones and a block's.
        object.__setattr__(self, "_registers", {})
        object.__setattr__(self, "_block", None)

    @classmethod
    def open_uio(
        cls,
        name: str,
        *,
        region: int = 0,
        read_only: bool = False,
        force: bool = False,
        mock: bool = False,
    ) -> Device:
        """Opens region `region` (default 0) of the UIO device matching `name`."""
        lib = _get_lib()
        c_name = _check_c_str(name, "UIO name")
        flags = _build_flags(region=region, read_only=read_only, force=force, mock=mock)
        handle = lib.mmreg_open_uio(c_name, flags)
        if not handle:
            _raise_last_error(lib)
        return cls(handle, lib)

    @classmethod
    def open_uio_path(
        cls,
        path: str,
        *,
        region: int = 0,
        read_only: bool = False,
        force: bool = False,
        mock: bool = False,
    ) -> Device:
        """Opens region `region` (default 0) of the UIO device node at `path` (e.g. `"/dev/uio0"`)."""
        lib = _get_lib()
        c_path = _check_c_str(path, "UIO path")
        flags = _build_flags(region=region, read_only=read_only, force=force, mock=mock)
        handle = lib.mmreg_open_uio_path(c_path, flags)
        if not handle:
            _raise_last_error(lib)
        return cls(handle, lib)

    @classmethod
    def open_addr(
        cls,
        base: int,
        size: int,
        *,
        read_only: bool = False,
        force: bool = False,
        mock: bool = False,
    ) -> Device:
        """Opens physical address window `[base, base + size)` via UIO or `/dev/mem` (`debug` feature)."""
        lib = _get_lib()
        checked_base = _check_uint(base, 64, "Base address")
        checked_size = _check_uint(size, _SIZE_BITS, "Window size")
        flags = _build_flags(region=0, read_only=read_only, force=force, mock=mock)
        handle = lib.mmreg_open_addr(checked_base, checked_size, flags)
        if not handle:
            _raise_last_error(lib)
        return cls(handle, lib)

    def _require_handle(self) -> int:
        handle: int = self._handle
        if not handle:
            raise MmregError("E_INVALID_ARG", "[E_INVALID_ARG] Operation on a closed mmreg.Device.")
        return handle

    def close(self) -> None:
        """Unmaps and closes the device window (idempotent)."""
        handle: int = self._handle
        if handle:
            lib: ctypes.CDLL = self._lib
            object.__setattr__(self, "_handle", 0)
            lib.mmreg_close(handle)

    def __del__(self) -> None:
        try:
            self.close()
        except Exception:
            pass

    def __reduce_ex__(self, protocol: object) -> NoReturn:
        # `copy.copy`, `copy.deepcopy` and `pickle` all end here: a copy would share the C handle and
        # close it twice.
        raise _MmregTypeError(
            "E_NOT_COPYABLE",
            f"[E_NOT_COPYABLE] '{type(self).__name__}' owns an open device handle and cannot be copied or pickled; "
            "share the same object instead.",
        )

    def __enter__(self) -> Device:
        self._require_handle()
        return self

    def __exit__(self, exc_type: object, exc_val: object, exc_tb: object) -> None:
        self.close()

    @property
    def base_address(self) -> int:
        """Physical base address of the mapped window."""
        handle = self._require_handle()
        return int(self._lib.mmreg_base_address(handle))

    @property
    def size(self) -> int:
        """Size of the mapped window in bytes."""
        handle = self._require_handle()
        return int(self._lib.mmreg_size(handle))

    @property
    def warnings(self) -> List[str]:
        """Non-fatal hardware pre-flight warnings collected when the device was opened."""
        handle = self._require_handle()
        lib = self._lib
        count = int(lib.mmreg_warning_count(handle))
        out: List[str] = []
        for idx in range(count):
            ptr = lib.mmreg_warning(handle, idx)
            if ptr:
                out.append(ptr.decode("utf-8", errors="replace"))
        return out

    def read_u8(self, offset: int) -> int:
        handle = self._require_handle()
        lib = self._lib
        val = ctypes.c_uint8()
        if lib.mmreg_rd8(handle, _check_uint(offset, 32, "Offset"), ctypes.byref(val)) != 0:
            _raise_last_error(lib)
        return int(val.value)

    def read_u16(self, offset: int) -> int:
        handle = self._require_handle()
        lib = self._lib
        val = ctypes.c_uint16()
        if lib.mmreg_rd16(handle, _check_uint(offset, 32, "Offset"), ctypes.byref(val)) != 0:
            _raise_last_error(lib)
        return int(val.value)

    def read_u32(self, offset: int) -> int:
        handle = self._require_handle()
        lib = self._lib
        val = ctypes.c_uint32()
        if lib.mmreg_rd32(handle, _check_uint(offset, 32, "Offset"), ctypes.byref(val)) != 0:
            _raise_last_error(lib)
        return int(val.value)

    def read_u64(self, offset: int) -> int:
        handle = self._require_handle()
        lib = self._lib
        val = ctypes.c_uint64()
        if lib.mmreg_rd64(handle, _check_uint(offset, 32, "Offset"), ctypes.byref(val)) != 0:
            _raise_last_error(lib)
        return int(val.value)

    def write_u8(self, offset: int, value: int) -> None:
        handle = self._require_handle()
        lib = self._lib
        if lib.mmreg_wr8(handle, _check_uint(offset, 32, "Offset"), _check_uint(value, 8, "Value")) != 0:
            _raise_last_error(lib)

    def write_u16(self, offset: int, value: int) -> None:
        handle = self._require_handle()
        lib = self._lib
        if lib.mmreg_wr16(handle, _check_uint(offset, 32, "Offset"), _check_uint(value, 16, "Value")) != 0:
            _raise_last_error(lib)

    def write_u32(self, offset: int, value: int) -> None:
        handle = self._require_handle()
        lib = self._lib
        if lib.mmreg_wr32(handle, _check_uint(offset, 32, "Offset"), _check_uint(value, 32, "Value")) != 0:
            _raise_last_error(lib)

    def write_u64(self, offset: int, value: int) -> None:
        handle = self._require_handle()
        lib = self._lib
        if lib.mmreg_wr64(handle, _check_uint(offset, 32, "Offset"), _check_uint(value, 64, "Value")) != 0:
            _raise_last_error(lib)

    def update_u32(self, offset: int, mask: int, value: int) -> int:
        """Atomically updates bits in `mask` at `offset` (`(old & ~mask) | (value & mask)`) under one lock."""
        handle = self._require_handle()
        lib = self._lib
        updated = ctypes.c_uint32()
        rc = lib.mmreg_update32(
            handle,
            _check_uint(offset, 32, "Offset"),
            _check_uint(mask, 32, "Mask"),
            _check_uint(value, 32, "Value"),
            ctypes.byref(updated),
        )
        if rc != 0:
            _raise_last_error(lib)
        return int(updated.value)

    def _acquire_lock(self) -> None:
        handle = self._require_handle()
        lib = self._lib
        if lib.mmreg_lock(handle) != 0:
            _raise_last_error(lib)

    def _release_lock(self) -> None:
        handle = self._require_handle()
        lib = self._lib
        if lib.mmreg_unlock(handle) != 0:
            _raise_last_error(lib)

    def lock(self) -> LockGuard:
        """Returns a reentrant context manager (`with dev.lock():`) holding the device lock."""
        return LockGuard(self)

    def irq_unmask(self) -> None:
        """Unmasks (re-enables) the UIO interrupt line by writing `1u32` to `/dev/uioN`."""
        handle = self._require_handle()
        lib = self._lib
        if lib.mmreg_irq_unmask(handle) != 0:
            _raise_last_error(lib)

    @property
    def block(self) -> Optional[Block]:
        """The `Block` attached to this device, or `None` if opened without a block file."""
        return self.__dict__.get("_block")

    def irq_wait(self, timeout_ms: Optional[int]) -> Optional[int]:
        """Waits up to `timeout_ms` ms (`None`: no timeout) for a UIO interrupt.

        Returns the cumulative interrupt count, or `None` when the timeout elapsed.
        """
        handle = self._require_handle()
        lib = self._lib
        timeout = _U64_MAX if timeout_ms is None else _check_uint(timeout_ms, 64, "Timeout (ms)")
        count = ctypes.c_uint32()
        rc = lib.mmreg_irq_wait(handle, timeout, ctypes.byref(count))
        if rc == 1:
            return int(count.value)
        if rc == 0:
            return None
        _raise_last_error(lib)
        return None

    def wait_irq(self, timeout: Optional[float] = None) -> Optional[int]:
        """Waits up to `timeout` seconds (`None`: no timeout) for a UIO interrupt."""
        if timeout is None:
            return self.irq_wait(None)
        if isinstance(timeout, bool) or not isinstance(timeout, (int, float)):
            raise MmregError(
                "E_INVALID_ARG",
                f"[E_INVALID_ARG] Timeout {timeout!r} must be a non-negative number of seconds or None.",
            )
        if math.isnan(timeout) or math.isinf(timeout) or timeout < 0:
            raise MmregError(
                "E_INVALID_ARG",
                f"[E_INVALID_ARG] Timeout {timeout!r} must be a finite non-negative number of seconds.",
            )
        return self.irq_wait(int(math.ceil(timeout * 1000.0)))

    def dump(self, *, diff_reset: bool = False, side_effects: bool = False) -> str:
        """Returns a formatted multi-line dump of the block's registers, then the inline ones.

        Registers with read side effects (`onread: "rclr"` or `"rset"`) are skipped and noted
        unless `side_effects=True` is passed. When `diff_reset=True`, block registers whose value
        equals their reset value are left out.
        """
        return format_device_dump(self, diff_reset=diff_reset, side_effects=side_effects)

    def watch(
        self,
        *targets: Any,
        interval: float = 0.1,
        count: Optional[int] = None,
        only_volatile: bool = False,
        side_effects: bool = False,
        file: Any = None,
    ) -> None:
        """Polls `targets` (register or field names such as `"IRQ_STATUS.CHANGE"` or
        `"DEBOUNCE[2].CYCLES"`, or objects) every `interval` seconds (at least 1 ms), `count` times or
        until Ctrl-C, printing each value and then each change (`old -> new`) to `file` (stdout).

        Without `targets`: every readable register (`only_volatile=True`: those with a field
        hardware changes); `rclr` / `rset` registers only with `side_effects=True`.
        """
        watch_targets(
            self,
            targets,
            interval=interval,
            count=count,
            only_volatile=only_volatile,
            side_effects=side_effects,
            file=file,
        )

    def define(
        self,
        name: str,
        *,
        offset: Optional[int] = None,
        at: Optional[int] = None,
        fields: Optional[Dict[str, Union[str, int, Tuple[int, int]]]] = None,
        w1c: Iterable[str] = (),
    ) -> InlineRegister:
        """Defines an inline 32-bit register on this device (`offset=` or `at=`).

        `d.define("ISR", at=0xC, fields={"DONE": "0", "ERR": "1", "MODE": "7:4"}, w1c={"DONE", "ERR"})`
        marks `DONE` and `ERR` write-1-to-clear. Every field write is a masked read-modify-write
        that keeps the other fields and writes `0` to the other `w1c` bits: `d.ISR.DONE = 1` clears
        only `DONE` and keeps `MODE`. List write-1-to-pulse (self-clearing) bits in `w1c` too.
        """
        self._require_handle()
        _check_attr_name(name, Device, "Register name")
        if (offset is None) == (at is None):
            raise MmregError(
                "E_INVALID_ARG",
                f"[E_INVALID_ARG] Specify exactly one of `offset=` or `at=` for register '{name}'.",
            )
        raw_offset = offset if offset is not None else at
        assert raw_offset is not None
        checked_offset = _check_uint(raw_offset, 32, "Offset")
        base_addr = self.base_address
        region_size = self.size
        if checked_offset % 4 != 0 or (base_addr + checked_offset) % 4 != 0:
            raise MmregError(
                "E_UNALIGNED_ACCESS",
                f"[E_UNALIGNED_ACCESS] Register '{name}' at offset 0x{checked_offset:X} is not 4-byte aligned.",
            )
        if checked_offset + 4 > region_size:
            raise MmregError(
                "E_OUT_OF_BOUNDS",
                f"[E_OUT_OF_BOUNDS] Register '{name}' at offset 0x{checked_offset:X} (4 bytes) "
                f"exceeds window size 0x{region_size:X}.",
            )
        regs: Dict[str, Any] = self._registers
        existing = regs.get(name.upper())
        if existing is not None and existing.name != name:
            raise MmregError(
                "E_INVALID_ARG",
                f"[E_INVALID_ARG] Register name '{name}' conflicts case-insensitively with '{existing.name}'.",
            )
        reg = InlineRegister(self, name, checked_offset, fields, w1c)
        regs[name.upper()] = reg
        return reg

    def _find(self, key: str) -> Any:
        regs: Optional[Dict[str, Any]] = self.__dict__.get("_registers")
        return regs.get(key.upper()) if regs is not None and isinstance(key, str) else None

    def _not_found(self, key: str) -> _MmregAttributeError:
        return _no_attr(self, key)

    def __getattr__(self, item: str) -> Any:
        _reject_private_attr(self, item)
        found = self._find(item)
        if found is None:
            raise _no_attr(self, item)
        return found

    def __setattr__(self, key: str, value: object) -> None:
        if key.startswith("_") or self._find(key) is None:
            # Internal state, or an attribute of the caller's (e.g. `mock.patch.object`).
            object.__setattr__(self, key, value)
            return
        _set_named(self, key, value)

    def __getitem__(self, key: str) -> Any:
        found = self._find(key)
        if found is None:
            raise _no_attr(self, key)
        return found

    def __setitem__(self, key: str, value: object) -> None:
        _assign(self[key], value)


def _no_attr(obj: object, item: str) -> _MmregAttributeError:
    code = "E_SUBREG_NOT_FOUND" if isinstance(obj, InlineRegister) else "E_REG_NOT_FOUND"
    return _MmregAttributeError(code, f"[{code}] '{type(obj).__name__}' object has no attribute '{item}'")


def _name_set(names: Union[str, Iterable[str]], what: str) -> Set[str]:
    """Returns `names` as a set of strings (a single string is one name), else `E_INVALID_ARG`."""
    if isinstance(names, str):
        return {names}
    try:
        result = set(names)
    except TypeError:
        result = None
    if result is None or not all(isinstance(n, str) for n in result):
        raise MmregError("E_INVALID_ARG", f"[E_INVALID_ARG] {what} must be names (strings), not {names!r}.")
    return result


def _reject_private_attr(obj: object, item: str) -> None:
    """Raises `AttributeError` for `_`-prefixed names in `__getattr__`.

    Registers and fields cannot start with `_` (see `_check_attr_name`), so such a lookup is an
    internal or protocol attribute (`_fields`, `__setstate__`, ...). Raising here keeps `hasattr`,
    `copy`, and half-initialised objects from recursing through `__getattr__`.
    """
    if item.startswith("_"):
        raise _no_attr(obj, item)


def _check_attr_name(name: str, owner: type, what: str) -> None:
    """Rejects names that are not identifiers or that would shadow an attribute of `owner`."""
    if (
        not isinstance(name, str)
        or not name.isidentifier()
        or name.startswith("_")
        or hasattr(owner, name)
        or name in ("name", "offset")
    ):
        raise MmregError(
            "E_INVALID_ARG",
            f"[E_INVALID_ARG] {what} {name!r} is not a valid identifier or shadows an existing attribute.",
        )


def _build_flags(*, region: int, read_only: bool, force: bool, mock: bool) -> int:
    flags = _mmreg_region_flag(region)
    if read_only:
        flags |= MMREG_O_RDONLY
    if force:
        flags |= MMREG_O_FORCE
    if mock:
        flags |= MMREG_O_MOCK
    return flags


# The block-file layer imports the names above, so it is imported last.
from ._access import (  # noqa: E402
    BlockField,
    BlockRegister,
    BlockRegisterArray,
    BlockRegisterGroup,
    BlockRegisterGroupArray,
    format_device_dump,
    watch_targets,
)
from ._resolve import InstanceInfo, InstanceList  # noqa: E402
from ._resolve import list_instances as list  # noqa: E402, F401  (mmreg.list, not in __all__)
from ._resolve import open_device as open  # noqa: E402, F401  (mmreg.open, not in __all__)
from .block import Block, EnumValue  # noqa: E402
from .block import check_file as check  # noqa: E402

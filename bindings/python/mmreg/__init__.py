"""
mmreg — Safe, concurrent 8/16/32/64-bit memory-mapped IO (UIO and /dev/mem) register access.

Pure-Python `ctypes` binding over `libmmreg` (the C ABI in `bindings/c`, `cargo build -p mmreg-capi`).
"""

from __future__ import annotations

import ctypes
import ctypes.util
import os
import sys
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Tuple, Union

__all__ = [
    "Device",
    "InlineField",
    "InlineRegister",
    "LockGuard",
    "MmregError",
    "MMREG_O_FORCE",
    "MMREG_O_MOCK",
    "MMREG_O_RDONLY",
    "find_uio",
]

__version__ = "1.2.0"

MMREG_O_RDONLY: int = 1 << 0
MMREG_O_FORCE: int = 1 << 1
MMREG_O_MOCK: int = 1 << 2


_U64_MAX = (1 << 64) - 1


def _check_uint(value: int, bits: int, what: str) -> int:
    """Returns `int(value)`, raising `E_INVALID_ARG` unless it fits in an unsigned `bits`-bit integer."""
    ivalue = int(value)
    if not (0 <= ivalue < (1 << bits)):
        raise MmregError(
            "E_INVALID_ARG",
            f"[E_INVALID_ARG] {what} {value!r} does not fit in an unsigned {bits}-bit integer.",
        )
    return ivalue


def _mmreg_region_flag(region: int) -> int:
    if not (0 <= region <= 0xFF):
        raise MmregError("E_INVALID_ARG", f"[E_INVALID_ARG] Region index {region} out of range 0..255")
    return (region & 0xFF) << 8


class MmregError(Exception):
    """Structured error raised by mmreg operations, carrying a stable `E_*` `.code`."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code: str = code
        self.message: str = message

    def __str__(self) -> str:
        return self.message


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
    ]
    for directory in search_dirs:
        for name in names:
            candidates.append(directory / name)
    return candidates


def _load_library() -> ctypes.CDLL:
    for path in _candidate_library_paths():
        if path.is_file():
            return ctypes.CDLL(str(path))

    found = ctypes.util.find_library("mmreg")
    if found:
        return ctypes.CDLL(found)

    raise OSError(
        "Could not locate shared library 'libmmreg' (build with `cargo build --release -p mmreg-capi` "
        "or set MMREG_LIB_PATH=/path/to/libmmreg.so)."
    )


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
        raise OSError(
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
    buf = ctypes.create_string_buffer(256)
    rc = lib.mmreg_find_uio(name.encode("utf-8"), buf, len(buf))
    if rc != 0:
        _raise_last_error(lib)
    return buf.value.decode("utf-8")


def _parse_bit_spec(spec: Union[str, int, Tuple[int, int]]) -> Tuple[int, int]:
    """Parses a bit specification (`"0"`, `"7:4"`, `"[7:4]"`, `0`, or `(7, 4)`) into `(msb, lsb)`."""
    if isinstance(spec, int):
        msb, lsb = spec, spec
    elif isinstance(spec, tuple) and len(spec) == 2:
        msb, lsb = int(spec[0]), int(spec[1])
    elif isinstance(spec, str):
        cleaned = spec.strip().strip("[]")
        if ":" in cleaned:
            hi_s, lo_s = cleaned.split(":", 1)
            msb, lsb = int(hi_s, 0), int(lo_s, 0)
        else:
            bit = int(cleaned, 0)
            msb, lsb = bit, bit
    else:
        raise MmregError("E_SUBREG_RANGE", f"[E_SUBREG_RANGE] Invalid field bit specification: {spec!r}")

    if lsb < 0 or msb > 31 or msb < lsb:
        raise MmregError(
            "E_SUBREG_RANGE",
            f"[E_SUBREG_RANGE] Invalid subregister bit range [{msb}:{lsb}] (expected 0 <= lsb <= msb <= 31).",
        )
    return msb, lsb


class InlineField:
    """Bitfield `[msb:lsb]` of an inline-defined 32-bit register.

    Reading hardware is always explicit (`int(field)`, `field.read()`, or comparing with `==`);
    `repr()` never touches the register, so REPLs and debuggers cannot clear read-to-clear bits.
    """

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

        A `w1c` field is written directly (`value << lsb`, every other bit `0`), never read back.
        Any other field is updated with `update_u32`, which also writes `0` to the register's `w1c`
        bits so pending ones are not cleared by the read-modify-write.
        """
        shifted = _check_uint(value, self.width, f"Field {self.reg_name}.{self.name} value") << self.lsb
        if self.w1c:
            self._device.write_u32(self.offset, shifted)
            return shifted
        return self._device.update_u32(self.offset, self.mask | self._other_w1c_mask, shifted)

    def __int__(self) -> int:
        return self.read()

    def __index__(self) -> int:
        return self.read()

    def __eq__(self, other: object) -> bool:
        if isinstance(other, (int, InlineField)):
            return int(self) == int(other)
        return NotImplemented

    # Defining `__eq__` makes instances unhashable; that is intended, as equality reads hardware.
    __hash__ = None  # type: ignore[assignment]

    def __repr__(self) -> str:
        access = " w1c" if self.w1c else ""
        return f"<InlineField {self.reg_name}.{self.name}[{self.msb}:{self.lsb}]{access} @ +0x{self.offset:X}>"


class InlineRegister:
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
        fields = fields or {}
        w1c_names = set(w1c)
        unknown = sorted(w1c_names - set(fields))
        if unknown:
            raise MmregError(
                "E_SUBREG_NOT_FOUND",
                f"[E_SUBREG_NOT_FOUND] w1c field(s) {unknown} not defined in register '{name}'.",
            )
        parsed_fields: Dict[str, InlineField] = {}
        for field_name, spec in fields.items():
            _check_attr_name(field_name, InlineRegister, f"Field name in register '{name}'")
            msb, lsb = _parse_bit_spec(spec)
            parsed_fields[field_name] = InlineField(
                device, name, int(offset), field_name, msb, lsb, w1c=field_name in w1c_names
            )
        w1c_mask = 0
        for field in parsed_fields.values():
            if field.w1c:
                w1c_mask |= field.mask
        for field in parsed_fields.values():
            field._other_w1c_mask = w1c_mask & ~field.mask
        object.__setattr__(self, "_fields", parsed_fields)

    def read(self) -> int:
        """Reads the 32-bit register from hardware."""
        return self._device.read_u32(self.offset)

    def write(self, value: int) -> None:
        """Writes `value` to the 32-bit register in hardware."""
        self._device.write_u32(self.offset, int(value))

    def __getattr__(self, item: str) -> InlineField:
        fields: Dict[str, InlineField] = object.__getattribute__(self, "_fields")
        if item in fields:
            return fields[item]
        reg_name = object.__getattribute__(self, "name")
        avail = ", ".join(sorted(fields.keys()))
        raise MmregError(
            "E_SUBREG_NOT_FOUND",
            f"[E_SUBREG_NOT_FOUND] Field '{item}' not found in register '{reg_name}'. Available: [{avail}]",
        )

    def __setattr__(self, key: str, value: int) -> None:
        fields: Dict[str, InlineField] = object.__getattribute__(self, "_fields")
        if key in fields:
            fields[key].write(int(value))
            return
        reg_name = object.__getattribute__(self, "name")
        avail = ", ".join(sorted(fields.keys()))
        raise MmregError(
            "E_SUBREG_NOT_FOUND",
            f"[E_SUBREG_NOT_FOUND] Field '{key}' not found in register '{reg_name}'. Available: [{avail}]",
        )

    def __int__(self) -> int:
        return self.read()

    def __index__(self) -> int:
        return self.read()

    def __eq__(self, other: object) -> bool:
        if isinstance(other, (int, InlineRegister)):
            return int(self) == int(other)
        return NotImplemented

    # Defining `__eq__` makes instances unhashable; that is intended, as equality reads hardware.
    __hash__ = None  # type: ignore[assignment]

    def __repr__(self) -> str:
        fields: Dict[str, InlineField] = object.__getattribute__(self, "_fields")
        return f"<InlineRegister {self.name} @ +0x{self.offset:X} fields=[{', '.join(fields)}]>"


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

    def __init__(self, handle: int, lib: ctypes.CDLL) -> None:
        object.__setattr__(self, "_handle", handle)
        object.__setattr__(self, "_lib", lib)
        object.__setattr__(self, "_registers", {})

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
        flags = _build_flags(region=region, read_only=read_only, force=force, mock=mock)
        handle = lib.mmreg_open_uio(name.encode("utf-8"), flags)
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
        flags = _build_flags(region=region, read_only=read_only, force=force, mock=mock)
        handle = lib.mmreg_open_uio_path(path.encode("utf-8"), flags)
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
        flags = _build_flags(region=0, read_only=read_only, force=force, mock=mock)
        handle = lib.mmreg_open_addr(int(base), int(size), flags)
        if not handle:
            _raise_last_error(lib)
        return cls(handle, lib)

    def _require_handle(self) -> int:
        handle: int = object.__getattribute__(self, "_handle")
        if not handle:
            raise MmregError("E_INVALID_ARG", "[E_INVALID_ARG] Operation on a closed mmreg.Device.")
        return handle

    def close(self) -> None:
        """Unmaps and closes the device window (idempotent)."""
        handle: int = object.__getattribute__(self, "_handle")
        if handle:
            lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
            object.__setattr__(self, "_handle", 0)
            lib.mmreg_close(handle)

    def __del__(self) -> None:
        try:
            self.close()
        except Exception:
            pass

    def __enter__(self) -> Device:
        self._require_handle()
        return self

    def __exit__(self, exc_type: object, exc_val: object, exc_tb: object) -> None:
        self.close()

    @property
    def base_address(self) -> int:
        """Physical base address of the mapped window."""
        handle = self._require_handle()
        lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
        return int(lib.mmreg_base_address(handle))

    @property
    def size(self) -> int:
        """Size of the mapped window in bytes."""
        handle = self._require_handle()
        lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
        return int(lib.mmreg_size(handle))

    @property
    def warnings(self) -> List[str]:
        """Non-fatal hardware pre-flight warnings collected when the device was opened."""
        handle = self._require_handle()
        lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
        count = int(lib.mmreg_warning_count(handle))
        out: List[str] = []
        for idx in range(count):
            ptr = lib.mmreg_warning(handle, idx)
            if ptr:
                out.append(ptr.decode("utf-8", errors="replace"))
        return out

    def read_u8(self, offset: int) -> int:
        handle = self._require_handle()
        lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
        val = ctypes.c_uint8()
        if lib.mmreg_rd8(handle, _check_uint(offset, 32, "Offset"), ctypes.byref(val)) != 0:
            _raise_last_error(lib)
        return int(val.value)

    def read_u16(self, offset: int) -> int:
        handle = self._require_handle()
        lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
        val = ctypes.c_uint16()
        if lib.mmreg_rd16(handle, _check_uint(offset, 32, "Offset"), ctypes.byref(val)) != 0:
            _raise_last_error(lib)
        return int(val.value)

    def read_u32(self, offset: int) -> int:
        handle = self._require_handle()
        lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
        val = ctypes.c_uint32()
        if lib.mmreg_rd32(handle, _check_uint(offset, 32, "Offset"), ctypes.byref(val)) != 0:
            _raise_last_error(lib)
        return int(val.value)

    def read_u64(self, offset: int) -> int:
        handle = self._require_handle()
        lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
        val = ctypes.c_uint64()
        if lib.mmreg_rd64(handle, _check_uint(offset, 32, "Offset"), ctypes.byref(val)) != 0:
            _raise_last_error(lib)
        return int(val.value)

    def write_u8(self, offset: int, value: int) -> None:
        handle = self._require_handle()
        lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
        if lib.mmreg_wr8(handle, _check_uint(offset, 32, "Offset"), _check_uint(value, 8, "Value")) != 0:
            _raise_last_error(lib)

    def write_u16(self, offset: int, value: int) -> None:
        handle = self._require_handle()
        lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
        if lib.mmreg_wr16(handle, _check_uint(offset, 32, "Offset"), _check_uint(value, 16, "Value")) != 0:
            _raise_last_error(lib)

    def write_u32(self, offset: int, value: int) -> None:
        handle = self._require_handle()
        lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
        if lib.mmreg_wr32(handle, _check_uint(offset, 32, "Offset"), _check_uint(value, 32, "Value")) != 0:
            _raise_last_error(lib)

    def write_u64(self, offset: int, value: int) -> None:
        handle = self._require_handle()
        lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
        if lib.mmreg_wr64(handle, _check_uint(offset, 32, "Offset"), _check_uint(value, 64, "Value")) != 0:
            _raise_last_error(lib)

    def update_u32(self, offset: int, mask: int, value: int) -> int:
        """Atomically updates bits in `mask` at `offset` (`(old & ~mask) | (value & mask)`) under one lock."""
        handle = self._require_handle()
        lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
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
        lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
        if lib.mmreg_lock(handle) != 0:
            _raise_last_error(lib)

    def _release_lock(self) -> None:
        handle = self._require_handle()
        lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
        if lib.mmreg_unlock(handle) != 0:
            _raise_last_error(lib)

    def lock(self) -> LockGuard:
        """Returns a reentrant context manager (`with dev.lock():`) holding the device lock."""
        return LockGuard(self)

    def irq_unmask(self) -> None:
        """Unmasks (re-enables) the UIO interrupt line by writing `1u32` to `/dev/uioN`."""
        handle = self._require_handle()
        lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
        if lib.mmreg_irq_unmask(handle) != 0:
            _raise_last_error(lib)

    def irq_wait(self, timeout_ms: Optional[int]) -> Optional[int]:
        """Waits up to `timeout_ms` ms (`None`: no timeout) for a UIO interrupt.

        Returns the cumulative interrupt count, or `None` when the timeout elapsed.
        """
        handle = self._require_handle()
        lib: ctypes.CDLL = object.__getattribute__(self, "_lib")
        timeout = _U64_MAX if timeout_ms is None else _check_uint(timeout_ms, 64, "Timeout (ms)")
        count = ctypes.c_uint32()
        rc = lib.mmreg_irq_wait(handle, timeout, ctypes.byref(count))
        if rc == 1:
            return int(count.value)
        if rc == 0:
            return None
        _raise_last_error(lib)
        return None

    def define(
        self,
        name: str,
        *,
        offset: int,
        fields: Optional[Dict[str, Union[str, int, Tuple[int, int]]]] = None,
        w1c: Iterable[str] = (),
    ) -> InlineRegister:
        """Defines an inline 32-bit register on this device.

        `d.define("ISR", offset=0xC, fields={"DONE": "0", "ERR": "1", "MODE": "7:4"}, w1c={"DONE", "ERR"})`
        marks `DONE` and `ERR` write-1-to-clear: writing them never read-modify-writes, and writing
        `MODE` writes `0` to them so pending bits are not cleared by accident.
        """
        self._require_handle()
        _check_attr_name(name, Device, "Register name")
        reg = InlineRegister(self, name, _check_uint(offset, 32, "Offset"), fields, w1c)
        regs: Dict[str, InlineRegister] = object.__getattribute__(self, "_registers")
        regs[name] = reg
        return reg

    def __getattr__(self, item: str) -> InlineRegister:
        regs: Dict[str, InlineRegister] = object.__getattribute__(self, "_registers")
        if item in regs:
            return regs[item]
        raise AttributeError(f"'{type(self).__name__}' object has no attribute '{item}'")

    def __setattr__(self, key: str, value: object) -> None:
        regs: Dict[str, InlineRegister] = object.__getattribute__(self, "_registers")
        if key in regs:
            if isinstance(value, InlineRegister):
                return
            regs[key].write(int(value))  # type: ignore[arg-type]
            return
        object.__setattr__(self, key, value)


def _check_attr_name(name: str, owner: type, what: str) -> None:
    """Rejects names that are not identifiers or that would shadow an attribute of `owner`."""
    if not name.isidentifier() or name.startswith("_") or hasattr(owner, name) or name in ("name", "offset"):
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

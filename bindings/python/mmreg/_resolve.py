"""Finding block and instance files and the hardware: `mmreg.open()` and `mmreg.list()`."""

from __future__ import annotations

import os
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Dict, Iterable, List, Optional, Tuple, Union

from . import Device, MmregError, _MmregOSError
from ._access import attach_block
from .block import Block, CheckSpec, InstanceSpec, load_instance_file, merge_instances

# `typing.Iterable`: a module-level alias is evaluated at import, and `collections.abc` types are
# subscriptable only from Python 3.9 (`requires-python = ">=3.8"`).
PathArg = Optional[Union[str, Path, Iterable[Union[str, Path]]]]

_DEFAULT_SYSFS_UIO = "/sys/class/uio"
_DEFAULT_DEV_ROOT = "/dev"
# Placeholder physical base of a mock window for a UIO device whose sysfs `maps/mapN` is unreadable.
_MOCK_BASE = 0x4000_0000
_MIN_AT_WINDOW = 0x1000


def _os_error(code: str, detail: str) -> _MmregOSError:
    return _MmregOSError(code, f"[{code}] {detail}")


def _paths(arg: PathArg) -> List[Path]:
    if arg is None:
        return []
    if isinstance(arg, (str, Path)):
        return [Path(arg)]
    return [Path(p) for p in arg]


def _env_paths(var: str) -> List[Path]:
    return [Path(p) for p in os.environ.get(var, "").split(":") if p.strip()]


def _expand_json_files(paths: Iterable[Path]) -> List[Path]:
    """Each directory becomes its `*.json` files (sorted); files are kept as given."""
    files: List[Path] = []
    for p in paths:
        files.extend(sorted(p.glob("*.json")) if p.is_dir() else [p])
    return files


def get_block_search_dirs(blocks_path: PathArg = None) -> List[Path]:
    """`blocks_path`, then `$MMREG_BLOCKS`, `/etc/mmreg/blocks`, `/usr/share/mmreg/blocks`."""
    return [
        *_paths(blocks_path),
        *_env_paths("MMREG_BLOCKS"),
        Path("/etc/mmreg/blocks"),
        Path("/usr/share/mmreg/blocks"),
    ]


def get_instance_search_files(instances_path: PathArg = None) -> List[Path]:
    """`instances_path` if given, else `$MMREG_INSTANCES` then `/etc/mmreg/instances.d/*.json`."""
    if instances_path is not None:
        return _expand_json_files(_paths(instances_path))
    etc_dir = Path("/etc/mmreg/instances.d")
    return _expand_json_files([*_env_paths("MMREG_INSTANCES"), *([etc_dir] if etc_dir.is_dir() else [])])


def load_all_instances(instances_path: PathArg = None) -> Dict[str, InstanceSpec]:
    """Loads every instance file on the search path, keyed by upper-case instance name.

    Raises `E_INSTANCE_DUPLICATE` if two files define one instance name.
    """
    merged: Dict[str, InstanceSpec] = {}
    for file_path in get_instance_search_files(instances_path):
        if not file_path.is_file():
            # Only files named explicitly ($MMREG_INSTANCES, `instances_path`) can be missing.
            raise _os_error("E_BLOCK_NOT_FOUND", f"Instance file '{file_path}' not found.")
        merge_instances(merged, load_instance_file(file_path))
    return merged


def _find_block_file(stems: Sequence[str], blocks_path: PathArg) -> Optional[Path]:
    """The first `<stem>.json` on the block search path, trying `stems` in order."""
    search_dirs = get_block_search_dirs(blocks_path)
    for stem in stems:
        for directory in search_dirs:
            candidate = directory / f"{stem}.json"
            if candidate.is_file():
                return candidate
    return None


def resolve_block(block: Union[str, Path, Block], *, blocks_path: PathArg = None) -> Block:
    """Resolves `block`: a `Block`, a file path, or a `compatible` looked up on the block search path."""
    if isinstance(block, Block):
        return block
    if isinstance(block, Path):
        return Block.from_file(block)
    if not isinstance(block, str) or not block.strip():
        raise MmregError("E_INVALID_ARG", f"[E_INVALID_ARG] Invalid block specifier {block!r}.")
    text = block.strip()
    # A path (a `compatible` never contains a separator), or an existing `*.json` file.
    if "/" in text or "\\" in text or (text.endswith(".json") and Path(text).is_file()):
        return Block.from_file(text)
    stem = text[:-5] if text.endswith(".json") else text
    found = _find_block_file([stem], blocks_path)
    if found is None:
        searched = ", ".join(str(d) for d in get_block_search_dirs(blocks_path))
        raise _os_error("E_BLOCK_NOT_FOUND", f"Block file '{stem}.json' not found in [{searched}].")
    return Block.from_file(found)


@dataclass(frozen=True)
class SysfsUioEntry:
    uio_dir: Path
    dev_name: str  # e.g. "uio0"
    name: str  # contents of /sys/class/uio/uio0/name
    compatibles: Tuple[str, ...]

    def read_map(self, region: int) -> Optional[Tuple[int, int]]:
        """`(addr, size)` of `maps/map<region>`, or `None` if it cannot be read."""
        map_dir = self.uio_dir / "maps" / f"map{region}"
        try:
            return (
                int((map_dir / "addr").read_text(encoding="utf-8").strip(), 0),
                int((map_dir / "size").read_text(encoding="utf-8").strip(), 0),
            )
        except (OSError, ValueError):
            return None


def _scan_sysfs_uio(sysfs_uio: Union[str, Path] = _DEFAULT_SYSFS_UIO) -> List[SysfsUioEntry]:
    root = Path(sysfs_uio)
    if not root.is_dir():
        return []
    entries: List[SysfsUioEntry] = []
    for child in sorted(root.iterdir(), key=lambda p: p.name):
        if not child.name.startswith("uio"):
            continue
        try:
            uio_name = (child / "name").read_text(encoding="utf-8").strip()
        except OSError:
            continue
        try:
            raw = (child / "device" / "of_node" / "compatible").read_bytes()
        except OSError:
            raw = b""
        compatibles = tuple(p.decode("utf-8", errors="replace").strip() for p in raw.split(b"\x00") if p.strip())
        entries.append(SysfsUioEntry(uio_dir=child, dev_name=child.name, name=uio_name, compatibles=compatibles))
    return entries


def _find_sysfs_uio_by_name(uio_name: str, sysfs_uio: Union[str, Path]) -> Optional[SysfsUioEntry]:
    return next((e for e in _scan_sysfs_uio(sysfs_uio) if e.name == uio_name), None)


def _find_block_for_compatibles(compatibles: Sequence[str], *, blocks_path: PathArg = None) -> Optional[Block]:
    found = _find_block_file(compatibles, blocks_path)
    return None if found is None else Block.from_file(found)


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
        return "\n".join(str(item) for item in self) if self else "<no instances found>"


def list_instances(
    *,
    blocks_path: PathArg = None,
    instances_path: PathArg = None,
    sysfs_uio: Union[str, Path] = _DEFAULT_SYSFS_UIO,
    dev_root: Union[str, Path] = _DEFAULT_DEV_ROOT,
) -> InstanceList:
    """Lists the instances of the instance files and the UIO devices with a block file, and whether
    each device is present."""
    instances = load_all_instances(instances_path)
    uio_entries = _scan_sysfs_uio(sysfs_uio)
    uio_by_name = {e.name: e for e in uio_entries}
    dev_root_path = Path(dev_root)
    # With the default /dev, the sysfs entry is enough; a custom root (tests) must hold the node too.
    check_nodes = dev_root_path != Path(_DEFAULT_DEV_ROOT)

    def node_present(node: str) -> bool:
        return not check_nodes or (dev_root_path / node).exists()

    results = InstanceList()
    for inst in instances.values():
        try:
            block_label = resolve_block(inst.block, blocks_path=blocks_path).compatible
            block_ok = True
        except MmregError:
            block_label, block_ok = inst.block, False
        if inst.uio is not None:
            entry = uio_by_name.get(inst.uio)
            device_ok = entry is not None and node_present(entry.dev_name)
            target = f"uio:{inst.uio}" if inst.uio_map == 0 else f"uio:{inst.uio}[{inst.uio_map}]"
        else:
            device_ok = node_present("mem")
            target = f"at:0x{inst.at:X}"
        results.append(InstanceInfo(inst.name, block_label, target, block_ok and device_ok, inst.source, inst.desc))

    covered = {inst.uio for inst in instances.values() if inst.uio is not None}
    for entry in uio_entries:
        if entry.name in covered or entry.name.upper() in instances:
            continue
        blk = _find_block_for_compatibles(entry.compatibles, blocks_path=blocks_path)
        if blk is not None:
            results.append(
                InstanceInfo(
                    entry.name,
                    blk.compatible,
                    f"uio:{entry.name}",
                    node_present(entry.dev_name),
                    str(entry.uio_dir),
                    blk.desc,
                )
            )
    return results


def _run_instance_checks(inst: InstanceSpec, check_reader: Optional[Callable[[int], int]]) -> None:
    for chk in inst.checks:
        val = _read_check(chk, check_reader)
        if (val & chk.mask) != chk.expected:
            raise MmregError(
                "E_VALIDATION_HOOK_FAILED",
                f"[E_VALIDATION_HOOK_FAILED] {inst.source} (register '{inst.name}', field '{chk.name}'): "
                f"Pre-access check '{chk.name}' at 0x{chk.addr:X} failed: "
                f"read 0x{val:08X} (masked 0x{val & chk.mask:08X}), expected 0x{chk.expected:08X}.",
            )


def _read_check(chk: CheckSpec, check_reader: Optional[Callable[[int], int]]) -> int:
    if check_reader is not None:
        return int(check_reader(chk.addr)) & 0xFFFF_FFFF
    # Outside a UIO region this needs a `debug` libmmreg (/dev/mem), else E_DEBUG_DISABLED.
    page_base = chk.addr & ~0xFFF
    with Device.open_addr(page_base, chk.addr - page_base + 4, read_only=True) as probe:
        return probe.read_u32(chk.addr - page_base)


def _open_uio(
    uio_name: str,
    *,
    uio_map: int,
    read_only: bool,
    force: bool,
    mock: bool,
    sysfs_uio: Union[str, Path],
    dev_root: Union[str, Path],
    mock_size: Optional[int],
) -> Device:
    if uio_name.startswith("/"):
        # `mock` needs `force` in libmmreg.
        return Device.open_uio_path(uio_name, region=uio_map, read_only=read_only, force=force or mock, mock=mock)
    custom_sysfs = Path(sysfs_uio) != Path(_DEFAULT_SYSFS_UIO)
    entry = _find_sysfs_uio_by_name(uio_name, sysfs_uio)
    if custom_sysfs and entry is None:
        raise _os_error("E_UIO_NOT_FOUND", f"UIO device '{uio_name}' not found in '{sysfs_uio}'.")
    if mock and entry is not None:
        # A mock window at the device's sysfs address (else a placeholder one) of the device's size.
        window = entry.read_map(uio_map)
        if window is not None:
            return Device.open_addr(window[0], window[1], read_only=read_only, force=True, mock=True)
        if mock_size is not None:
            return Device.open_addr(_MOCK_BASE, mock_size, read_only=read_only, force=True, mock=True)
    if custom_sysfs and not mock:
        node = str(Path(dev_root) / entry.dev_name)  # type: ignore[union-attr]
        return Device.open_uio_path(node, region=uio_map, read_only=read_only, force=force)
    return Device.open_uio(uio_name, region=uio_map, read_only=read_only, force=force or mock, mock=mock)


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
    blocks_path: PathArg = None,
    instances_path: PathArg = None,
    sysfs_uio: Union[str, Path] = _DEFAULT_SYSFS_UIO,
    dev_root: Union[str, Path] = _DEFAULT_DEV_ROOT,
    check_reader: Optional[Callable[[int], int]] = None,
) -> Device:
    """Opens a device by instance or UIO `name`, or with `uio=` / `at=` (and optionally `block=`).

    An instance's `checks` run before its window is mapped, except with `force=True` or `mock=True`
    (no hardware to check), as for the library's pre-flight checks. `check_reader(addr) -> int`
    replaces their hardware reads (tests) and makes them run in every mode.
    """
    if name is not None and (block is not None or uio is not None or at is not None):
        raise MmregError(
            "E_INVALID_ARG",
            "[E_INVALID_ARG] Pass either a positional `name` or keyword `block`/`uio`/`at`, not both.",
        )
    if uio is not None and at is not None:
        raise MmregError("E_INVALID_ARG", "[E_INVALID_ARG] Specify either `uio` or `at`, not both.")

    resolved: Optional[Block] = None
    if name is not None:
        if not isinstance(name, str) or not name.strip():
            raise MmregError("E_INVALID_ARG", f"[E_INVALID_ARG] Device name must be a non-empty string, got {name!r}.")
        # 1. instance files, 2. /sys/class/uio/*/name -> of_node/compatible -> block file.
        inst = load_all_instances(instances_path).get(name.upper())
        if inst is not None:
            resolved = resolve_block(inst.block, blocks_path=blocks_path)
            if inst.checks and (check_reader is not None or not (force or mock)):
                _run_instance_checks(inst, check_reader)
            uio, uio_map, at, size = inst.uio, inst.uio_map, inst.at, inst.size
        else:
            resolved = _block_for_uio_name(name, sysfs_uio=sysfs_uio, blocks_path=blocks_path)
            uio = name
    elif uio is None and at is None:
        raise MmregError(
            "E_INVALID_ARG", "[E_INVALID_ARG] `mmreg.open()` requires either `name`, `uio=...`, or `at=...`."
        )
    elif block is not None:
        resolved = resolve_block(block, blocks_path=blocks_path)

    # Mock window of a UIO device without a readable sysfs map: the block's registers.
    block_size = resolved.min_window_size if resolved is not None else None
    if uio is not None:
        dev = _open_uio(
            uio,
            uio_map=uio_map,
            read_only=read_only,
            force=force,
            mock=mock,
            sysfs_uio=sysfs_uio,
            dev_root=dev_root,
            mock_size=block_size if block_size is not None else size,
        )
    else:
        window = size
        if window is None and resolved is not None:
            # The block's `size`, else at least a page, leaving room for `define()` next to the block.
            window = resolved.size or max(resolved.min_window_size, _MIN_AT_WINDOW)
        if window is None:
            raise MmregError(
                "E_INVALID_ARG", "[E_INVALID_ARG] `mmreg.open(at=...)` without a block file requires `size=...`."
            )
        dev = Device.open_addr(at, window, read_only=read_only, force=force, mock=mock)  # type: ignore[arg-type]
    if resolved is not None:
        try:
            attach_block(dev, resolved)
        except BaseException:
            dev.close()
            raise
    return dev


def _block_for_uio_name(name: str, *, sysfs_uio: Union[str, Path], blocks_path: PathArg) -> Block:
    entry = _find_sysfs_uio_by_name(name, sysfs_uio)
    if entry is None:
        raise _os_error("E_UIO_NOT_FOUND", f"'{name}' was not found in instance files or UIO sysfs ('{sysfs_uio}').")
    if not entry.compatibles:
        raise _os_error(
            "E_BLOCK_NOT_FOUND",
            f"UIO device '{name}' ({entry.dev_name}) has no device/of_node/compatible property to locate a block file.",
        )
    blk = _find_block_for_compatibles(entry.compatibles, blocks_path=blocks_path)
    if blk is None:
        searched = ", ".join(str(d) for d in get_block_search_dirs(blocks_path))
        raise _os_error(
            "E_BLOCK_NOT_FOUND",
            f"No block file found for UIO device '{name}' (compatible: [{', '.join(entry.compatibles)}]) "
            f"in [{searched}].",
        )
    return blk

"""
CLI entry point for `python -m mmreg`:

    python -m mmreg check <file.json> [<file2.json> ...]

Validates `mmreg-block/1` and `mmreg-instances/1` JSON files without touching hardware.
"""

from __future__ import annotations

import argparse
from collections.abc import Sequence
import sys
from typing import Dict, List, Optional

from . import MmregError
from .block import Block, InstanceSpec, _err, check_file


def _cmd_check(files: Sequence[str]) -> int:
    seen_instances: Dict[str, InstanceSpec] = {}
    errors = 0

    for file_path in files:
        try:
            result = check_file(file_path)
            if isinstance(result, Block):
                print(
                    f"OK: {file_path} (mmreg-block, compatible={result.compatible!r}, "
                    f"{len(result.flat_registers)} register(s))"
                )
            else:
                for name, spec in result.items():
                    up = name.upper()
                    if up in seen_instances:
                        prev = seen_instances[up]
                        raise _err(
                            "E_INSTANCE_DUPLICATE",
                            f"Duplicate instance '{name}' defined in both '{prev.source}' and '{spec.source}'.",
                            source=spec.source,
                            reg=name,
                        )
                    seen_instances[up] = spec
                print(f"OK: {file_path} (mmreg-instances, {len(result)} instance(s))")
        except MmregError as exc:
            print(str(exc), file=sys.stderr)
            errors += 1

    return 1 if errors else 0


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m mmreg",
        description="mmreg block and instance file validator.",
    )
    sub = parser.add_subparsers(dest="command", required=True)
    check_parser = sub.add_parser(
        "check",
        help="Validate one or more mmreg-block/1 or mmreg-instances/1 JSON files.",
    )
    check_parser.add_argument(
        "files",
         nargs="+",
        metavar="FILE",
        help="Path(s) to block or instance JSON files.",
    )

    args = parser.parse_args(list(argv) if argv is not None else None)
    if args.command == "check":
        return _cmd_check(args.files)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())

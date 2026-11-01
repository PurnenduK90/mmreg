"""Register access by name from a block file and an instance file, on an in-memory register window:
runs without hardware.

    python3 by_name.py

`blocks/acme,gpio-1.0.json` describes the IP block and `instances/board.json` places it at an
address. On hardware, install them as `/usr/share/mmreg/blocks/acme,gpio-1.0.json` and
`/etc/mmreg/instances.d/board.json` and call `mmreg.open("gpio0")`; with a Device Tree
`compatible = "acme,gpio-1.0", "generic-uio"` the block file alone is enough.
"""

from pathlib import Path

import mmreg
from mmreg import MmregError

HERE = Path(__file__).resolve().parent
PATHS = {"blocks_path": HERE / "blocks", "instances_path": HERE / "instances"}


def main() -> None:
    print(mmreg.list(**PATHS))

    # `mock=True` backs the window with plain memory (no write-1-to-clear or read-to-clear
    # behaviour). `force` and `mock` skip the instance's `checks`, which need the real SoC.
    with mmreg.open("gpio0", mock=True, force=True, **PATHS) as d:
        d.CTRL.EN = 1
        d.CTRL.MODE = "OPEN_DRAIN"  # enum names, case-insensitive
        d.DEBOUNCE[3].CYCLES = 10  # register arrays
        print(d.CTRL)  # decoded: CTRL @ 0x08 = 0x0000_0003  (EN=1, MODE=OPEN_DRAIN, RESTART=0)
        print(f"MODE is {d.CTRL.MODE} ({int(d.CTRL.MODE)}), open drain: {d.CTRL.MODE == 'OPEN_DRAIN'}")

        # Field writes are masked read-modify-writes: clearing one IRQ_STATUS bit writes 0 to the others.
        d.IRQ_STATUS.CHANGE = 0x01
        # Reading EVENT_FIFO pops an event, so it needs `side_effects=True`.
        print(f"EVENT_FIFO = {d.EVENT_FIFO.read(side_effects=True):#x}")

        # Access rules raise MmregError with a stable code.
        for refused in (
            lambda: setattr(d.VERSION, "MAJOR", 2),  # read-only field
            lambda: int(d.EVENT_FIFO),  # read side effect without side_effects=True
            lambda: setattr(d.CTRL, "MODE", "TRISTATE"),  # not an enum entry
            lambda: d.DEBOUNCE[8],  # outside the array
        ):
            try:
                refused()
            except MmregError as err:
                print(f"[{err.code}]")

        # Registers that differ from their reset value; EVENT_FIFO is skipped, not read.
        print(d.dump(diff_reset=True))


if __name__ == "__main__":
    main()

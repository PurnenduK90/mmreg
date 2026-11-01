"""Interrupt loop on a real UIO device (needs the hardware; see `quickstart.py` otherwise).

    python3 uio_irq.py <uio name> [count]      # e.g. `python3 uio_irq.py axi_timer 10`

The peripheral is assumed to have an `ISR` register at 0x0C with write-1-to-clear `DONE` (bit 0)
and `ERR` (bit 1) bits; adapt the definition to your IP.
"""

import sys

from mmreg import Device, MmregError


def main(argv: list) -> int:
    if len(argv) < 2:
        print("usage: uio_irq.py <uio name> [count]", file=sys.stderr)
        return 2
    count = int(argv[2]) if len(argv) > 2 else 5
    try:
        with Device.open_uio(argv[1]) as d:
            for warning in d.warnings:
                print(warning, file=sys.stderr)
            d.define("ISR", offset=0x0C, fields={"DONE": "0", "ERR": "1"}, w1c={"DONE", "ERR"})
            print(f"{argv[1]} at {d.base_address:#x} ({d.size} bytes): waiting for {count} interrupts")

            d.irq_unmask()
            seen = 0
            while seen < count:
                total = d.irq_wait(5000)  # milliseconds; None waits without a timeout
                if total is None:
                    print("no interrupt in 5 s")
                    continue
                seen += 1
                isr = int(d.ISR)
                print(f"interrupt {seen} (kernel count {total}): ISR = {isr:#010x}")

                # Level-triggered sources: clear the cause in the device before unmasking.
                # Field writes are masked, so clearing one bit never touches the others.
                if d.ISR.DONE == 1:
                    d.ISR.DONE = 1
                if d.ISR.ERR == 1:
                    d.ISR.ERR = 1
                d.irq_unmask()
    except MmregError as err:
        print(err, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

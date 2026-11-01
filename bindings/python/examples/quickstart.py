"""Tour of the mmreg Python binding on an in-memory register window: runs without hardware.

    python3 quickstart.py

`mock=True` backs the device with plain memory (it does not model write-1-to-clear bits). On
hardware, open the device with `Device.open_uio("<uio name>")` instead (see `uio_irq.py`). Access
by name from block files: `by_name.py`.
"""

import copy

from mmreg import Device, MmregError


def main() -> None:
    with Device.open_addr(0x4000_0000, 0x100, force=True, mock=True) as d:
        # 8, 16, 32 and 64-bit accesses (each naturally aligned).
        d.write_u8(0x20, 0xAB)
        d.write_u16(0x22, 0xCAFE)
        d.write_u64(0x28, 0x0102_0304_0506_0708)
        print(f"u8 {d.read_u8(0x20):#04x}, u16 {d.read_u16(0x22):#06x}, u64 {d.read_u64(0x28):#018x}")

        # Locked read-modify-write of bits 7:4.
        print(f"CTRL = {d.update_u32(0x00, 0xF0, 0x5 << 4):#010x}")

        # A multi-register sequence under one lock (reentrant).
        with d.lock():
            d.write_u32(0x10, d.read_u32(0x00))

        # Inline register definitions: access fields by name.
        d.define("CTRL", offset=0x00, fields={"EN": "0", "MODE": "7:4"})
        d.CTRL.EN = True  # 1-bit fields accept bool
        d.CTRL.MODE = 3
        print(f"CTRL = {int(d.CTRL):#010x}: EN = {int(d.CTRL.EN)}, MODE = {int(d.CTRL.MODE)}")

        # Write-1-to-clear fields: every field write is a masked read-modify-write, so clearing
        # DONE keeps the read-write IRQ_EN and writes 0 to ERR, leaving a pending ERR alone.
        d.define("ISR", offset=0x0C, fields={"DONE": "0", "ERR": "1", "IRQ_EN": "8"}, w1c={"DONE", "ERR"})
        d.ISR.IRQ_EN = 1
        written = d.ISR.DONE.write(1)
        print(f"clearing ISR.DONE wrote {written:#010x} (IRQ_EN kept, ERR written as 0)")
        print(repr(d.ISR))  # repr never reads hardware

        # Every error is an MmregError with a stable code.
        for bad in (
            lambda: d.read_u32(0x02),
            lambda: d.write_u8(0x00, 0x100),
            lambda: setattr(d.CTRL, "MODE", 0x10),
            lambda: d.CTRL.NOPE,
            lambda: copy.copy(d),
        ):
            try:
                bad()
            except MmregError as err:
                print(f"[{err.code}]")


if __name__ == "__main__":
    main()

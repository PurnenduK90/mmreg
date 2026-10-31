"""Unit tests for the pure-Python `mmreg` ctypes binding."""

import unittest

from mmreg import Device, MmregError, find_uio


class TestMmregPythonBinding(unittest.TestCase):
    def test_open_addr_widths_update_lock_and_inline_define(self) -> None:
        with Device.open_addr(0x4000_0000, 0x100, force=True, mock=True) as dev:
            self.assertEqual(dev.base_address, 0x4000_0000)
            self.assertEqual(dev.size, 0x100)
            self.assertEqual(dev.warnings, [])

            # 8, 16, 32, 64-bit access
            dev.write_u8(0x00, 0xAB)
            self.assertEqual(dev.read_u8(0x00), 0xAB)

            dev.write_u16(0x02, 0xCAFE)
            self.assertEqual(dev.read_u16(0x02), 0xCAFE)

            dev.write_u32(0x04, 0x1234_5678)
            self.assertEqual(dev.read_u32(0x04), 0x1234_5678)
            updated = dev.update_u32(0x04, 0x0000_FF00, 0x0000_AA00)
            self.assertEqual(updated, 0x1234_AA78)

            dev.write_u64(0x10, 0x0102_0304_0506_0708)
            self.assertEqual(dev.read_u64(0x10), 0x0102_0304_0506_0708)

            # Reentrant sequence lock context manager
            with dev.lock():
                with dev.lock():
                    dev.write_u32(0x18, 0xDEAD_BEEF)
                    self.assertEqual(dev.read_u32(0x18), 0xDEAD_BEEF)

            # Inline register and field definitions (`d.define`)
            dev.define("CTRL", offset=0x08, fields={"EN": "0", "MODE": "7:4"})
            dev.CTRL = 0
            self.assertEqual(int(dev.CTRL), 0)
            dev.CTRL.EN = 1
            dev.CTRL.MODE = 0x5
            self.assertEqual(int(dev.CTRL.EN), 1)
            self.assertEqual(int(dev.CTRL.MODE), 0x5)
            self.assertEqual(int(dev.CTRL), 0x51)

            # Unknown inline field raises E_SUBREG_NOT_FOUND
            with self.assertRaises(MmregError) as ctx:
                _ = dev.CTRL.NONEXISTENT
            self.assertEqual(ctx.exception.code, "E_SUBREG_NOT_FOUND")

            # Unaligned access raises E_UNALIGNED_ACCESS
            with self.assertRaises(MmregError) as ctx:
                dev.read_u32(0x02)
            self.assertEqual(ctx.exception.code, "E_UNALIGNED_ACCESS")

            # Out-of-bounds access raises E_OUT_OF_BOUNDS
            with self.assertRaises(MmregError) as ctx:
                dev.read_u32(0x100)
            self.assertEqual(ctx.exception.code, "E_OUT_OF_BOUNDS")

    def test_value_range_checks(self) -> None:
        with Device.open_addr(0x4000_0000, 0x100, force=True, mock=True) as dev:
            dev.define("CTRL", offset=0x08, fields={"MODE": "7:4"})
            dev.CTRL = 0
            # A value wider than the field is rejected, not silently truncated.
            with self.assertRaises(MmregError) as ctx:
                dev.CTRL.MODE = 0x15
            self.assertEqual(ctx.exception.code, "E_INVALID_ARG")
            self.assertEqual(int(dev.CTRL), 0)

            for bad in (
                lambda: dev.write_u8(0, 0x100),
                lambda: dev.write_u32(0, -1),
                lambda: dev.write_u64(0, 1 << 64),
                lambda: dev.update_u32(0, 1 << 32, 0),
                lambda: dev.read_u32(-4),
                lambda: dev.irq_wait(-1),
            ):
                with self.assertRaises(MmregError) as ctx:
                    bad()
                self.assertEqual(ctx.exception.code, "E_INVALID_ARG")

    def test_w1c_fields_and_side_effect_free_repr(self) -> None:
        with Device.open_addr(0x4000_0000, 0x100, force=True, mock=True) as dev:
            dev.define("ISR", offset=0x0C, fields={"DONE": "0", "ERR": "1", "MODE": "7:4"}, w1c={"DONE", "ERR"})
            dev.write_u32(0x0C, 0x3)  # both w1c bits pending

            # Writing an rw field writes 0 to the w1c bits instead of writing back the pending 1s.
            self.assertEqual(dev.update_u32(0x0C, 0, 0), 0x3)
            dev.ISR.MODE = 0x2
            self.assertEqual(dev.read_u32(0x0C), 0x20)

            # Writing a w1c field is a direct write of only that field's bits.
            self.assertEqual(dev.ISR.DONE.write(1), 0x1)
            self.assertEqual(dev.read_u32(0x0C), 0x1)

            # repr never reads hardware.
            self.assertIn("DONE[0:0] w1c", repr(dev.ISR.DONE))
            self.assertIn("fields=[DONE, ERR, MODE]", repr(dev.ISR))

            with self.assertRaises(MmregError) as ctx:
                dev.define("BAD", offset=0x10, fields={"A": "0"}, w1c={"B"})
            self.assertEqual(ctx.exception.code, "E_SUBREG_NOT_FOUND")

    def test_define_rejects_shadowing_names(self) -> None:
        with Device.open_addr(0x4000_0000, 0x100, force=True, mock=True) as dev:
            for name in ("size", "lock", "close", "_handle", "not valid"):
                with self.assertRaises(MmregError) as ctx:
                    dev.define(name, offset=0)
                self.assertEqual(ctx.exception.code, "E_INVALID_ARG")
            with self.assertRaises(MmregError) as ctx:
                dev.define("CTRL", offset=0, fields={"read": "0"})
            self.assertEqual(ctx.exception.code, "E_INVALID_ARG")

    def test_read_only_and_uio_errors(self) -> None:
        with Device.open_uio("mock_axi_gpio", read_only=True, force=True, mock=True) as ro:
            self.assertEqual(ro.read_u32(0), 0)
            with self.assertRaises(MmregError) as ctx:
                ro.write_u32(0, 1)
            self.assertEqual(ctx.exception.code, "E_READ_ONLY")

        with self.assertRaises(MmregError) as ctx:
            find_uio("nonexistent_uio_device_xyz")
        self.assertEqual(ctx.exception.code, "E_UIO_NOT_FOUND")


if __name__ == "__main__":
    unittest.main()

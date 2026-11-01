"""Unit tests for the pure-Python `mmreg` ctypes binding."""

import copy
import os
import pickle
import tempfile
import unittest
from unittest import mock

import mmreg
from mmreg import Device, InlineRegister, MmregError, find_uio


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

            # A 1-bit field accepts bool; wider fields and whole registers do not.
            dev.CTRL.EN = False
            self.assertEqual(int(dev.CTRL), 0x50)
            dev.CTRL.EN = True
            self.assertEqual(int(dev.CTRL), 0x51)
            for bad in (lambda: setattr(dev.CTRL, "MODE", True), lambda: setattr(dev, "CTRL", True)):
                with self.assertRaises(MmregError) as ctx:
                    bad()
                self.assertEqual(ctx.exception.code, "E_INVALID_ARG")

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
                lambda: dev.irq_wait(0.5),  # type: ignore[arg-type]
                lambda: dev.write_u32(0, True),  # type: ignore[arg-type]
                lambda: Device.open_addr(-4096, 0x100, force=True, mock=True),
                lambda: Device.open_addr(1 << 64, 0x100, force=True, mock=True),
                lambda: Device.open_addr(0x4000_0000, -1, force=True, mock=True),
                lambda: Device.open_uio("bad\x00name", force=True, mock=True),
                lambda: find_uio("bad\x00name"),
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

            # Writing a w1c field is a masked write too: the rw field MODE and unmodelled bits keep
            # their value, and the other w1c bit (ERR) is written as 0 so it stays pending. (The
            # mock is plain memory, so the 1 written to DONE reads back as 1.)
            dev.write_u32(0x0C, 0x0000_0123)  # MODE = 2, ERR pending, unmodelled bit 8 set
            self.assertEqual(dev.ISR.DONE.write(1), 0x0000_0121)
            self.assertEqual(dev.read_u32(0x0C), 0x0000_0121)

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
            with self.assertRaises(MmregError) as ctx:
                dev.define("UNALIGNED", offset=0x02)
            self.assertEqual(ctx.exception.code, "E_UNALIGNED_ACCESS")
            with self.assertRaises(MmregError) as ctx:
                dev.define("OOB", offset=0x100)
            self.assertEqual(ctx.exception.code, "E_OUT_OF_BOUNDS")

    def test_copy_pickle_and_uninitialised_objects(self) -> None:
        with Device.open_addr(0x4000_0000, 0x100, force=True, mock=True) as dev:
            dev.define("CTRL", offset=0, fields={"EN": "0"})
            # A copy would share the C handle and close it twice.
            for bad in (lambda: copy.copy(dev), lambda: copy.deepcopy(dev), lambda: pickle.dumps(dev)):
                with self.assertRaises(TypeError) as type_ctx:
                    bad()
                self.assertIsInstance(type_ctx.exception, MmregError)
                self.assertEqual(type_ctx.exception.code, "E_NOT_COPYABLE")
            # A register copy shares the device and stays usable.
            reg = copy.copy(dev.CTRL)
            reg.EN = 1
            self.assertEqual(int(dev.CTRL), 1)
            self.assertFalse(hasattr(dev, "_missing"))
            self.assertFalse(hasattr(dev.CTRL, "__missing__"))

        # Lookups on objects whose `__init__` never ran raise AttributeError instead of recursing.
        for cls in (Device, InlineRegister):
            uninit = cls.__new__(cls)
            self.assertFalse(hasattr(uninit, "anything"))
            with self.assertRaises(AttributeError) as attr_ctx:
                uninit.anything  # noqa: B018
            self.assertIn("anything", str(attr_ctx.exception))

    def test_every_error_is_mmreg_error(self) -> None:
        # MmregError survives pickling (multiprocessing returns worker exceptions this way).
        err = pickle.loads(pickle.dumps(MmregError("E_INVALID_ARG", "[E_INVALID_ARG] boom")))
        self.assertEqual((type(err), err.code, str(err)), (MmregError, "E_INVALID_ARG", "[E_INVALID_ARG] boom"))

        with Device.open_addr(0x4000_0000, 0x100, force=True, mock=True) as dev:
            # Bad bit specifications, field/w1c argument types and overlapping fields.
            cases = {
                "E_SUBREG_RANGE": [
                    {"A": "x"},
                    {"A": "7:"},
                    {"A": True},
                    {"A": (7.9, 4.2)},
                    {"A": None},
                    {"A": "32"},
                    {"A": "3:0", "B": "2:1"},
                ],
                "E_INVALID_ARG": ["EN", [("EN", 0)], {1: "0"}],
            }
            for code, field_specs in cases.items():
                for fields in field_specs:
                    with self.assertRaises(MmregError, msg=repr(fields)) as ctx:
                        dev.define("R", offset=0, fields=fields)  # type: ignore[arg-type]
                    self.assertEqual(ctx.exception.code, code, repr(fields))
            for w1c in (5, [1]):
                with self.assertRaises(MmregError) as ctx:
                    dev.define("R", offset=0, fields={"A": "0"}, w1c=w1c)  # type: ignore[arg-type]
                self.assertEqual(ctx.exception.code, "E_INVALID_ARG")

            # Decimal with a leading zero, hex, and a bare string as the only w1c name are fine.
            reg = dev.define("R", offset=0, fields={"A": "08", "B": "0x1F:0x10", "C": "0"}, w1c="C")
            self.assertEqual((reg.A.msb, reg.B.msb, reg.B.lsb, reg.C.w1c), (8, 31, 16, True))

            # Missing registers and fields are MmregError and AttributeError, so hasattr works.
            for obj, item, code in ((dev, "NOPE", "E_REG_NOT_FOUND"), (reg, "NOPE", "E_SUBREG_NOT_FOUND")):
                self.assertFalse(hasattr(obj, item))
                with self.assertRaises(MmregError) as ctx:
                    getattr(obj, item)
                self.assertIsInstance(ctx.exception, AttributeError)
                self.assertEqual(ctx.exception.code, code)

    def test_library_load_error_is_mmreg_and_os_error(self) -> None:
        with tempfile.NamedTemporaryFile(suffix=".so") as not_a_library:
            with mock.patch.dict(os.environ, {"MMREG_LIB_PATH": not_a_library.name}):
                with self.assertRaises(OSError) as ctx:
                    mmreg._load_library()
        self.assertIsInstance(ctx.exception, MmregError)
        self.assertEqual(ctx.exception.code, "E_LIB_LOAD")

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

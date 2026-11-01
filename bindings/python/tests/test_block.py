"""Unit tests for v1.3 `mmreg-block/1`, `mmreg-instances/1`, name resolution, access rules, and CLI."""

from __future__ import annotations

import io
import json
from pathlib import Path
import tempfile
from typing import Dict, List, Tuple
import unittest
from unittest import mock

import mmreg
from mmreg import Block, EnumValue, MmregError
from mmreg.__main__ import main as cli_main

EXAMPLES = Path(__file__).resolve().parent.parent / "examples"


EXAMPLE_GPIO_BLOCK = {
    "format": "mmreg-block/1.0",
    "compatible": "acme,gpio-1.0",
    "x-generator": "rdl2mmreg 0.1",
    "enum": {
        "x-enum-note": "ignored",
        "mode_e": {
            "x-entry-note": "ignored",
            "PUSH_PULL": {"value": 0, "desc": "Drive high and low", "x-vendor": 1},
            "OPEN_DRAIN": {"value": "0x1", "desc": "Drive low only"},
        },
    },
    "gpio": {
        "desc": "8-pin GPIO controller",
        "size": "0x100",
        "x-block-meta": {"author": "acme"},
        "VERSION": {
            "at": "0x0",
            "sw": "r",
            "reset": "0x0002_0001",
            "MINOR": {"bits": "15:0"},
            "MAJOR": {"bits": "31:16"},
        },
        "CTRL": {
            "at": "0x8",
            "desc": "Control",
            "x-reg-note": True,
            "EN": {"bits": "0", "desc": "Block enable", "x-field-note": 123},
            "MODE": {"bits": "2:1", "encode": "mode_e"},
            "RESTART": {
                "bits": "3",
                "sw": "w",
                "singlepulse": True,
                "desc": "Write 1: restart",
            },
        },
        "IN": {"at": "0x14", "sw": "r", "hw": "w"},
        "IRQ_STATUS": {
            "at": "0x18",
            "CHANGE": {
                "bits": "7:0",
                "onwrite": "woclr",
                "hw": "w",
                "desc": "Pin changed; write 1 to clear",
            },
        },
        "DEBOUNCE": {
            "at": "0x24",
            "dim": 8,
            "CYCLES": {"bits": "7:0", "reset": 4},
        },
    },
}


class TestBlockAndInstancesValidation(unittest.TestCase):
    def test_valid_example_block_and_x_escape(self) -> None:
        blk = Block.from_dict(EXAMPLE_GPIO_BLOCK, source="acme,gpio-1.0.json")
        self.assertEqual(blk.compatible, "acme,gpio-1.0")
        self.assertEqual(blk.key, "gpio")
        self.assertEqual(blk.size, 0x100)
        self.assertEqual(blk.regwidth, 32)
        self.assertEqual(blk.accesswidth, 32)
        self.assertEqual(len(blk.flat_registers), 4 + 8)

    def test_version_checks(self) -> None:
        for bad_fmt in (
            "mmreg-block/1.1",
            "mmreg-block/2.0",
            "mmreg-block/0.9",
            "other-format/1.0",
            123,
        ):
            data = dict(EXAMPLE_GPIO_BLOCK, format=bad_fmt)
            with self.assertRaises(MmregError) as ctx:
                Block.from_dict(data, source="bad_ver.json")
            self.assertEqual(ctx.exception.code, "E_BLOCK_VERSION")
            self.assertIn("bad_ver.json", str(ctx.exception))
            self.assertIn("register", str(ctx.exception))
            self.assertIn("field", str(ctx.exception))

        # Missing format key -> E_BLOCK_PARSE
        no_fmt = {k: v for k, v in EXAMPLE_GPIO_BLOCK.items() if k != "format"}
        with self.assertRaises(MmregError) as ctx:
            Block.from_dict(no_fmt, source="no_fmt.json")
        self.assertEqual(ctx.exception.code, "E_BLOCK_PARSE")
        self.assertIn("no_fmt.json", str(ctx.exception))

    def test_unknown_keys_and_values_rejected_with_context(self) -> None:
        # 1. Typo in field property ("onwrit" instead of "onwrite")
        bad_field = json.loads(json.dumps(EXAMPLE_GPIO_BLOCK))
        bad_field["gpio"]["IRQ_STATUS"]["CHANGE"]["onwrit"] = "woclr"
        with self.assertRaises(MmregError) as ctx:
            Block.from_dict(bad_field, source="typo.json")
        self.assertEqual(ctx.exception.code, "E_BLOCK_PARSE")
        self.assertIn("typo.json", str(ctx.exception))
        self.assertIn("IRQ_STATUS", str(ctx.exception))
        self.assertIn("CHANGE", str(ctx.exception))

        # 2. Invalid sw value ("wr" instead of "rw")
        bad_sw = json.loads(json.dumps(EXAMPLE_GPIO_BLOCK))
        bad_sw["gpio"]["CTRL"]["EN"]["sw"] = "wr"
        with self.assertRaises(MmregError) as ctx:
            Block.from_dict(bad_sw, source="bad_sw.json")
        self.assertEqual(ctx.exception.code, "E_BLOCK_PARSE")
        self.assertIn("bad_sw.json", str(ctx.exception))
        self.assertIn("CTRL", str(ctx.exception))
        self.assertIn("EN", str(ctx.exception))

        # 3. Unknown register property
        bad_reg = json.loads(json.dumps(EXAMPLE_GPIO_BLOCK))
        bad_reg["gpio"]["IN"]["unknown_prop"] = 1
        with self.assertRaises(MmregError) as ctx:
            Block.from_dict(bad_reg, source="bad_reg.json")
        self.assertEqual(ctx.exception.code, "E_BLOCK_PARSE")
        self.assertIn("bad_reg.json", str(ctx.exception))
        self.assertIn("IN", str(ctx.exception))

        # 4. Unknown block property
        bad_blk = json.loads(json.dumps(EXAMPLE_GPIO_BLOCK))
        bad_blk["gpio"]["unknown_block_prop"] = 1
        with self.assertRaises(MmregError) as ctx:
            Block.from_dict(bad_blk, source="bad_blk.json")
        self.assertEqual(ctx.exception.code, "E_BLOCK_PARSE")
        self.assertIn("bad_blk.json", str(ctx.exception))

    def test_overlap_alignment_bounds_and_enum_checks(self) -> None:
        def make_block(gpio_body: dict, enums: dict | None = None) -> Block:
            doc = {
                "format": "mmreg-block/1.0",
                "compatible": "test,blk-1.0",
                "gpio": gpio_body,
            }
            if enums is not None:
                doc["enum"] = enums
            return Block.from_dict(doc, source="check.json")

        # Unaligned register
        with self.assertRaises(MmregError) as ctx:
            make_block({"R0": {"at": "0x02"}})
        self.assertEqual(ctx.exception.code, "E_BLOCK_PARSE")
        self.assertIn("R0", str(ctx.exception))

        # Register exceeds block size
        with self.assertRaises(MmregError) as ctx:
            make_block({"size": 16, "R0": {"at": 0, "dim": 5}})
        self.assertEqual(ctx.exception.code, "E_BLOCK_PARSE")
        self.assertIn("R0[4]", str(ctx.exception))

        # Overlapping registers
        with self.assertRaises(MmregError) as ctx:
            make_block({"R0": {"at": 0, "dim": 2}, "R1": {"at": 4}})
        self.assertEqual(ctx.exception.code, "E_BLOCK_PARSE")

        # Overlapping fields
        with self.assertRaises(MmregError) as ctx:
            make_block(
                {
                    "R0": {
                        "at": 0,
                        "F1": {"bits": "3:0"},
                        "F2": {"bits": "5:3"},
                    }
                }
            )
        self.assertEqual(ctx.exception.code, "E_BLOCK_PARSE")
        self.assertIn("R0", str(ctx.exception))
        self.assertIn("F2", str(ctx.exception))

        # Field bits outside regwidth
        with self.assertRaises(MmregError) as ctx:
            make_block({"R0": {"at": 0, "regwidth": 16, "F1": {"bits": "16:0"}}})
        self.assertEqual(ctx.exception.code, "E_BLOCK_PARSE")

        # Enum value does not fit in field width
        with self.assertRaises(MmregError) as ctx:
            make_block(
                {"R0": {"at": 0, "F1": {"bits": "0", "encode": "big_e"}}},
                enums={"big_e": {"A": {"value": 0}, "B": {"value": 2}}},
            )
        self.assertEqual(ctx.exception.code, "E_BLOCK_PARSE")
        self.assertIn("F1", str(ctx.exception))

        # Case-insensitive sibling collision, while scalar "reset" + child "RESET" is allowed
        blk = make_block({"R0": {"at": 0, "reset": 0, "RESET": {"bits": "0"}}})
        self.assertIn("RESET", blk.children["R0"].fields)  # type: ignore[union-attr]

        with self.assertRaises(MmregError) as ctx:
            make_block({"R0": {"at": 0, "en": {"bits": "0"}, "EN": {"bits": "1"}}})
        self.assertEqual(ctx.exception.code, "E_BLOCK_PARSE")


class TestAccessRulesWideRegistersAndDump(unittest.TestCase):
    def test_access_rules_and_field_rmw_neutral_values(self) -> None:
        block_doc = {
            "format": "mmreg-block/1.0",
            "compatible": "test,access-1.0",
            "enum": {
                "mode_e": {
                    "PUSH_PULL": {"value": 0},
                    "OPEN_DRAIN": {"value": 1},
                }
            },
            "ip": {
                "size": "0x100",
                "MIXED": {
                    "at": "0x00",
                    "RW_A": {"bits": "3:0", "sw": "rw", "reset": 5},
                    "W1C_B": {"bits": "7:4", "sw": "rw", "onwrite": "woclr"},
                    "W0C_C": {"bits": "11:8", "sw": "rw", "onwrite": "wzc", "reset": "0xF"},
                    "RO_D": {"bits": "15:12", "sw": "r"},
                    "PULSE_E": {"bits": "16", "sw": "w", "singlepulse": True},
                    "NA_F": {"bits": "19:17", "sw": "na"},
                    "MODE": {"bits": "21:20", "encode": "mode_e"},
                },
                "WO_PLAIN": {
                    "at": "0x04",
                    "A": {"bits": "7:0", "sw": "w"},
                    "B": {"bits": "15:8", "sw": "rw"},
                },
                "WCLR_REG": {
                    "at": "0x08",
                    "A": {"bits": "7:0", "onwrite": "wclr"},
                    "B": {"bits": "15:8", "sw": "rw"},
                },
                "FIFO": {
                    "at": "0x0C",
                    "DATA": {"bits": "7:0", "onread": "rclr"},
                    "FLAG": {"bits": "8", "sw": "rw"},
                },
                "ALL_WO": {"at": "0x10", "sw": "w"},
                "ALL_RO": {"at": "0x14", "sw": "r", "reset": "0x1234_5678"},
                "ALL_NA": {"at": "0x18", "sw": "na"},
                "WIDE_HI": {
                    "at": "0x20",
                    "regwidth": 64,
                    "accesswidth": 32,
                    "LO": {"bits": "31:0"},
                    "HI": {"bits": "63:32"},
                },
                "WIDE_LO": {
                    "at": "0x28",
                    "regwidth": 64,
                    "accesswidth": 32,
                    "word_order": "lo_first",
                    "LO": {"bits": "31:0"},
                    "HI": {"bits": "63:32"},
                },
                "CH": {
                    "at": "0x40",
                    "dim": 2,
                    "stride": "0x10",
                    "CFG": {"at": "0x00", "EN": {"bits": "0"}},
                    "STAT": {"at": "0x04", "sw": "r"},
                },
            },
        }
        blk = Block.from_dict(block_doc, source="access.json")

        with mmreg.open(block=blk, at=0x4000_0000, force=True, mock=True) as d:
            # Seed MIXED in mock memory:
            # RW_A=0x5, W1C_B=0xA (pending), W0C_C=0x3, RO_D=0x9, PULSE_E=1, NA_F=0x6, unmodeled bit 31=1
            d.write_u32(0x00, 0x800C_93A5)

            # Writing MODE="OPEN_DRAIN" (1) via RMW must:
            # - keep RW_A (0x5), NA_F (0x6), and unmodeled bit 31 (1)
            # - write 0 to W1C_B, RO_D, PULSE_E
            # - write 0xF (all 1s) to W0C_C
            # - write 1 to MODE [21:20]
            d.MIXED.MODE = "open_drain"
            after = d.read_u32(0x00)
            self.assertEqual(after, 0x801C_0F05)
            self.assertEqual(d.MIXED.MODE, "OPEN_DRAIN")
            self.assertEqual(d.MIXED.MODE, 1)
            self.assertIsInstance(d.MIXED.MODE.read(), EnumValue)
            self.assertEqual(str(d.MIXED.MODE), "OPEN_DRAIN")
            # sw="w", singlepulse=True reads back 0
            self.assertEqual(int(d.MIXED.PULSE_E), 0)

            # Case-insensitive access
            self.assertEqual(int(d.mixed.rw_a), 5)
            d.mixed.rw_a = 9
            self.assertEqual(int(d.MIXED.RW_A), 9)

            # Refused field accesses on MIXED:
            with self.assertRaises(MmregError) as ctx:
                d.MIXED.RO_D = 1
            self.assertEqual(ctx.exception.code, "E_READ_ONLY")

            with self.assertRaises(MmregError) as ctx:
                _ = int(d.MIXED.NA_F)
            self.assertEqual(ctx.exception.code, "E_ACCESS_DENIED")

            with self.assertRaises(MmregError) as ctx:
                d.MIXED.NA_F = 1
            self.assertEqual(ctx.exception.code, "E_ACCESS_DENIED")

            # Register with a plain sw="w" field refuses field RMW, allows whole-register write
            with self.assertRaises(MmregError) as ctx:
                d.WO_PLAIN.B = 1
            self.assertEqual(ctx.exception.code, "E_WRITE_ONLY")
            d.WO_PLAIN = 0x1234
            self.assertEqual(int(d.WO_PLAIN.B), 0x12)
            self.assertIn("A=-", str(d.WO_PLAIN))

            # Register with wclr refuses field RMW, allows whole-register write
            with self.assertRaises(MmregError) as ctx:
                d.WCLR_REG.B = 1
            self.assertEqual(ctx.exception.code, "E_RMW_UNSAFE")
            d.WCLR_REG = 0x5678
            self.assertEqual(int(d.WCLR_REG), 0x5678)

            # Register with rclr refuses read & field RMW unless side_effects=True
            d.FIFO = 0x142
            with self.assertRaises(MmregError) as ctx:
                _ = int(d.FIFO)
            self.assertEqual(ctx.exception.code, "E_READ_SIDE_EFFECT")
            with self.assertRaises(MmregError) as ctx:
                _ = int(d.FIFO.DATA)
            self.assertEqual(ctx.exception.code, "E_READ_SIDE_EFFECT")
            with self.assertRaises(MmregError) as ctx:
                d.FIFO.FLAG = 0
            self.assertEqual(ctx.exception.code, "E_READ_SIDE_EFFECT")

            self.assertEqual(d.FIFO.read(side_effects=True), 0x142)
            self.assertEqual(d.FIFO.DATA.read(side_effects=True), 0x42)
            d.FIFO.FLAG.write(0, side_effects=True)
            self.assertEqual(d.FIFO.read(side_effects=True), 0x42)

            # ALL_WO, ALL_RO, ALL_NA whole-register rules
            d.ALL_WO = 0x99
            with self.assertRaises(MmregError) as ctx:
                _ = int(d.ALL_WO)
            self.assertEqual(ctx.exception.code, "E_WRITE_ONLY")

            with self.assertRaises(MmregError) as ctx:
                d.ALL_RO = 1
            self.assertEqual(ctx.exception.code, "E_READ_ONLY")

            with self.assertRaises(MmregError) as ctx:
                _ = int(d.ALL_NA)
            self.assertEqual(ctx.exception.code, "E_ACCESS_DENIED")
            with self.assertRaises(MmregError) as ctx:
                d.ALL_NA = 1
            self.assertEqual(ctx.exception.code, "E_ACCESS_DENIED")

            # Wide registers: verify word order of writes and reads
            calls: List[Tuple[str, int, int]] = []
            orig_wr32 = d.write_u32
            orig_rd32 = d.read_u32

            def spy_wr32(off: int, val: int) -> None:
                calls.append(("w", off, val))
                orig_wr32(off, val)

            def spy_rd32(off: int) -> int:
                v = orig_rd32(off)
                calls.append(("r", off, v))
                return v

            with mock.patch.object(d, "write_u32", side_effect=spy_wr32), mock.patch.object(
                d, "read_u32", side_effect=spy_rd32
            ):
                # Default hi_first: writes +0x24 (high) then +0x20 (low)
                d.WIDE_HI = 0x1122_3344_5566_7788
                self.assertEqual(
                    calls,
                    [("w", 0x24, 0x1122_3344), ("w", 0x20, 0x5566_7788)],
                )
                calls.clear()
                self.assertEqual(int(d.WIDE_HI), 0x1122_3344_5566_7788)
                self.assertEqual(
                    calls,
                    [("r", 0x20, 0x5566_7788), ("r", 0x24, 0x1122_3344)],
                )
                calls.clear()

                # lo_first: writes +0x28 (low) then +0x2C (high)
                d.WIDE_LO = 0xAABB_CCDD_EEFF_0011
                self.assertEqual(
                    calls,
                    [("w", 0x28, 0xEEFF_0011), ("w", 0x2C, 0xAABB_CCDD)],
                )

            # Register group array access
            d.CH[0].CFG.EN = 1
            d.CH[1].CFG.EN = 1
            self.assertEqual(d.read_u32(0x40), 1)
            self.assertEqual(d.read_u32(0x50), 1)

            # Dump skips FIFO (rclr) and says so, and diff_reset filters unchanged registers
            full_dump = d.dump()
            self.assertIn("FIFO @ 0x0c = <skipped: read side effect (rclr)>", full_dump)
            self.assertIn("ALL_WO @ 0x10 = <skipped: write-only>", full_dump)
            self.assertIn("MODE=OPEN_DRAIN", full_dump)

            diff_dump = d.dump(diff_reset=True)
            self.assertIn("FIFO @ 0x0c = <skipped: read side effect (rclr)>", diff_dump)
            self.assertIn("CH[0].CFG", diff_dump)
            # CH[0].STAT is 0, which matches its default reset=0, so it is omitted in diff_reset=True
            self.assertNotIn("CH[0].STAT", diff_dump)


class TestResolutionListChecksAndCli(unittest.TestCase):
    def test_open_resolution_instance_checks_list_and_cli(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            blocks_dir = root / "blocks"
            inst_dir = root / "instances.d"
            sysfs_uio = root / "sys_uio"
            blocks_dir.mkdir()
            inst_dir.mkdir()
            sysfs_uio.mkdir()

            # Write block file `acme,gpio-1.0.json`
            block_file = blocks_dir / "acme,gpio-1.0.json"
            block_file.write_text(json.dumps(EXAMPLE_GPIO_BLOCK), encoding="utf-8")

            # Set up fake sysfs UIO device `uio0` named `gpio0` with compatible `acme,gpio-1.0\0generic-uio\0`
            uio0 = sysfs_uio / "uio0"
            (uio0 / "device" / "of_node").mkdir(parents=True)
            (uio0 / "maps" / "map0").mkdir(parents=True)
            (uio0 / "name").write_text("gpio0\n", encoding="utf-8")
            (uio0 / "device" / "of_node" / "compatible").write_bytes(b"acme,gpio-1.0\x00generic-uio\x00")
            (uio0 / "maps" / "map0" / "addr").write_text("0xa0010000\n", encoding="utf-8")
            (uio0 / "maps" / "map0" / "size").write_text("0x1000\n", encoding="utf-8")

            # Write instance file defining `gpio_inst` (with check) and `missing_uio`
            inst_file1 = inst_dir / "10-board.json"
            inst_doc1 = {
                "format": "mmreg-instances/1.0",
                "x-board": "rev-b",
                "gpio_inst": {
                    "block": "acme,gpio-1.0",
                    "at": "0xA002_0000",
                    "checks": [
                        {
                            "name": "gpio_clk_en",
                            "addr": "0xFF5E_0074",
                            "mask": "0x0100_0000",
                            "expected": "0x0100_0000",
                        }
                    ],
                },
                "missing_uio": {
                    "block": "acme,gpio-1.0",
                    "uio": "nonexistent_uio",
                },
            }
            inst_file1.write_text(json.dumps(inst_doc1), encoding="utf-8")

            # 1. Resolve `gpio0` purely via sysfs UIO -> compatible -> block file
            with mmreg.open(
                "gpio0",
                mock=True,
                force=True,
                blocks_path=blocks_dir,
                instances_path=inst_dir,
                sysfs_uio=sysfs_uio,
            ) as d:
                self.assertEqual(d.base_address, 0xA001_0000)
                self.assertEqual(d.size, 0x1000)
                d.CTRL.MODE = "OPEN_DRAIN"
                d.DEBOUNCE[3].CYCLES = 10
                self.assertEqual(d.CTRL.MODE, "OPEN_DRAIN")
                self.assertEqual(int(d.DEBOUNCE[3].CYCLES), 10)
                self.assertIn("CTRL @ 0x08 = 0x0000_0002  (EN=0, MODE=OPEN_DRAIN, RESTART=0)", str(d.CTRL))

            # 2. Resolve `gpio_inst` via instance file: check fails when clock bit is 0, succeeds when 1
            with self.assertRaises(MmregError) as ctx:
                mmreg.open(
                    "gpio_inst",
                    mock=True,
                    force=True,
                    blocks_path=blocks_dir,
                    instances_path=inst_dir,
                    sysfs_uio=sysfs_uio,
                    check_reader=lambda addr: 0x0000_0000,
                )
            self.assertEqual(ctx.exception.code, "E_VALIDATION_HOOK_FAILED")
            self.assertIn("gpio_clk_en", str(ctx.exception))

            with mmreg.open(
                "gpio_inst",
                mock=True,
                force=True,
                blocks_path=blocks_dir,
                instances_path=inst_dir,
                sysfs_uio=sysfs_uio,
                check_reader=lambda addr: 0x0100_0000,
            ) as d2:
                self.assertEqual(d2.base_address, 0xA002_0000)

            # 3. `mmreg.list()` reports instance presence accurately
            found = mmreg.list(
                blocks_path=blocks_dir,
                instances_path=inst_dir,
                sysfs_uio=sysfs_uio,
            )
            by_name: Dict[str, mmreg.InstanceInfo] = {item.name: item for item in found}
            self.assertTrue(by_name["gpio_inst"].present)
            self.assertFalse(by_name["missing_uio"].present)
            self.assertTrue(by_name["gpio0"].present)
            self.assertIn("[present]", str(found))
            self.assertIn("[missing]", str(found))

            # 4. Duplicate instance across two files raises E_INSTANCE_DUPLICATE
            inst_file2 = inst_dir / "20-dup.json"
            inst_file2.write_text(
                json.dumps(
                    {
                        "format": "mmreg-instances/1.0",
                        "gpio_inst": {"block": "acme,gpio-1.0", "uio": "gpio0"},
                    }
                ),
                encoding="utf-8",
            )
            with self.assertRaises(MmregError) as ctx:
                mmreg.list(
                    blocks_path=blocks_dir,
                    instances_path=inst_dir,
                    sysfs_uio=sysfs_uio,
                )
            self.assertEqual(ctx.exception.code, "E_INSTANCE_DUPLICATE")

            # 5. CLI validator (`python -m mmreg check`)
            out_buf = io.StringIO()
            err_buf = io.StringIO()
            with mock.patch("sys.stdout", out_buf), mock.patch("sys.stderr", err_buf):
                rc_ok = cli_main(["check", str(block_file), str(inst_file1)])
            self.assertEqual(rc_ok, 0)
            self.assertIn("OK:", out_buf.getvalue())

            # CLI detects duplicate across two instance files
            err_buf = io.StringIO()
            with mock.patch("sys.stdout", io.StringIO()), mock.patch("sys.stderr", err_buf):
                rc_dup = cli_main(["check", str(inst_file1), str(inst_file2)])
            self.assertEqual(rc_dup, 1)
            self.assertIn("[E_INSTANCE_DUPLICATE]", err_buf.getvalue())

    def test_inline_define_at_alias_and_wait_irq(self) -> None:
        with mmreg.open(at=0x4000_0000, size=0x100, force=True, mock=True) as d:
            scratch = d.define("SCRATCH", at=0x40, fields={"LO": "15:0", "HI": "31:16"})
            scratch.HI = 0xBEEF
            scratch.lo = 0x1234  # case-insensitive field lookup
            self.assertEqual(int(d.scratch), 0xBEEF_1234)  # case-insensitive register lookup
            self.assertIn("SCRATCH @ 0x40 = 0xbeef_1234", d.dump())

            for bad_timeout in (-0.1, True, "1.0", float("nan"), float("inf")):
                with self.assertRaises(MmregError) as ctx:
                    d.wait_irq(bad_timeout)  # type: ignore[arg-type]
                self.assertEqual(ctx.exception.code, "E_INVALID_ARG")


class TestNamesChecksAndBinding(unittest.TestCase):
    def test_names_of_attributes_are_never_written_by_attribute_assignment(self) -> None:
        blk = Block.from_dict(
            {
                "format": "mmreg-block/1.0",
                "compatible": "t",
                "b": {
                    "SIZE": {"at": "0x0"},
                    "CTRL": {"at": "0x8", "RESET": {"bits": "0"}, "NAME": {"bits": "1"}, "READ": {"bits": "2"}},
                },
            }
        )
        with mmreg.open(block=blk, at=0x4000_0000, force=True, mock=True) as d:
            # `d.CTRL.name` is the register's name, so assigning it must not write the field NAME.
            for owner, attr in ((d.CTRL, "name"), (d.CTRL, "reset"), (d.CTRL, "read"), (d, "size")):
                with self.assertRaises(MmregError) as ctx:
                    setattr(owner, attr, 1)
                self.assertEqual(ctx.exception.code, "E_INVALID_ARG")
                self.assertIn("use [", str(ctx.exception))
            self.assertEqual(d.read_u32(0x8), 0)
            self.assertEqual(d.read_u32(0x0), 0)
            self.assertEqual(d.CTRL.name, "CTRL")
            # Exact-case attributes and `[]` reach the register and its fields.
            d.CTRL.NAME = 1
            d.CTRL["reset"] = 1
            d["size"] = 0x55
            self.assertEqual(d.read_u32(0x8), 0b11)
            self.assertEqual(int(d.SIZE), 0x55)
            self.assertEqual(int(d.CTRL["read"]), 0)

            d.define("SCRATCH", at=0x40, fields={"NAME": "0", "HI": "31:16"})
            with self.assertRaises(MmregError):
                d.SCRATCH.name = 1
            d.SCRATCH["name"] = 1
            self.assertEqual(int(d.SCRATCH.NAME), 1)

    def test_assigning_registers_and_star_import(self) -> None:
        with mmreg.open(at=0x4000_0000, size=0x100, force=True, mock=True) as d:
            a = d.define("A", at=0x0)
            d.define("B", at=0x4)
            d.B = 7
            d.A = d.B  # copies the value
            self.assertEqual(int(a), 7)
            with mock.patch.object(d, "write_u32", side_effect=AssertionError("written")):
                d.A = d.A  # no write-back that could clear pending bits

        namespace: Dict[str, object] = {}
        exec("from mmreg import *", namespace)
        self.assertNotIn("open", namespace)
        self.assertNotIn("list", namespace)
        self.assertIs(namespace["Block"], Block)

    def test_instance_checks_skipped_with_force_or_mock(self) -> None:
        paths = {"blocks_path": EXAMPLES / "blocks", "instances_path": EXAMPLES / "instances"}
        # The example check expects a set clock bit; the mock window reads 0, so it would fail.
        with mmreg.open("gpio0", mock=True, force=True, **paths) as d:
            self.assertEqual(d.base_address, 0xA001_0000)
            self.assertEqual(d.size, 0x100)
        with self.assertRaises(MmregError) as ctx:
            mmreg.open("gpio0", mock=True, force=True, check_reader=lambda addr: 0, **paths)
        self.assertEqual(ctx.exception.code, "E_VALIDATION_HOOK_FAILED")

    def test_non_string_keys_and_lazy_arrays(self) -> None:
        for doc in (
            {"format": "mmreg-block/1.0", "compatible": "t", "b": {1: {"at": "0"}}},
            {"format": "mmreg-block/1.0", "compatible": "t", 2: {}, "b": {"R": {"at": "0"}}},
            {"format": "mmreg-block/1.0", "compatible": "t", "b": {"R": {"at": "0", "F": {"bits": "0", 3: 1}}}},
        ):
            with self.assertRaises(MmregError) as ctx:
                Block.from_dict(doc)  # type: ignore[arg-type]
            self.assertEqual(ctx.exception.code, "E_BLOCK_PARSE")

        blk = Block.from_dict(
            {
                "format": "mmreg-block/1.0",
                "compatible": "t",
                "b": {
                    "R": {"at": "0", "dim": 4096, "A": {"bits": "0"}, "B": {"bits": "31:16"}},
                    "CH": {"at": "0x4000", "dim": 2, "CFG": {"at": "0"}, "STAT": {"at": "4", "sw": "r"}},
                },
            }
        )
        with mmreg.open(block=blk, at=0x4000_0000, force=True, mock=True) as d:
            self.assertEqual(len(d.R), 4096)
            d.R[4095].B = 0xBEEF
            self.assertEqual(d.read_u32(4095 * 4), 0xBEEF_0000)
            self.assertEqual([r.offset for r in d.CH], [0x4000, 0x4008])
            self.assertEqual(d.CH[1].STAT.offset, 0x400C)
            with self.assertRaises(IndexError):
                d.R[4096]
            self.assertEqual(len(d.dump().splitlines()), 4096 + 4)

    def test_dump_lists_inline_registers_of_a_block_device(self) -> None:
        with mmreg.open(
            "gpio0", mock=True, force=True, blocks_path=EXAMPLES / "blocks", instances_path=EXAMPLES / "instances"
        ) as d:
            d.define("SCRATCH", at=0x80, fields={"LO": "15:0"})
            d.SCRATCH.LO = 3
            dump = d.dump()
            self.assertIn("EVENT_FIFO @ 0x1c = <skipped: read side effect (rclr)>", dump)
            # `~` marks values hardware changes (`hw: "w"`): a register without fields, and a field.
            self.assertIn("IN~ @ 0x14 = 0x0000_0000", dump.splitlines())
            self.assertIn("IRQ_STATUS @ 0x18 = 0x0000_0000  (CHANGE~=0)", dump)
            self.assertIn("(EN=0, MODE=PUSH_PULL, RESTART=0)", dump)
            self.assertTrue(dump.endswith("SCRATCH @ 0x80 = 0x0000_0003  (LO=3)"))

    def test_unknown_reset_x_is_left_out_of_diff_reset(self) -> None:
        blk = Block.from_dict(
            {
                "format": "mmreg-block/1.0",
                "compatible": "t",
                "b": {
                    "size": "0x10",
                    "R": {"at": "0x0", "A": {"bits": "7:0", "reset": "x"}, "B": {"bits": "15:8", "reset": "0x1"}},
                    "S": {"at": "0x4", "reset": "x"},
                },
            }
        )
        with mmreg.open(block=blk, at=0x4000_0000, force=True, mock=True) as d:
            self.assertEqual(d.R.reset, 0x100)  # only B has a reset value
            self.assertIsNone(d.S.reset)
            self.assertIsNone(d.R.A.reset)
            d.R = 0x1A5  # A is unknown after reset, B is at its reset value
            self.assertNotIn("R @", d.dump(diff_reset=True))
            d.R.B = 2
            self.assertIn("R @ 0x00 = 0x0000_02a5", d.dump(diff_reset=True))
            # A register without any reset value is always listed.
            self.assertIn("S @ 0x04", d.dump(diff_reset=True))

    def test_example_files_are_valid(self) -> None:
        files = sorted(str(p) for p in EXAMPLES.glob("*/*.json"))
        self.assertEqual(len(files), 2)
        with mock.patch("sys.stdout", io.StringIO()):
            self.assertEqual(cli_main(["check", *files]), 0)

    def test_watch_registers_and_fields(self) -> None:
        with mmreg.open(
            "gpio0", mock=True, force=True, blocks_path=EXAMPLES / "blocks", instances_path=EXAMPLES / "instances"
        ) as d:
            scratch = d.define("SCRATCH", at=0x80, fields={"LO": "15:0", "HI": "31:16"})
            scratch.LO = 1
            d.CTRL.MODE = "PUSH_PULL"
            d.IRQ_STATUS.CHANGE = 0

            # 1. Watch a single BlockField across 3 polls, mutating on sleep
            step = 0

            def fake_sleep(_sec: float) -> None:
                nonlocal step
                step += 1
                if step == 1:
                    d.CTRL.MODE = "OPEN_DRAIN"
                elif step == 2:
                    d.IRQ_STATUS = 0x05
                    scratch.LO = 9

            buf = io.StringIO()
            with mock.patch("time.sleep", side_effect=fake_sleep):
                self.assertIsNone(d.CTRL.MODE.watch(interval=0.01, count=3, file=buf))
            # Field values print as in `dump` (`MODE=OPEN_DRAIN`).
            self.assertEqual(
                buf.getvalue().splitlines(),
                [
                    "CTRL.MODE @ 0x08 = PUSH_PULL",
                    "CTRL.MODE @ 0x08 = PUSH_PULL -> OPEN_DRAIN",
                ],
            )

            # 2. Watch by names and objects on Device (`IRQ_STATUS.CHANGE`, `DEBOUNCE[2].CYCLES`, `SCRATCH.LO`)
            step = 0
            d.IRQ_STATUS = 0
            d.DEBOUNCE[2].CYCLES = 4
            scratch.LO = 1

            def fake_sleep2(_sec: float) -> None:
                nonlocal step
                step += 1
                if step == 1:
                    d.IRQ_STATUS = 0x03
                    d.DEBOUNCE[2].CYCLES = 12
                    scratch.LO = 7

            buf2 = io.StringIO()
            with mock.patch("time.sleep", side_effect=fake_sleep2):
                d.watch(
                    "IRQ_STATUS.CHANGE",
                    "DEBOUNCE[2].CYCLES",
                    scratch.LO,
                    interval=0.01,
                    count=2,
                    file=buf2,
                )
            self.assertEqual(
                buf2.getvalue().splitlines(),
                [
                    "IRQ_STATUS.CHANGE~ @ 0x18 = 0",
                    "DEBOUNCE[2].CYCLES @ 0x2c = 4",
                    "SCRATCH.LO @ 0x80 = 1",
                    "IRQ_STATUS.CHANGE~ @ 0x18 = 0 -> 3",
                    "DEBOUNCE[2].CYCLES @ 0x2c = 4 -> 0xc",
                    "SCRATCH.LO @ 0x80 = 1 -> 7",
                ],
            )

            # 3. `d.watch(only_volatile=True, count=1)` polls only volatile registers (`IN~`, `IRQ_STATUS`)
            # and skips `EVENT_FIFO` (`rclr`).
            buf3 = io.StringIO()
            d.watch(only_volatile=True, count=1, file=buf3)
            self.assertEqual(
                buf3.getvalue().splitlines(),
                [
                    "IN~ @ 0x14 = 0x0000_0000",
                    "IRQ_STATUS @ 0x18 = 0x0000_0003  (CHANGE~=3)",
                ],
            )

            # 4. Invalid watch arguments raise E_INVALID_ARG
            # `interval` below 1 ms (a busy loop on the bus) is refused, as by the CLI.
            for bad_kwargs in (
                {"interval": 0, "count": 1},
                {"interval": 0.0005, "count": 1},
                {"interval": -0.1, "count": 1},
                {"interval": True, "count": 1},
                {"interval": float("nan"), "count": 1},
                {"interval": 0.01, "count": 0},
                {"interval": 0.01, "count": True},
            ):
                with self.assertRaises(MmregError) as ctx:
                    d.CTRL.watch(**bad_kwargs)  # type: ignore[arg-type]
                self.assertEqual(ctx.exception.code, "E_INVALID_ARG")

            # A target of another device is refused.
            with mmreg.open(at=0x5000_0000, size=0x100, force=True, mock=True) as other:
                other_reg = other.define("R", at=0x0)
                with self.assertRaises(MmregError):
                    d.watch(other_reg, count=1, file=io.StringIO())

            # Until Ctrl-C without `count`; nothing is accumulated, so memory stays constant.
            polls = 0

            def interrupt_after_three(_sec: float) -> None:
                nonlocal polls
                polls += 1
                if polls == 3:
                    raise KeyboardInterrupt

            with mock.patch("time.sleep", side_effect=interrupt_after_three):
                self.assertIsNone(d.IN.watch(interval=0.01, file=io.StringIO()))
            self.assertEqual(polls, 3)


if __name__ == "__main__":
    unittest.main()

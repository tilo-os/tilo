#!/usr/bin/python3
"""File-backed firmware tests; never touches a real EFI variable."""

import importlib.machinery
import importlib.util
import os
from pathlib import Path
import struct
import tempfile
import unittest
import uuid
from unittest.mock import patch

HELPER = Path(__file__).resolve().parents[1] / "usr/libexec/tilo-boot-order"
loader = importlib.machinery.SourceFileLoader("tilo_boot_order", str(HELPER))
spec = importlib.util.spec_from_loader(loader.name, loader)
mod = importlib.util.module_from_spec(spec)
loader.exec_module(mod)
ESP = "11111111-2222-3333-4444-555555555555"
WIN_ESP = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
TILO = r"\EFI\systemd\systemd-bootaa64.efi"
WINDOWS = r"\EFI\Microsoft\Boot\bootmgfw.efi"


def boot_option(description, esp, path):
    hd = (struct.pack("<BBHIQQ", 4, 1, 42, 4, 2048, 4194304)
          + uuid.UUID(esp).bytes_le + b"\x02\x02")
    file_bytes = (path + "\0").encode("utf-16-le")
    file_node = struct.pack("<BBH", 4, 4, len(file_bytes) + 4) + file_bytes
    end = b"\x7f\xff\x04\x00"
    nodes = hd + file_node + end
    return struct.pack("<IH", 1, len(nodes)) + (description + "\0").encode("utf-16-le") + nodes


def restart_fixture(test):
    """Give the next subTest phase of a merged test the fresh fixture it had
    as its own test: tear down, run cleanups, reset instance state, set up."""
    test.tearDown()
    test.doCleanups()
    fresh = type(test)(test._testMethodName).__dict__
    keep = {key: test.__dict__[key] for key in ("_outcome", "_subtest") if key in test.__dict__}
    test.__dict__.clear()
    test.__dict__.update(fresh, **keep)
    test.setUp()


class BootOrderTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.vars = self.root / "vars"
        self.vars.mkdir()
        self.manager = self.root / "efibootmgr"
        self.manager.write_text("""#!/usr/bin/python3
import os, pathlib, struct, sys
args = sys.argv[1:]
if len(args) != 3 or args[:2] != ['-q', '-o']:
    sys.exit(2)
ids = args[2].split(',')
root = pathlib.Path(os.environ['TILO_BOOT_ORDER_EFIVARS'])
path = root / 'BootOrder-8be4df61-93ca-11d2-aa0d-00e098032b8c'
path.write_bytes(bytes.fromhex('07000000') + b''.join(struct.pack('<H', int(x, 16)) for x in ids))
if os.environ.get('TILO_TEST_MUTATE_NEXT'):
    (root / 'BootNext-8be4df61-93ca-11d2-aa0d-00e098032b8c').write_bytes(
        bytes.fromhex('07000000') + struct.pack('<H', 2))
""")
        self.manager.chmod(0o755)
        self.env = patch.dict(os.environ, TILO_BOOT_ORDER_EFIVARS=str(self.vars),
                              TILO_BOOT_ORDER_EFIBOOTMGR=str(self.manager))
        self.env.start()
        self.addCleanup(self.env.stop)
        self.put("BootOrder", struct.pack("<H", 1))
        self.put("BootCurrent", struct.pack("<H", 0x100))
        self.put("Boot0001", boot_option("Windows Boot Manager", WIN_ESP, WINDOWS))
        self.put("Boot0100", boot_option("Tilo", ESP, TILO))
        self.put("LoaderDevicePartUUID", (ESP + "\0").encode("utf-16-le"), mod.LOADER_VENDOR)

    def put(self, name, data, vendor=mod.EFI_GLOBAL):
        (self.vars / f"{name}-{vendor}").write_bytes(b"\x07\0\0\0" + data)

    def order(self):
        return mod.ids("BootOrder")

    def test_enlist_writes_after_windows_and_verifies_readback(self):
        with self.subTest(phase="appends_after_windows_and_is_idempotent"):
            mod.enlist("0100", ESP, TILO)
            self.assertEqual(self.order(), ["0001", "0100"])
            mod.enlist("0100", ESP, TILO)
            self.assertEqual(self.order(), ["0001", "0100"])
        restart_fixture(self)
        with self.subTest(phase="detects_failed_firmware_readback"):
            with patch.dict(os.environ, TILO_TEST_MUTATE_NEXT="1"):
                with self.assertRaisesRegex(mod.Refusal, "firmware readback differs"):
                    mod.enlist("0100", ESP, TILO)

    def test_refuses_unsafe_boot_state(self):
        with self.subTest(phase="refuses_busy_bootnext_and_wrong_identity"):
            self.put("BootNext", struct.pack("<H", 2))
            with self.assertRaisesRegex(mod.Refusal, "BootNext is occupied"):
                mod.enlist("0100", ESP, TILO)
            self.assertEqual(self.order(), ["0001"])
            (self.vars / f"BootNext-{mod.EFI_GLOBAL}").unlink()
            with self.assertRaisesRegex(mod.Refusal, "does not match"):
                mod.enlist("0100", "bbbbbbbb-cccc-dddd-eeee-ffffffffffff", TILO)
            self.assertEqual(self.order(), ["0001"])
        restart_fixture(self)
        with self.subTest(phase="refuses_non_windows_default_or_non_current_tilo"):
            self.put("Boot0001", boot_option("other", WIN_ESP, TILO))
            with self.assertRaisesRegex(mod.Refusal, "Windows Boot Manager is not the first"):
                mod.enlist("0100", ESP, TILO)
            self.put("Boot0001", boot_option("Windows Boot Manager", WIN_ESP, WINDOWS))
            self.put("BootCurrent", struct.pack("<H", 1))
            with self.assertRaisesRegex(mod.Refusal, "not the current"):
                mod.enlist("0100", ESP, TILO)
            self.assertEqual(self.order(), ["0001"])
        restart_fixture(self)
        with self.subTest(phase="refuses_ambiguous_or_modified_boot_option"):
            self.put("BootOrder", struct.pack("<HH", 1, 1))
            with self.assertRaisesRegex(mod.Refusal, "duplicated"):
                mod.enlist("0100", ESP, TILO)
            self.put("BootOrder", struct.pack("<H", 1))
            self.put("Boot0100", boot_option("Tilo", ESP, r"\EFI\BOOT\other.efi"))
            with self.assertRaisesRegex(mod.Refusal, "does not match"):
                mod.enlist("0100", ESP, TILO)
            self.assertEqual(self.order(), ["0001"])


if __name__ == "__main__":
    unittest.main()

"""Tests for the read-only Windows dual-boot preflight gate."""
import importlib.util
from pathlib import Path
import sys
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "dev" / "verify-windows-dualboot-preflight.py"
spec = importlib.util.spec_from_file_location("windows_dualboot_preflight", SCRIPT)
preflight = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = preflight
spec.loader.exec_module(preflight)


def observed(**changes):
    value = {
        "WindowsDisk": {"Number": 0, "FriendlyName": "Samsung NVMe", "UniqueId": "nvme-identity", "Size": 1_000_000, "BusType": "NVMe", "IsBoot": True, "IsSystem": True, "IsOffline": False, "IsReadOnly": False, "LargestFreeExtent": 100_000, "PartitionStyle": "GPT"},
        "MediaDisk": {"Number": 4, "FriendlyName": "Verified USB", "UniqueId": "usb-identity", "Size": 200_000, "BusType": "USB", "IsBoot": False, "IsSystem": False, "IsOffline": False, "IsReadOnly": False, "LargestFreeExtent": 0, "PartitionStyle": "GPT"},
        "Partitions": [
            {"PartitionNumber": 1, "Guid": "{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}", "GptType": "{c12a7328-f81f-11d2-ba4b-00a0c93ec93b}", "Offset": 1048576, "Size": 272629760, "Type": "System"},
            {"PartitionNumber": 3, "Guid": "{ffffffff-1111-2222-3333-444444444444}", "GptType": "{ebd0a0a2-b9e5-4433-87c0-68b6b72699c7}", "Offset": 300000000, "Size": 600000000, "Type": "Basic"},
        ],
        "BitLocker": {"MountPoint": "C:", "VolumeStatus": "FullyEncrypted", "ProtectionStatus": "Off", "EncryptionPercentage": 100, "LockStatus": "Unlocked"},
    }
    value.update(changes)
    return value


class PreflightTests(unittest.TestCase):
    def baseline(self):
        return preflight.make_receipt(observed(), 0, 4)

    def test_esp_detection_and_requirements(self):
        with self.subTest(phase="normalizes_one_efi_partition"):
            receipt = self.baseline()
            self.assertEqual(receipt["windows_esp"]["number"], 1)
            self.assertTrue(receipt["read_only"])
        with self.subTest(phase="requires_exactly_one_windows_esp"):
            with self.assertRaisesRegex(ValueError, "exactly one EFI"):
                preflight.make_receipt(observed(Partitions=[]), 0, 4)
        with self.subTest(phase="passes_only_when_all_requirements_hold"):
            receipt = self.baseline()
            self.assertEqual(preflight.verify_receipt(receipt, receipt, 64_000), [])
        with self.subTest(phase="blocks_bitlocker_extent_media_and_esp_drift"):
            baseline = self.baseline()
            base = observed()
            cases = {
                "bitlocker": observed(BitLocker={"MountPoint":"C:", "VolumeStatus":"FullyEncrypted", "ProtectionStatus":"On"}),
                "extent": observed(WindowsDisk=base["WindowsDisk"] | {"LargestFreeExtent": 63_999}),
                "media": observed(MediaDisk=base["MediaDisk"] | {"BusType":"NVMe"}),
                "esp": observed(Partitions=[base["Partitions"][0] | {"Offset": 2097152}, base["Partitions"][1]]),
            }
            for name, data in cases.items():
                with self.subTest(name=name):
                    self.assertTrue(preflight.verify_receipt(preflight.make_receipt(data, 0, 4), baseline, 64_000))

    def test_media_safety_facts_must_be_explicit_booleans(self):
        baseline = self.baseline()
        for source_key, receipt_key in (("IsBoot", "is_boot"), ("IsSystem", "is_system"),
                                        ("IsReadOnly", "is_read_only"), ("IsOffline", "is_offline")):
            for value in (None, 0, "", "False", True):
                with self.subTest(field=source_key, value=value):
                    data = observed()
                    data["MediaDisk"][source_key] = value
                    current = preflight.make_receipt(data, 0, 4)
                    self.assertTrue(preflight.verify_receipt(current, baseline, 64_000))
            with self.subTest(field=source_key, phase="missing_observation"):
                data = observed()
                del data["MediaDisk"][source_key]
                current = preflight.make_receipt(data, 0, 4)
                self.assertTrue(preflight.verify_receipt(current, baseline, 64_000))
            with self.subTest(field=receipt_key, phase="missing_receipt_fact"):
                current = self.baseline()
                del current["external_media"][receipt_key]
                self.assertTrue(preflight.verify_receipt(current, baseline, 64_000))

    def test_equal_unknown_windows_identities_do_not_prove_preservation(self):
        for section, field, values in (
            ("WindowsDisk", "UniqueId", (None, "", " ")),
            ("WindowsDisk", "Size", (-1, 0)),
            ("WindowsDisk", "PartitionStyle", (None, "")),
            ("Partitions", "Guid", (None, "", "invalid", "00000000-0000-0000-0000-000000000000")),
            ("Partitions", "PartitionNumber", (-1, 0)),
            ("Partitions", "Offset", (-1,)),
            ("Partitions", "Size", (-1, 0)),
        ):
            for value in values:
                with self.subTest(section=section, field=field, value=value):
                    data = observed()
                    row = data[section][0] if section == "Partitions" else data[section]
                    row[field] = value
                    before = preflight.make_receipt(data, 0, 4)
                    current = preflight.make_receipt(data, 0, 4)
                    self.assertIn("Windows disk or EFI System Partition identity is missing, invalid, or changed from the baseline",
                                  preflight.verify_receipt(current, before, 64_000))
        with self.subTest(phase="missing_keys_in_saved_receipts"):
            before, current = self.baseline(), self.baseline()
            del before["windows_esp"]["guid"]
            del current["windows_esp"]["guid"]
            self.assertTrue(preflight.verify_receipt(current, before, 64_000))

    def test_bitlocker_states(self):
        with self.subTest(phase="accepts_suspended_encryption_without_requiring_full_decryption"):
            self.assertTrue(preflight.bitlocker_ready_for_planned_boot_change(
                {"volume_status":"FullyEncrypted", "protection_status":"Off", "encryption_percentage":100, "lock_status":"Unlocked"}
            ))
            self.assertTrue(preflight.bitlocker_ready_for_planned_boot_change(
                {"volume_status":"FullyDecrypted", "protection_status":"Off", "encryption_percentage":0, "lock_status":"Unlocked"}
            ))
        with self.subTest(phase="rejects_active_or_partial_bitlocker_states"):
            cases = (
                {"volume_status":"FullyEncrypted", "protection_status":"On", "encryption_percentage":100, "lock_status":"Unlocked"},
                {"volume_status":"FullyEncrypted", "protection_status":"Off", "encryption_percentage":99, "lock_status":"Unlocked"},
                {"volume_status":"FullyEncrypted", "protection_status":"Off", "encryption_percentage":100, "lock_status":"Locked"},
                {"volume_status":"FullyDecrypted", "protection_status":"On", "encryption_percentage":0, "lock_status":"Unlocked"},
            )
            for value in cases:
                with self.subTest(value=value):
                    self.assertFalse(preflight.bitlocker_ready_for_planned_boot_change(value))


if __name__ == "__main__":
    unittest.main()

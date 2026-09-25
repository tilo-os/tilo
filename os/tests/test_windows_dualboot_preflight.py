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

    def test_normalizes_one_efi_partition(self):
        receipt = self.baseline()
        self.assertEqual(receipt["windows_esp"]["number"], 1)
        self.assertTrue(receipt["read_only"])

    def test_requires_exactly_one_windows_esp(self):
        with self.assertRaisesRegex(ValueError, "exactly one EFI"):
            preflight.make_receipt(observed(Partitions=[]), 0, 4)

    def test_passes_only_when_all_requirements_hold(self):
        receipt = self.baseline()
        self.assertEqual(preflight.verify_receipt(receipt, receipt, 64_000), [])

    def test_blocks_bitlocker_extent_media_and_esp_drift(self):
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

    def test_accepts_suspended_encryption_without_requiring_full_decryption(self):
        self.assertTrue(preflight.bitlocker_ready_for_planned_boot_change(
            {"volume_status":"FullyEncrypted", "protection_status":"Off", "encryption_percentage":100, "lock_status":"Unlocked"}
        ))
        self.assertTrue(preflight.bitlocker_ready_for_planned_boot_change(
            {"volume_status":"FullyDecrypted", "protection_status":"Off", "encryption_percentage":0, "lock_status":"Unlocked"}
        ))

    def test_rejects_active_or_partial_bitlocker_states(self):
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

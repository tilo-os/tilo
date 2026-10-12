#!/usr/bin/env python3
"""Exercise the OS pairing probes against person-scoped fixtures, without a VM."""
import json
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

DEV = Path(__file__).resolve().parents[1] / "dev"
PAIRING = (DEV / "test-nearby-pairing.sh").read_text()
JOINED = (DEV / "test-joined-device-startup.sh").read_text()
IDENTITY = "11" * 32
OTHER = "22" * 32
A = "aa" * 32
B = "bb" * 32


def function(source, name):
    match = re.search(r"(?ms)^" + name + r"\(\) \{\n.*?^\}", source)
    if match is None:
        raise AssertionError("missing production function " + name)
    return match.group()


def predicate(source, artifact):
    # Execute the actual jq predicate attached to this production artifact.
    match = re.search(r"jq -e\b[^']*'([^']*)'\s*\\?\s*\"\$out/" + re.escape(artifact) + r"\"", source)
    if match is None:
        raise AssertionError("missing production predicate for " + artifact)
    return match[1]


class PersonalDevicesProbeTest(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name) / "peers"
        self.root.mkdir()
        self.person = self.root / IDENTITY
        self.person.mkdir()
        self.entries = [{"identity": IDENTITY, "device": A}, {"identity": IDENTITY, "device": B}]
        self.path = self.person / "personal-device-peers.json"
        self.path.write_text(json.dumps(self.entries))

    def probe(self, prelude=""):
        definition = function(PAIRING, "personal_devices").replace("/var/lib/tilo/peers", str(self.root))
        runner = '''set -euo pipefail
identity=$1
vm_a() { [ "$1" = ssh ]; bash -c "$2"; }
'''
        runner += prelude + "\n" + definition + "\npersonal_devices vm_a\n"
        return subprocess.run(["bash", "-c", runner, "fixture", IDENTITY],
                              capture_output=True, text=True, timeout=5)

    def test_reads_only_this_person_not_global_or_another_identity(self):
        (self.root / "personal-device-peers.json").write_text(json.dumps(["legacy-global"]))
        foreign = self.root / OTHER
        foreign.mkdir()
        (foreign / "personal-device-peers.json").write_text(json.dumps([{"identity": OTHER, "device": A}]))
        result = self.probe()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), self.entries)

    def test_refused_join_without_person_directory_has_no_devices(self):
        (self.root / "personal-device-peers.json").write_text(json.dumps([A, B]))
        self.path.unlink()
        self.person.rmdir()
        result = self.probe()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), [])

    def test_missing_list_is_empty_before_commit(self):
        self.path.unlink()
        result = self.probe()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), [])

    def test_missing_shared_layout_fails_instead_of_claiming_no_devices(self):
        self.path.unlink()
        self.person.rmdir()
        self.root.rmdir()
        result = self.probe()
        self.assertNotEqual(result.returncode, 0)
        self.assertNotEqual(result.stdout, "[]")

    def test_existing_list_read_error_is_not_hidden(self):
        # A deterministic read refusal works even when the test runner is root.
        result = self.probe("cat() { echo 'fixture read denied' >&2; return 73; }; export -f cat")
        self.assertEqual(result.returncode, 73)
        self.assertNotEqual(result.stdout, "[]")

    def test_broken_person_entry_fails_instead_of_claiming_no_devices(self):
        self.path.unlink()
        self.person.rmdir()
        self.person.symlink_to(self.root / "absent")
        result = self.probe()
        self.assertNotEqual(result.returncode, 0)
        self.assertNotEqual(result.stdout, "[]")

    def test_broken_list_fails_instead_of_claiming_no_devices(self):
        self.path.unlink()
        self.path.symlink_to(self.person / "absent")
        result = self.probe()
        self.assertNotEqual(result.returncode, 0)
        self.assertNotEqual(result.stdout, "[]")


class PersonalDevicesAssertionsTest(unittest.TestCase):
    def check(self, source, artifact, entries):
        result = subprocess.run(["jq", "-e", "--arg", "identity", IDENTITY,
                                 "--arg", "a", A, "--arg", "b", B, "--arg", "device", B,
                                 predicate(source, artifact)], input=json.dumps(entries),
                                capture_output=True, text=True, timeout=5)
        return result.returncode == 0

    def test_paired_devices_need_exactly_both_devices_bound_to_this_identity(self):
        artifact = "$each-personal-devices.json"
        entries = [{"identity": IDENTITY, "device": B}, {"identity": IDENTITY, "device": A}]
        self.assertTrue(self.check(PAIRING, artifact, entries))
        for invalid in ([A, B], [], entries[:1], entries + entries[:1],
                        [{"identity": OTHER, "device": B}, entries[1]]):
            with self.subTest(entries=invalid):
                self.assertFalse(self.check(PAIRING, artifact, invalid))

    def test_refused_device_cannot_be_recorded_by_approver(self):
        artifact = "a-personal-devices-refused.json"
        self.assertTrue(self.check(PAIRING, artifact, [{"identity": IDENTITY, "device": A}]))
        for invalid in ([{"identity": IDENTITY, "device": B}], [A],
                        [{"identity": OTHER, "device": A}]):
            with self.subTest(entries=invalid):
                self.assertFalse(self.check(PAIRING, artifact, invalid))

    def test_refused_join_is_an_empty_list_not_an_empty_object(self):
        artifact = "b-personal-devices-refused.json"
        self.assertTrue(self.check(PAIRING, artifact, []))
        self.assertFalse(self.check(PAIRING, artifact, {}))
        self.assertFalse(self.check(PAIRING, artifact, [{"identity": IDENTITY, "device": B}]))

    def test_out_of_band_join_records_only_its_certified_device(self):
        artifact = "joined-personal-devices.json"
        self.assertTrue(self.check(JOINED, artifact, [{"identity": IDENTITY, "device": B}]))
        for invalid in ([B], [], [{"identity": OTHER, "device": B}],
                        [{"identity": IDENTITY, "device": A}]):
            with self.subTest(entries=invalid):
                self.assertFalse(self.check(JOINED, artifact, invalid))


if __name__ == "__main__":
    unittest.main()

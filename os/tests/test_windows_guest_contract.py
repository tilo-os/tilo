#!/usr/bin/env python3
"""Test rejection of stale/incomplete/mutated frozen guest input contracts."""

import importlib.util
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

OS = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("guest_contract", OS / "dev/windows_guest_contract.py")
CONTRACT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONTRACT)
REF = "registry.example/tilo/guest-buildroot@sha256:" + "a" * 64


def fixture_header(path):
    stem, arch = path.name[:-4].rsplit(".", 1)
    name, version, release = stem.rsplit("-", 2)
    return {"name": name, "epoch": "0", "version": version,
            "release": release, "architecture": arch}


class ContractTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.packages = self.root / "packages.txt"
        self.packages.write_text("wine.x86_64\nwine.i686\n")
        self.repo = self.root / "repo"
        self.repo.mkdir()
        header_query = patch.object(CONTRACT, "read_rpm_header", side_effect=fixture_header)
        header_query.start()
        self.addCleanup(header_query.stop)
        for arch in ("x86_64", "i686"):
            (self.repo / f"wine-11.0-3.fc44.{arch}.rpm").write_bytes(b"fixture " + arch.encode())
        self.record()

    def record(self):
        self.contract = CONTRACT.snapshot_contract(self.packages, self.repo, REF)
        (self.repo / "windows-guest-contract.json").write_text(json.dumps(self.contract))

    def verify(self):
        return CONTRACT.verify_snapshot(self.packages, self.repo, REF)

    def test_complete_contract_is_stable(self):
        self.assertEqual(self.verify(), self.contract)
        self.assertEqual(self.contract["transaction_base"], "empty-installroot")
        self.assertEqual(self.contract["package_roots"], ["wine.i686", "wine.x86_64"])

    def test_development_wine_trust_is_bound_to_snapshot(self):
        trust = self.repo / 'development-wine'
        trust.mkdir()
        for name in ('public.asc', 'paired-contract.json', 'development-wine.json'):
            (trust / name).write_text('public fixture ' + name)
        (trust / 'reference.txt').write_text(REF + '\n')
        self.record()
        self.assertTrue(self.verify()['development_wine']['development_only'])
        (trust / 'public.asc').write_text('changed trust anchor')
        with self.assertRaisesRegex(ValueError, 'development_wine'):
            self.verify()

    def test_development_wine_snapshot_rejects_unexpected_trust_files(self):
        trust = self.repo / 'development-wine'
        trust.mkdir()
        (trust / 'private.key').write_text('must never be exported')
        with self.assertRaisesRegex(ValueError, 'unexpected files'):
            self.record()

    def test_package_root_changes_invalidate_snapshot(self):
        self.packages.write_text("wine.x86_64\nwine.i686\npython3.x86_64\n")
        with self.assertRaisesRegex(ValueError, "package_roots"):
            self.verify()

    def test_removed_added_and_changed_rpms_rejected(self):
        target = self.repo / "wine-11.0-3.fc44.i686.rpm"
        original = target.read_bytes()
        target.unlink()
        with self.assertRaisesRegex(ValueError, "rpms"):
            self.verify()
        target.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "rpms"):
            self.verify()
        target.write_bytes(original)
        (self.repo / "extra-1-1.noarch.rpm").write_bytes(b"extra")
        with self.assertRaisesRegex(ValueError, "rpms"):
            self.verify()

    def test_buildroot_mismatch_rejected(self):
        with self.assertRaisesRegex(ValueError, "buildroot_ref"):
            CONTRACT.verify_snapshot(self.packages, self.repo, REF.replace("a" * 64, "b" * 64))

    def test_duplicate_nevra_with_distinct_filename_rejected(self):
        duplicate = self.repo / "zz-1-1.x86_64.rpm"
        duplicate.write_bytes(b"duplicate")
        original = self.repo / "wine-11.0-3.fc44.x86_64.rpm"
        with patch.object(CONTRACT, "read_rpm_header", side_effect=lambda path:
                          fixture_header(original if path == duplicate else path)):
            with self.assertRaisesRegex(ValueError, "duplicate RPM NEVRA"):
                CONTRACT.rpm_inventory(self.repo)

    def test_cache_collisions_must_have_identical_bytes(self):
        first, second = self.root / "cache-a", self.root / "cache-b"
        first.mkdir()
        second.mkdir()
        name = "extra-1-1.noarch.rpm"
        (first / name).write_bytes(b"original")
        (second / name).write_bytes(b"original")
        CONTRACT.collect_rpms([first, second], self.repo)
        self.assertEqual((self.repo / name).read_bytes(), b"original")
        (second / name).write_bytes(b"conflicting")
        with self.assertRaisesRegex(ValueError, "collide"):
            CONTRACT.collect_rpms([first, second], self.repo)
        self.assertEqual((self.repo / name).read_bytes(), b"original")

    def test_base_relative_snapshot_is_not_accepted(self):
        self.contract["transaction_base"] = "fedora-bootc-base-delta"
        (self.repo / "windows-guest-contract.json").write_text(json.dumps(self.contract))
        with self.assertRaisesRegex(ValueError, "transaction_base"):
            self.verify()

    def test_rejects_wrong_architecture_empty_and_symlink_rpms(self):
        wrong = self.repo / "foo-1-1.aarch64.rpm"
        wrong.write_bytes(b"arm")
        with self.assertRaisesRegex(ValueError, "architecture"):
            CONTRACT.rpm_inventory(self.repo)
        wrong.unlink()
        empty = self.repo / "empty-1-1.noarch.rpm"
        empty.touch()
        with self.assertRaisesRegex(ValueError, "empty RPM"):
            CONTRACT.rpm_inventory(self.repo)
        empty.unlink()
        try:
            empty.symlink_to(self.repo / "wine-11.0-3.fc44.i686.rpm")
        except OSError:
            self.skipTest("symlinks unavailable")
        with self.assertRaisesRegex(ValueError, "non-symlink"):
            CONTRACT.rpm_inventory(self.repo)

    def test_unpinned_references_rejected(self):
        for value in ("image:latest", "image@sha256:abcd", "image@sha256:" + "A" * 64, REF + "\n"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                CONTRACT.immutable_ref(value)

    def test_invalid_or_duplicate_roots_rejected(self):
        for text in ("", "wine\n", "wine.aarch64\n", "wine.*\n", "wine.x86_64;touch /x\n",
                     "wine.x86_64\nwine.x86_64\n"):
            with self.subTest(text=text):
                self.packages.write_text(text)
                with self.assertRaises(ValueError):
                    CONTRACT.package_roots(self.packages)

    def test_manifest_preserves_features_with_separate_dxvk_payload(self):
        roots = set(CONTRACT.package_roots(OS / "dev/windows-guest-packages.txt"))
        for package in ("wine", "wine-alsa", "mesa-dri-drivers", "mesa-vulkan-drivers",
                        "mesa-libGL", "mesa-libEGL", "vulkan-loader"):
            for arch in ("x86_64", "i686"):
                self.assertIn(f"{package}.{arch}", roots)
        for package in ("glibc-locale-source",
                        "glibc-langpack-en", "python3"):
            self.assertIn(f"{package}.x86_64", roots)
        self.assertFalse(any(name.startswith("wine-dxvk") for name in roots))
        self.assertFalse(any("-devel." in name for name in roots))


class HeaderTests(unittest.TestCase):
    def query(self, row, status=0):
        with patch.object(CONTRACT.subprocess, "run",
                          return_value=SimpleNamespace(stdout=row, returncode=status)):
            return CONTRACT.read_rpm_header(Path("untrusted-name.x86_64.rpm"))

    def test_header_architecture_not_filename_is_authoritative(self):
        for arch in ("aarch64", "riscv64", "src"):
            with self.subTest(arch=arch), self.assertRaisesRegex(ValueError, "architecture/type"):
                self.query(f"wine\t0\t11.0\t3.fc44\t{arch}\t(none)\n")
        for arch in ("x86_64", "i686", "noarch"):
            self.assertEqual(self.query(f"wine\t0\t11.0\t3.fc44\t{arch}\t(none)\n")["architecture"], arch)

    def test_renamed_source_rpm_rejected(self):
        with self.assertRaisesRegex(ValueError, "architecture/type"):
            self.query("wine\t0\t11.0\t3.fc44\tx86_64\t1\n")

    def test_failed_or_malformed_header_query_rejected(self):
        for row, status in (("wine\t0", 0), ("wine\t0\t11.0\t3.fc44\tx86_64\t(none)\n", 1)):
            with self.subTest(row=row), self.assertRaisesRegex(ValueError, "valid RPM header"):
                self.query(row, status)


if __name__ == "__main__":
    unittest.main()

"""Exercise the provider packager on private synthetic archives, without Java."""
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "os/scripts/stage-signal-runtime.py"
spec = importlib.util.spec_from_file_location("signal_runtime_staging", SCRIPT)
staging = importlib.util.module_from_spec(spec)
spec.loader.exec_module(staging)


def archive(path, files, links=()):
    with tarfile.open(path, "w:gz") as output:
        for name, data in files.items():
            entry = tarfile.TarInfo(name)
            entry.size = len(data)
            entry.mode = 0o755 if name.endswith("bin/java") else 0o644
            output.addfile(entry, io.BytesIO(data))
        for name, target in links:
            entry = tarfile.TarInfo(name)
            entry.type = tarfile.SYMTYPE
            entry.linkname = target
            output.addfile(entry)


class SignalRuntimeStaging(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.assets = self.root / "assets"
        self.assets.mkdir()
        self.out = self.root / "signal-provider"
        self.pin = copy.deepcopy(json.loads(staging.PIN.read_text()))
        self.java_root = self.pin["assets"]["java"]["root"]
        self.java_files = {
            self.java_root + "/bin/java": b"\x7fELF\x02\x01" + bytes(12) + b"\x3e\x00",
            self.java_root + "/release": b'JAVA_VERSION="25.0.4.1"\n',
            self.java_root + "/legal/java.base/LICENSE": b"synthetic license",
        }
        self.write_java(links=[(self.java_root + "/legal/LICENSE", "java.base/LICENSE")])
        archive(self.path("signal"), {
            "signal-cli-0.14.8/lib/signal-cli-0.14.8.jar": b"synthetic classes",
            "signal-cli-0.14.8/lib/libsignal-client-0.102.1.jar": b"synthetic native classes",
        })
        self.path("license").write_bytes(b"synthetic Signal license")
        self.path("source").write_bytes(b"synthetic corresponding source")
        self.hashes()

    def path(self, kind):
        return self.assets / self.pin["assets"][kind]["name"]

    def hashes(self):
        for kind, asset in self.pin["assets"].items():
            asset["sha256"] = hashlib.sha256(self.path(kind).read_bytes()).hexdigest()

    def write_java(self, links=()):
        archive(self.path("java"), self.java_files, links)

    def stage(self):
        staging.stage(self.assets, self.out, self.pin, "x86_64")

    def test_stages_owned_plain_bytes_and_retains_provenance_and_licenses(self):
        self.stage()
        self.assertFalse(any(path.is_symlink() for path in self.out.rglob("*")))
        self.assertEqual((self.out / "java/legal/LICENSE").read_bytes(), b"synthetic license")
        self.assertEqual((self.out / "signal/LICENSE").read_bytes(), b"synthetic Signal license")
        self.assertEqual((self.out / "signal/source.tar.gz").read_bytes(),
                         b"synthetic corresponding source")
        self.assertEqual(json.loads((self.out / "provenance.json").read_text()), self.pin)
        self.assertTrue((self.out / "java/bin/java").stat().st_mode & 0o100)

    def test_refuses_wrong_digest_without_publishing_partial_output(self):
        self.path("signal").write_bytes(b"different bytes")
        with self.assertRaisesRegex(ValueError, "digest differs"):
            self.stage()
        self.assertFalse(self.out.exists())

    def test_refuses_missing_asset(self):
        self.path("license").unlink()
        with self.assertRaisesRegex(ValueError, "missing pinned"):
            self.stage()
        self.assertFalse(self.out.exists())

    def test_refuses_unqualified_architecture(self):
        with self.assertRaisesRegex(ValueError, "only x86_64"):
            staging.stage(self.assets, self.out, self.pin, "aarch64")
        self.assertFalse(self.out.exists())

    def test_does_not_replace_an_existing_output(self):
        self.out.mkdir()
        sentinel = self.out / "original"
        sentinel.write_bytes(b"owned by another operation")
        with self.assertRaisesRegex(ValueError, "already exists"):
            self.stage()
        self.assertEqual(sentinel.read_bytes(), b"owned by another operation")

    def test_refuses_archive_traversal_before_extraction(self):
        self.java_files["../escaped"] = b"escape"
        self.write_java()
        self.hashes()
        with self.assertRaisesRegex(ValueError, "escapes"):
            self.stage()
        self.assertFalse(self.out.exists())

    def test_refuses_archive_link_outside_the_payload(self):
        self.write_java(links=[(self.java_root + "/foreign", "/etc/passwd")])
        self.hashes()
        with self.assertRaises(tarfile.FilterError):
            self.stage()
        self.assertFalse(self.out.exists())

    def test_refuses_a_foreign_java_elf(self):
        self.java_files[self.java_root + "/bin/java"] = b"\x7fELF\x02\x01" + bytes(12) + b"\xb7\x00"
        self.write_java()
        self.hashes()
        with self.assertRaisesRegex(ValueError, "foreign executable"):
            self.stage()
        self.assertFalse(self.out.exists())

    def test_refuses_a_different_java_release(self):
        self.java_files[self.java_root + "/release"] = b'JAVA_VERSION="21"\n'
        self.write_java()
        self.hashes()
        with self.assertRaisesRegex(ValueError, "version differs"):
            self.stage()
        self.assertFalse(self.out.exists())

    def test_repository_asset_inventory_matches_the_pin(self):
        assets = {}
        for line in (ROOT / "os/external-assets/github-assets.tsv").read_text().splitlines():
            if line and not line.startswith("#"):
                digest, name, url = line.split("\t")
                self.assertNotIn(name, assets)
                assets[name] = (digest, url)
        pin = json.loads(staging.PIN.read_text())
        for asset in [*pin["assets"].values(), pin["java_zero_source"]]:
            self.assertEqual(assets[asset["name"]], (asset["sha256"], asset["url"]))
        spec = importlib.util.spec_from_file_location("signal_release", ROOT / "tools/apps/signal_provider_release.py")
        release = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(release)
        proof = release.provenance(release.release.head())
        self.assertEqual(proof["runtime"], "tilo-runtime/" + pin["runtime"])
        self.assertEqual(proof["pin"], pin)


if __name__ == "__main__":
    unittest.main()

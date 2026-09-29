"""Build / OS dev picks x86_64 for pull requests and adds aarch64 when needed."""
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("os_dev_arches", ROOT / "tools/ci/os_dev_arches.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class OsDevArchesTests(unittest.TestCase):
    def test_architecture_choice(self):
        both = ["x86_64", "aarch64"]
        self.assertEqual(MODULE.arches("push"), both)
        self.assertEqual(MODULE.arches("pull_request", paths=["os/etc/chrony.conf"]), ["x86_64"])
        self.assertEqual(MODULE.arches("pull_request", labels=["os-arm64"]), both)
        for path in ("os/kernel/config/aarch64", "os/hardware/profiles/lenovo-83ed.toml",
                     "os/wpe-buildroot-snapshot.env", "os/dev/uki.sh",
                     "os/external-assets/fex-source.env"):
            with self.subTest(path=path):
                self.assertEqual(MODULE.arches("pull_request", paths=[path]), both)
        self.assertEqual(MODULE.arch_specific(["os/Containerfile", "os/kernel/x"]), ["os/kernel/x"])


if __name__ == "__main__":
    unittest.main()

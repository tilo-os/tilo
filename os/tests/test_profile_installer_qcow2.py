#!/usr/bin/env python3
"""Small qcow2 integration test for the CI install-profile tail."""

import hashlib
import importlib.util
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


HELPER = Path(__file__).resolve().parents[1] / "dev/profile-installer-qcow2.py"
spec = importlib.util.spec_from_file_location("profile_installer_qcow2", HELPER)
profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profile)


@unittest.skipUnless(shutil.which("qemu-img") and shutil.which("qemu-io"),
                     "qemu-img and qemu-io are required")
class ProfileQcow2Tests(unittest.TestCase):
    def test_overlay_contains_only_verified_tail_and_preserves_source(self):
        with tempfile.TemporaryDirectory(prefix="tilo-profile-test-") as temporary:
            root = Path(temporary)
            source = root / "signed source.qcow2"
            output = root / "profiled overlay.qcow2"
            encoder = root / "encoder"
            encoder.write_text("""#!/usr/bin/env python3
from pathlib import Path
import sys
target = Path(sys.argv[sys.argv.index('--target') + 1])
assert '--hostname' in sys.argv and 'cycle-tilo' in sys.argv
target.write_bytes(bytes(range(256)) * 256)
""")
            encoder.chmod(0o755)
            subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", str(source), "1M"],
                           check=True)
            subprocess.run(["qemu-io", "-f", "qcow2", "-c", "write -P 90 0 4096",
                            str(source)], check=True, capture_output=True)
            before = hashlib.sha256(source.read_bytes()).digest()
            profile.profile(source, output, encoder, ["--hostname", "cycle-tilo"])
            self.assertEqual(before, hashlib.sha256(source.read_bytes()).digest())
            self.assertEqual(profile.image_info(source)["virtual-size"], 1024 * 1024)
            self.assertEqual(profile.image_info(output)["virtual-size"], 1024 * 1024 + 65536)
            self.assertEqual(Path(profile.image_info(output)["full-backing-filename"]),
                             source.resolve())
            self.assertEqual(profile.read_tail(output, 1024 * 1024),
                             bytes(range(256)) * 256)
            subprocess.run(["qemu-io", "-r", "-f", "qcow2", "-c", "read -P 90 0 4096",
                            str(output)], check=True, capture_output=True)
            self.assertLess(output.stat().st_size, 1024 * 1024)


if __name__ == "__main__":
    unittest.main()

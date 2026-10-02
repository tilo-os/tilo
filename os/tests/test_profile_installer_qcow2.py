#!/usr/bin/env python3
"""Small qcow2 integration test for the CI install-profile tail."""

import hashlib
import importlib.util
import os
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
    def test_recovery_handoff_roundtrips_through_installer_without_identity(self):
        with tempfile.TemporaryDirectory(prefix="tilo-handoff-test-") as temporary:
            root = Path(temporary)
            source, output = root / "source.qcow2", root / "profiled.qcow2"
            encoder = root / "encoder"
            encoder.write_text("#!/usr/bin/env python3\n")
            encoder.chmod(0o755)
            subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", str(source), "1M"],
                           check=True)
            before = hashlib.sha256(source.read_bytes()).digest()
            handoff = root / "handoff"
            expected = {
                "etc/NetworkManager/system-connections/tilo-setup.nmconnection":
                    b"[connection]\nid=recovery\n[wifi]\nssid=studio\n",
                "etc/tilo/bringup-ssh.pub": b"ssh-ed25519 fixture-public-key\n",
            }
            for name, content in expected.items():
                path = handoff / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content)
            profile.profile(source, output, encoder, [], handoff)
            raw, extracted = root / "media.raw", root / "extracted"
            subprocess.run(["qemu-img", "convert", "-f", "qcow2", "-O", "raw",
                            str(output), str(raw)], check=True)
            subprocess.run(["bash", str(HELPER.parents[1] / "usr/libexec/tilo-installer-profile-extract")],
                           check=True, capture_output=True, env={**os.environ,
                               "TILO_INSTALLER_PROFILE_DEVICE": str(raw),
                               "TILO_INSTALLER_PROFILE_ALLOW_FILE": "1",
                               "TILO_INSTALLER_PROFILE_OUT_DIR": str(extracted)})
            unpacked = extracted / "setup-handoff"
            self.assertEqual({path.relative_to(unpacked).as_posix(): path.read_bytes()
                              for path in unpacked.rglob("*") if path.is_file()}, expected)
            wifi = unpacked / "etc/NetworkManager/system-connections/tilo-setup.nmconnection"
            self.assertEqual(wifi.stat().st_mode & 0o777, 0o600)
            self.assertEqual(unpacked.stat().st_mode & 0o777, 0o700)
            self.assertEqual(before, hashlib.sha256(source.read_bytes()).digest())
            self.assertLess(profile.image_info(output)["virtual-size"] - 1024 * 1024,
                            72 * 1024)

    def test_handoff_rejects_identity_and_redirected_files(self):
        with tempfile.TemporaryDirectory(prefix="tilo-handoff-refusal-") as temporary:
            root = Path(temporary)
            identity = root / "var/lib/tilo/device-private/keypair.bin"
            identity.parent.mkdir(parents=True)
            identity.write_bytes(b"must not preconfigure personal identity")
            with self.assertRaisesRegex(ValueError, "only Wi-Fi"):
                profile.handoff_payload(root)
            identity.unlink()
            key = root / "etc/tilo/bringup-ssh.pub"
            key.parent.mkdir(parents=True)
            key.symlink_to(identity)
            with self.assertRaisesRegex(ValueError, "redirected"):
                profile.handoff_payload(root)

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

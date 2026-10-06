#!/usr/bin/env python3
"""Exercise owner-key provisioning and refusal on an offline deployment."""

import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("dev_ssh", ROOT / "os/usr/libexec/tilo-install-dev-ssh.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ProvisionTests(unittest.TestCase):
    def test_pending_oobe_and_key_only_recovery(self):
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            etc = root / "etc"
            etc.mkdir()
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(root / "key")], check=True)
            labels = []
            module.apply(etc, "bob", root / "key.pub", lambda path, name: labels.append(name))
            self.assertEqual((etc / "ssh/authorized_keys/bob").read_text().split()[:2],
                             (root / "key.pub").read_text().split()[:2])
            config = (etc / "ssh/sshd_config.d/00-tilo-bringup.conf").read_text()
            for directive in ("AllowUsers bob", "PasswordAuthentication no", "PermitRootLogin no",
                              "AuthorizedKeysFile /etc/ssh/authorized_keys/%u"):
                self.assertIn(directive, config)
            enable = etc / "systemd/system/multi-user.target.wants/sshd.service"
            self.assertEqual(str(enable.readlink()), "/usr/lib/systemd/system/sshd.service")
            self.assertIn("/etc/ssh/authorized_keys/bob", labels)
            self.assertFalse((etc / "tilo/.firstboot-done").exists())
            self.assertFalse((root / "var/lib/tilo/device-private").exists())
            with self.assertRaisesRegex(ValueError, "already exists"):
                module.apply(etc, "bob", root / "key.pub", lambda *_: None)

    def test_symlink_and_key_options_refused(self):
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            etc = root / "etc"
            etc.mkdir()
            key = root / "key"
            key.write_text('command="id" ssh-ed25519 AAAA\n')
            with self.assertRaises(ValueError):
                module.apply(etc, "bob", key, lambda *_: None)
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(root / "real")], check=True)
            outside = root / "outside"
            outside.mkdir()
            (etc / "ssh").symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "symlink"):
                module.apply(etc, "bob", root / "real.pub", lambda *_: None)
            self.assertEqual(list(outside.iterdir()), [])

    def test_installer_requires_dev_self_install(self):
        backend = (ROOT / "os/usr/libexec/tilo-install").read_text()
        gate = backend.index('[ "$OFFLINE_TARGET" -eq 0 ]', backend.index('SSH_REQUEST='))
        apply = backend.index("python3 /usr/libexec/tilo-install-dev-ssh.py", gate)
        self.assertIn("/usr/lib/tilo/image-flavour", backend[gate:apply])
        self.assertIn("/usr/sbin/sshd", backend[gate:apply])
        self.assertLess(backend.index('rm -f "$DEPLOY_ETC/systemd/system/sshd.service"'), apply)


if __name__ == "__main__":
    unittest.main()

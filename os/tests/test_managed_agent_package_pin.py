"""Exercise image pin emission through the real packaging orchestration.

Package tools are fixtures: no builds, signing keys or package-format crypto
are exercised here. Production verification remains the packaging CLI's job.
"""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'os/scripts/pack-apps.sh'
HASH = 'ab' * 32
PUBLISHER = 'cd' * 32


class ManagedAgentPinTests(unittest.TestCase):
    def exercise(self, *, app='so.tilo.agent', digest=HASH, fail='', duplicate=False,
                 expected=PUBLISHER):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archives = root / 'archives'
            archives.mkdir()
            # Inspect reports the signed root separately from the manifest id.
            details = (f'package-id: tapp:{digest}\n'
                       f'id = "{app}"\nidentity = "ed25519:{PUBLISHER}"\n')
            (archives / 'executor.tapp').write_text(details)
            if duplicate:
                (archives / 'duplicate.tapp').write_text(details)
            package = root / 'package'
            package.write_text('''#!/bin/bash
set -eu
case "$1" in
 verify) [ "$FAIL" != verify ] ;;
 inspect) cat "$2" ;;
 *) exit 19 ;;
esac
''')
            installer = root / 'installer'
            installer.write_text('''#!/bin/bash
set -eu
[ "$FAIL" != install ] || exit 18
printf '[[entry]]\nid="unrelated-registry-data"\n' >> "$TILO_APPS_DATA_DIR/registry.toml"
''')
            package.chmod(0o755)
            installer.chmod(0o755)
            pin = root / 'protected-output' / 'hash'
            env = {'PATH': os.defpath, 'HOME': str(root), 'TMPDIR': str(root),
                   'TAPP_DIR': str(archives), 'STORE': str(root / 'store'),
                   'EXPECTED_PUBKEY': expected, 'FAIL': fail,
                   'PKG_BIN': str(package), 'INSTALL_BIN': str(installer),
                   'BUNDLE_BIN': '/bin/true', 'SHIM_BIN': '/missing',
                   'MANAGED_AGENT_HASH_OUT': str(pin)}
            result = subprocess.run(['bash', str(SCRIPT)], env=env,
                                    capture_output=True, text=True)
            success = (app == 'so.tilo.agent' and digest == HASH and not fail
                       and not duplicate and expected == PUBLISHER)
            if success:
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(pin.read_text(), HASH + '\n')
                self.assertEqual(pin.stat().st_mode & 0o777, 0o644)
            else:
                self.assertNotEqual(result.returncode, 0, result.stderr)
                self.assertFalse(pin.exists(), 'failed collection emitted a pin')

    def test_pin_uses_verified_archive_not_registry(self):
        self.exercise()

    def test_failed_verification_or_install_does_not_emit_pin(self):
        for fail in ('verify', 'install'):
            with self.subTest(fail=fail):
                self.exercise(fail=fail)

    def test_collection_publisher_must_match(self):
        self.exercise(expected='ef' * 32)

    def test_fixed_executor_is_required(self):
        self.exercise(app='so.tilo.other')

    def test_duplicate_executor_is_rejected(self):
        self.exercise(duplicate=True)

    def test_signed_root_hash_is_required(self):
        self.exercise(digest='not-a-hash')

    def test_image_installs_pin_with_protected_metadata(self):
        containerfile = (ROOT / 'os/Containerfile').read_text()
        self.assertIn('MANAGED_AGENT_HASH_OUT=/out/managed-agent-package-hash', containerfile)
        self.assertIn('COPY --from=native-builder-payload --chown=0:0 --chmod=0644 \\\n'
                      '    /out/managed-agent-package-hash /usr/lib/tilo/managed-agent-package-hash',
                      containerfile)
        self.assertIn('info.st_uid == 0 and not info.st_mode & 0o022', containerfile)
        self.assertIn('path.lstat()', containerfile)


if __name__ == '__main__':
    unittest.main()

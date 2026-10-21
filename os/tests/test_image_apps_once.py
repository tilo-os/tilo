"""The image's app store carries each app once: as its installed tree.

Runs the actual prebuilt-collection path of the image packaging script.
External package tools are explicit process doubles here: these tests prove
which steps run and what the store keeps, not cryptographic signature
validity (crates/dev/tilo-vmd/src/cohort.rs rebuilds and checks archives).
"""
import os
import shutil
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
PACK = REPO / 'os/scripts/pack-apps.sh'
KEY = '1' * 64


class ImageAppsOnce(unittest.TestCase):
    def exercise(self, failure=None, extra=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            inputs = root / 'inputs'
            inputs.mkdir()
            source = inputs / 'settings-0.1.42-aarch64.tapp'
            source.write_bytes(b'exact already signed archive fixture')
            tools = root / 'tools'
            tools.mkdir()
            calls = root / 'calls'
            package = tools / 'package'
            package.write_text('''#!/bin/bash
set -eu
printf '%s\\n' "$1" >> "$CALLS"
case "$1" in
 verify) [ "$FAILURE" != verify ] ;;
 inspect) printf 'identity = "ed25519:%s"\\nid = "so.tilo.settings"\\n' "$KEY" ;;
 *) exit 23 ;;
esac
''')
            install = tools / 'install'
            install.write_text('''#!/bin/bash
set -eu
printf '%s\\n' install >> "$CALLS"
[ "$FAILURE" != install ]
printf '[[entry]]\\nid="so.tilo.settings"\\n' >> "$TILO_APPS_DATA_DIR/registry.toml"
''')
            for tool in (package, install):
                tool.chmod(0o755)
            store = root / 'store'
            environment = dict(os.environ, TMPDIR=str(root), REPO=str(root),
                               STORE=str(store), TAPP_DIR=str(inputs),
                               PKG_BIN=str(package), INSTALL_BIN=str(install),
                               BUNDLE_BIN=shutil.which('true'), SHIM_BIN='/missing',
                               EXPECTED_PUBKEY=KEY, REQUIRE_MIN='1', KEY=KEY,
                               CALLS=str(calls), FAILURE=failure or '', **(extra or {}))
            result = subprocess.run(['bash', str(PACK)], env=environment,
                                    capture_output=True, text=True)
            archives = sorted(path.relative_to(store).as_posix() for path in store.rglob('*.tapp'))
            return result, archives, calls.read_text().splitlines()

    def test_prebuilt_app_is_verified_then_installed_and_kept_once(self):
        result, archives, calls = self.exercise()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, ['verify', 'inspect', 'install'])
        self.assertEqual(archives, [], 'the store keeps a second copy of an app')

    def test_a_retired_export_variable_adds_no_archive(self):
        result, archives, _ = self.exercise(extra={'COHORT_TAPP_DIR': 'cohort'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(archives, [])

    def test_refused_package_is_never_installed(self):
        for boundary in ('verify', 'install'):
            with self.subTest(boundary=boundary):
                result, archives, calls = self.exercise(failure=boundary)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(archives, [])
                if boundary == 'verify':
                    self.assertEqual(calls, ['verify'])

    def test_full_image_exports_no_archive_copy(self):
        source = (REPO / 'os/Containerfile').read_text()
        start = source.index('# ---- Pack + install every app')
        block = source[start:source.index('# Buildah leaves', start)]
        self.assertIn('/build/os/scripts/pack-apps.sh', block)
        self.assertNotIn('COHORT_TAPP_DIR', block)
        self.assertNotIn('COHORT_TAPP_DIR', PACK.read_text())


if __name__ == '__main__':
    unittest.main()

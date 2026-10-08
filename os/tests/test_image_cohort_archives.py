"""Exercise archive retention in the actual prebuilt image packaging script.

External package tools are explicit process doubles here: these tests prove
export ordering and bytes, not cryptographic signature validity.
"""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
PACK = REPO / 'os/scripts/pack-apps.sh'
KEY = '1' * 64


class ImageCohortArchives(unittest.TestCase):
    def exercise(self, failure=None, app_id='so.tilo.settings'):
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
 inspect) printf 'identity = "ed25519:%s"\\nid = "%s"\\n' "$KEY" "$APP_ID" ;;
 *) exit 23 ;;
esac
''')
            install = tools / 'install'
            install.write_text('''#!/bin/bash
set -eu
printf '%s\\n' install >> "$CALLS"
[ "$FAILURE" != install ]
printf '[[entry]]\\nid="%s"\\n' "$APP_ID" >> "$TILO_APPS_DATA_DIR/registry.toml"
''')
            for tool in (package, install):
                tool.chmod(0o755)
            archives = root / 'store/cohort'
            environment = dict(os.environ, TMPDIR=str(root), REPO=str(root),
                               STORE=str(root / 'store'), TAPP_DIR=str(inputs),
                               COHORT_TAPP_DIR=str(archives), PKG_BIN=str(package),
                               INSTALL_BIN=str(install), BUNDLE_BIN='/bin/true',
                               SHIM_BIN='/missing', EXPECTED_PUBKEY=KEY, REQUIRE_MIN='1',
                               KEY=KEY, APP_ID=app_id, CALLS=str(calls), FAILURE=failure or '')
            result = subprocess.run(['bash', str(PACK)], env=environment,
                                    capture_output=True, text=True)
            exported = {path.name: path.read_bytes() for path in archives.glob('*.tapp')}
            return result, exported, calls.read_text().splitlines(), source.read_bytes()

    def test_prebuilt_export_uses_manifest_identity_and_exact_installed_bytes(self):
        result, exported, calls, original = self.exercise()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(exported, {'so.tilo.settings.tapp': original})
        self.assertEqual(calls, ['verify', 'inspect', 'install'])

    def test_refused_package_is_never_exported(self):
        for boundary in ('verify', 'install'):
            with self.subTest(boundary=boundary):
                result, exported, calls, _ = self.exercise(failure=boundary)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(exported, {})
                if boundary == 'verify':
                    self.assertEqual(calls, ['verify'])

    def test_unsafe_manifest_identity_cannot_escape_archive_directory(self):
        for identity in ('../escape', '', 'a/b'):
            with self.subTest(identity=identity):
                result, exported, calls, _ = self.exercise(app_id=identity)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(exported, {})
                self.assertNotIn('install', calls)

    def test_full_image_enables_the_existing_cohort_export(self):
        source = (REPO / 'os/Containerfile').read_text()
        start = source.index('# ---- Pack + install every app')
        block = source[start:source.index('# Buildah leaves', start)]
        self.assertIn('COHORT_TAPP_DIR=/usr/share/tilo-apps/cohort', block)


if __name__ == '__main__':
    unittest.main()

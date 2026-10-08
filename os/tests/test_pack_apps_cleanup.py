"""Run the packaging script and check its private scratch lifecycle."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'scripts/pack-apps.sh'


class CleanupTests(unittest.TestCase):
    def exercise(self, fail=False, supplied=False, image=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scratch, store = root / 'scratch', root / 'store'
            scratch.mkdir()
            store.mkdir()
            (store / 'registry.toml').write_text('[[entry]]\nid="existing"\n')
            bundle = root / 'bundle'
            bundle.write_text('''#!/bin/bash
set -eu
case "$2" in
 generate) printf synthetic-private-key > "$4" ;;
 pubkey) [ "$FAIL_PUBKEY" != 1 ] || exit 17; printf '%064d\\n' 1 ;;
 *) exit 18 ;;
esac
''')
            bundle.chmod(0o755)
            environment = {'PATH': os.defpath, 'HOME': str(root), 'TMPDIR': str(scratch),
                           'REPO': str(root), 'STORE': str(store), 'REQUIRE_MIN': '0',
                           'BUNDLE_BIN': str(bundle), 'PKG_BIN': '/bin/true',
                           'INSTALL_BIN': '/bin/true', 'SHIM_BIN': '/missing',
                           'FAIL_PUBKEY': '1' if fail else '0',
                           'RUNTIME_ID': 'tilo-runtime/2026.07+native.' + '0' * 32}
            if image:
                environment.update(TILO_IMAGE_BUILD='1', SOURCE_DATE_EPOCH='1790502039')
            if supplied:
                stage = root / 'caller-stage'
                stage.mkdir()
                (stage / 'publisher.json').write_text('caller-owned-key')
                environment['STAGE'] = str(stage)
            result = subprocess.run(['bash', str(SCRIPT)], env=environment,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            self.assertEqual(result.returncode, 17 if fail else 0, result.stderr)
            self.assertEqual(list(scratch.iterdir()), [], 'ephemeral key directory survived')
            if image:
                self.assertIn('/tmp/tilo-image-app-pack-stage/publisher.json', result.stderr)
                self.assertFalse(Path('/tmp/tilo-image-app-pack-stage').exists())
            if supplied:
                self.assertEqual((stage / 'publisher.json').read_text(), 'caller-owned-key')

    def test_stage_cleanup_ownership(self):
        with self.subTest(phase="default_private_stage_removed_after_success"):
            self.exercise()
        with self.subTest(phase="default_private_stage_removed_after_failure"):
            self.exercise(fail=True)
        with self.subTest(phase="caller_owned_stage_and_key_preserved"):
            self.exercise(supplied=True)
        with self.subTest(phase="image_stage_is_stable_and_removed"):
            self.exercise(image=True)


if __name__ == '__main__':
    unittest.main()

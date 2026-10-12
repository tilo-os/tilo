"""Forward build evidence boundaries; fixture Cargo is not native proof."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

PATH = Path(__file__).with_name('record-capture-cargo.py')
SPEC = importlib.util.spec_from_file_location('capture_build_evidence', PATH)
PRODUCER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PRODUCER)


class CaptureBuildEvidenceTests(unittest.TestCase):
    def test_old_stream_import_and_unlocked_commands_are_refused(self):
        for arguments in (['import', 'old-compiler.jsonl'], ['build', '-p', 'imager'],
                          ['check', '--locked', '-p', 'imager']):
            with self.subTest(arguments=arguments):
                with self.assertRaisesRegex(ValueError, 'locked build/test'):
                    PRODUCER.build(arguments)

    def test_real_child_source_change_preserves_failure_but_emits_no_roles(self):
        with tempfile.TemporaryDirectory(prefix='tilo-capture-evidence-') as directory:
            root = Path(directory)
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            for name in ('Cargo.toml', 'Cargo.lock', 'apps/imager/build.rs',
                         'apps/imager/src/setup/home_capture/native_tests/signed_export.rs',
                         'apps/imager/packaging/qualify-capture-release.py',
                         'tools/ci/native-stores.sh', 'tools/ci/native-store-autoclear.py',
                         'tools/apps/pack-runtime-package.sh', 'tools/apps/stage_sdk_services.py',
                         'scripts/headless-examples/_control.py'):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('before\n')
            tools = root / 'tools-bin'
            tools.mkdir()
            cargo = tools / 'cargo'
            cargo.write_text('''#!/usr/bin/env python3
from pathlib import Path
import json
Path('Cargo.lock').write_text('changed by actual child\\n')
print(json.dumps({'reason':'build-finished','success':True}))
''')
            cargo.chmod(0o700)
            evidence = root / 'evidence'
            env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ['PATH'],
                       TILO_CAPTURE_BUILD_EVIDENCE=str(evidence))
            with mock.patch.object(PRODUCER, 'ROOT', root), mock.patch.dict(os.environ, env):
                with self.assertRaisesRegex(ValueError, 'source changed during actual Cargo build'):
                    PRODUCER.build(['build', '--locked', '-p', 'imager'])
            invocations = list(evidence.glob('cargo-*'))
            self.assertEqual(len(invocations), 1)
            status = json.loads((invocations[0] / 'build-status.json').read_text())
            self.assertEqual(status['exit_code'], 0)
            self.assertIs(status['listed_source_unchanged'], False)
            self.assertNotEqual((invocations[0] / 'source-before.json').read_bytes(),
                                (invocations[0] / 'source-after.json').read_bytes())
            self.assertEqual(list(invocations[0].glob('role-*.json')), [])
            self.assertEqual(list(invocations[0].glob('producer-*.json')), [])
            before = json.loads((invocations[0] / 'source-before.json').read_text())
            self.assertIn('scripts/headless-examples/_control.py', before)
            self.assertEqual(before['scripts/headless-examples/_control.py'],
                             PRODUCER.facts(root / 'scripts/headless-examples/_control.py'))


if __name__ == '__main__':
    unittest.main()

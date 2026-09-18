#!/usr/bin/env python3
"""Controller contract tests without Docker, credentials, or compilation."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).parents[2]
spec = importlib.util.spec_from_file_location('launch', ROOT / 'os/dev/launch-native-resume.py')
launch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(launch)


class LauncherContract(unittest.TestCase):
    @unittest.skipUnless(os.name == 'posix', 'executable-mode regression requires Linux')
    def test_staged_tool_executes_with_restrictive_umask(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'source'
            (source / 'os').mkdir(parents=True)
            recipe = source / 'os/Containerfile.development-compile'
            original = 'FROM scratch\nCOPY cosign /usr/local/bin/cosign\n'
            recipe.write_text(original)
            tool = root / 'installed-cosign'
            tool.write_text('#!/bin/sh\n[ "$1" = version ] && printf "staged-tool-ok\\n"\n')
            tool.chmod(0o755)
            build = root / 'build'
            build.mkdir()
            previous = os.umask(0o077)
            try:
                launch.stage_tools_context(source, build, tool)
            finally:
                os.umask(previous)
            staged = build / 'cosign'
            self.assertEqual(staged.stat().st_mode & 0o777, 0o755)
            self.assertEqual(staged.read_bytes(), tool.read_bytes())
            self.assertEqual(subprocess.check_output([staged, 'version'], text=True), 'staged-tool-ok\n')
            self.assertEqual(recipe.read_text(), original)
            self.assertTrue((build / 'Containerfile').read_text().endswith(
                '\nRUN /usr/local/bin/cosign version\n'))

    def test_driver_arguments_cannot_fall_back_to_full_native_compilation(self):
        values = {name + '_SHA256': str(i) * 64 for i, name in enumerate(
            ('WPE_ADOPTION', 'CHECKPOINT_INITIAL', 'CHECKPOINT_DELTA'), 1)}
        argv = launch.driver_argv('a' * 40, values)
        self.assertEqual(argv[argv.index('--mode') + 1], 'native-payload')
        self.assertEqual(argv[argv.index('--source-dir') + 1], '/work/repo')
        self.assertEqual(argv[argv.index('--controller-revision') + 1], 'a' * 40)
        self.assertEqual(argv[argv.index('--revision') + 1], launch.SOURCE)
        self.assertEqual(argv.count('--resume-checkpoint'), 2)
        for path in ('/work/checkpoint-initial.json', '/work/checkpoint-delta.json'):
            self.assertIn(path, argv)
        self.assertNotIn('--native-builder-receipt', argv)
        self.assertNotIn('--reconstructed-native-receipt', argv)

    def test_hash_or_complete_checkpoint_rejected_before_docker(self):
        value = {'purpose': 'tilo-development-partial-native-checkpoint', 'source_revision': launch.SOURCE,
                 'complete_native_builder': True, 'build_success': False, 'installable': False}
        raw = json.dumps(value)
        environment = {'CHECKPOINT_INITIAL': raw, 'CHECKPOINT_INITIAL_SHA256': hashlib.sha256(raw.encode()).hexdigest()}
        with self.assertRaises(ValueError):
            launch.prepare_receipts(environment)
        environment['CHECKPOINT_INITIAL_SHA256'] = '0' * 64
        with self.assertRaises(ValueError):
            launch.prepare_receipts(environment)

    def test_duplicate_checkpoint_fields_rejected_before_docker(self):
        raw = '{"purpose":"x","purpose":"y"}'
        with self.assertRaises(ValueError):
            launch.prepare_receipts({'CHECKPOINT_INITIAL': raw,
                                     'CHECKPOINT_INITIAL_SHA256': hashlib.sha256(raw.encode()).hexdigest()})


if __name__ == '__main__':
    unittest.main()

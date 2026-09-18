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
                '\nRUN chmod 0755 /var/lib && /usr/local/bin/cosign version\n'))

    def extra_environment(self):
        env = {}
        entries = {}
        for label, (_, digest) in launch.EXTRA_PARTIALS.items():
            value = {'purpose':'tilo-development-reconstructed-partial-native-checkpoint',
                'source_revision':launch.SOURCE,'helper_revision':'9d7a6b259c9b75b6c5031472bfed05b6461c1893',
                'run_id':21554,'run_number':8166,'installable':False,'complete_native_builder':False,
                'build_success':False,'resume_admitted':False,'inventory':{label:{'size':1,'sha256':'a'*64}},
                'reference':'code.podesta.ai/qos/tilo-development-partial-native-aarch64@sha256:'+digest}
            raw=json.dumps(value);sha=hashlib.sha256(raw.encode()).hexdigest()
            env[label]=raw;env[label+'_SHA256']=sha;entries[sha]={'inventory':value['inventory']}
        raw=json.dumps({'purpose':'tilo-reconstructed-partial-admission','receipts':entries})
        env[launch.ADMISSION]=raw;env[launch.ADMISSION+'_SHA256']=hashlib.sha256(raw.encode()).hexdigest()
        return env

    def test_four_checkpoint_and_admission_arguments(self):
        env=self.extra_environment()
        receipts=launch.prepare_extra_partials(env,dict)
        self.assertEqual(set(receipts),set(launch.EXTRA_PARTIALS)|{launch.ADMISSION})
        env.update({key+'_SHA256':'a'*64 for key in ['WPE_ADOPTION','CHECKPOINT_INITIAL','CHECKPOINT_DELTA']})
        argv=launch.driver_argv('a'*40,env)
        self.assertEqual(argv.count('--resume-checkpoint'),4)
        self.assertIn('--resume-reconstructed-partial-admission',argv)
        self.assertIn('/work/reconstructed-partial-admission.json',argv)

    def test_partial_opt_in_or_substitution_rejected(self):
        env=self.extra_environment();env.pop('CHECKPOINT_WINDOWS_SHA256')
        with self.assertRaises(ValueError):launch.prepare_extra_partials(env,dict)
        env=self.extra_environment();env['CHECKPOINT_RELEASE_SHA256']='0'*64
        with self.assertRaises(ValueError):launch.prepare_extra_partials(env,dict)
        env=self.extra_environment();value=json.loads(env['CHECKPOINT_RELEASE']);value['run_id']=20866
        env['CHECKPOINT_RELEASE']=json.dumps(value);env['CHECKPOINT_RELEASE_SHA256']=hashlib.sha256(env['CHECKPOINT_RELEASE'].encode()).hexdigest()
        with self.assertRaises(ValueError):launch.prepare_extra_partials(env,dict)
        self.assertEqual(launch.prepare_extra_partials({},dict),{})

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

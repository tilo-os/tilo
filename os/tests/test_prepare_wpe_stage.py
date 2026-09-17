import argparse
import importlib.util
import json
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location('wpe_stage',
    Path(__file__).resolve().parents[1] / 'dev/prepare-wpe-stage.py')
wpe = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(wpe)


class WpeStageTests(unittest.TestCase):
    def test_failure_diagnostics_keep_compiler_context_without_registry_auth(self):
        raw = (b'error authenticating registry token PRIVATE\n'
               b'Source/file.cpp:4:2: error: missing symbol\n'
               b'CMake Error at CMakeLists.txt:5 (find_package):\n'
               b'  Missing required package Example\n'
               b'../meson.build:23:0: ERROR: Missing package\n'
               b'/usr/bin/ld: cannot find -lExample\n'
               b'file.cpp:(.text): undefined reference to missing_symbol\n'
               b'authorization bearer PRIVATE\n')
        lines = wpe.compiler_failure_lines(raw)
        self.assertIn('Source/file.cpp:4:2: error: missing symbol', lines)
        self.assertIn('  Missing required package Example', lines)
        self.assertIn('../meson.build:23:0: ERROR: Missing package', lines)
        self.assertIn('/usr/bin/ld: cannot find -lExample', lines)
        self.assertIn('file.cpp:(.text): undefined reference to missing_symbol', lines)
        self.assertNotIn('PRIVATE', '\n'.join(lines))

    def setUp(self):
        self.args = argparse.Namespace(architecture='x86_64', epoch='0',
            cache='code.podesta.ai/qos/tilo-development-wpe-cache-x86_64',
            repo=Path('/private/source tree'), assets=Path('/private/source tree/assets'),
            base='code.podesta.ai/qos/fedora-bootc@sha256:' + '1' * 64,
            rpms='code.podesta.ai/qos/rpms@sha256:' + '2' * 64,
            kernel='code.podesta.ai/qos/qos-kernel@sha256:' + '3' * 64,
            wpe='code.podesta.ai/qos/tilo-wpe-buildroot@sha256:' + '4' * 64)

    def test_only_dependency_target_can_be_cached(self):
        argv = wpe.build_command(self.args, Path('/private/image.id'))
        self.assertEqual(argv[argv.index('--target') + 1], 'wpe-builder-1')
        self.assertEqual(argv[argv.index('--source-date-epoch') + 1], '0')
        self.assertIn('SOURCE_DATE_EPOCH=0', argv)
        self.assertIn('--layers', argv)
        self.assertIn('--network=none', argv)
        self.assertIn(f'github-assets={self.args.assets}', argv)
        self.assertEqual(argv.count(self.args.cache), 2)
        self.assertFalse(any('TRUST' in arg or 'GENERATION' in arg or 'revision' in arg for arg in argv))
        self.assertNotIn('--label', argv)
        self.assertNotIn('--secret', argv)
        self.assertNotIn('--build-arg TILO_WITH_BROWSER=0', argv)

    def test_explicit_jobs_preserve_default_and_reject_invalid(self):
        self.assertIn('WPE_BUILD_JOBS=2', wpe.build_command(self.args, Path('/iid')))
        self.args.jobs = 6
        self.assertIn('WPE_BUILD_JOBS=6', wpe.build_command(self.args, Path('/iid')))
        for value in (0, -1, 65, True, '6'):
            self.args.jobs = value
            with self.assertRaises(ValueError):
                wpe.validate(self.args)

    def test_rejects_wrong_arch_cache_mutable_input_and_invalid_epoch(self):
        for field, value in [('cache', self.args.cache + ':latest'),
                             ('cache', self.args.cache.replace('x86_64', 'aarch64')),
                             ('cache', 'code.podesta.ai/qos/tilo-buildcache'),
                             ('epoch', '01'), ('epoch', '-1'), ('epoch', '253402300800'),
                             ('epoch', '$(id)'), ('wpe', self.args.wpe.split('@')[0] + ':latest'),
                             ('base', 'fedora:latest')]:
            old = getattr(self.args, field)
            setattr(self.args, field, value)
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                wpe.validate(self.args)
            setattr(self.args, field, old)

    def test_accepts_both_native_identities_but_rejects_mismatch(self):
        identity = 'sha256:' + 'a' * 64
        for architecture, oci in wpe.ARCHITECTURES.items():
            image = {'Id': identity, 'Os': 'linux', 'Architecture': oci}
            self.assertEqual(wpe.image_identity(json.dumps([image]), identity, architecture), identity)
            for field, value in [('Id', 'b' * 64), ('Os', 'windows'), ('Architecture', 'unknown')]:
                invalid = image | {field: value}
                with self.assertRaises(ValueError):
                    wpe.image_identity(json.dumps([invalid]), identity, architecture)


if __name__ == '__main__':
    unittest.main()

import argparse
import importlib.util
import json
import io
import subprocess
from unittest.mock import patch
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location('wpe_stage',
    Path(__file__).resolve().parents[1] / 'dev/prepare-wpe-stage.py')
wpe = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(wpe)


def restart_fixture(test):
    """Give the next subTest phase of a merged test the fresh fixture it had
    as its own test: tear down, run cleanups, reset instance state, set up."""
    test.tearDown()
    test.doCleanups()
    fresh = type(test)(test._testMethodName).__dict__
    keep = {key: test.__dict__[key] for key in ("_outcome", "_subtest") if key in test.__dict__}
    test.__dict__.clear()
    test.__dict__.update(fresh, **keep)
    test.setUp()


class WpeStageTests(unittest.TestCase):
    def test_failure_diagnostics(self):
        with self.subTest(phase="failure_diagnostics_keep_compiler_context_without_registry_auth"):
            raw = (b'error authenticating registry token PRIVATE\n'
                   b'Source/file.cpp:4:2: error: missing symbol\n'
                   b'CMake Error at CMakeLists.txt:5 (find_package):\n'
                   b'  Missing required package Example\n'
                   b'../meson.build:23:0: ERROR: Missing package\n'
                   b'/usr/bin/ld: cannot find -lExample\n'
                   b'file.cpp:(.text): undefined reference to missing_symbol\n'
                   b'authorization bearer PRIVATE\n')
            lines = wpe.compiler_failure_lines(io.BytesIO(raw))
            self.assertIn('Source/file.cpp:4:2: error: missing symbol', lines)
            self.assertIn('  Missing required package Example', lines)
            self.assertIn('../meson.build:23:0: ERROR: Missing package', lines)
            self.assertIn('/usr/bin/ld: cannot find -lExample', lines)
            self.assertIn('file.cpp:(.text): undefined reference to missing_symbol', lines)
            self.assertNotIn('PRIVATE', '\n'.join(lines))
        restart_fixture(self)
        with self.subTest(phase="streamed_diagnostics_bound_reads_and_keep_driver_fatal_errors"):
            class BoundedStream(io.BytesIO):
                def read(self, *args):
                    raise AssertionError('whole-file read forbidden')
                def readline(self, limit=-1):
                    self.assert_limit(limit)
                    return super().readline(limit)
                @staticmethod
                def assert_limit(limit):
                    if not 0 < limit <= 4097:
                        raise AssertionError('unbounded line read')
            driver = b'c++: fatal error: Killed signal terminated program cc1plus\n'
            raw = BoundedStream(b'x' * 100000 + b'\n' + driver * 100)
            lines = wpe.compiler_failure_lines(raw)
            self.assertEqual(len(lines), 80)
            self.assertTrue(all(line == driver.decode().strip() for line in lines))
        restart_fixture(self)
        with self.subTest(phase="prepare_spools_private_stderr_and_removes_it_on_failure"):
            paths = []
            def failed_build(argv, **kwargs):
                stream = kwargs['stderr']
                self.assertNotEqual(stream, subprocess.PIPE)
                paths.append(Path(stream.name))
                stream.write(b'auth token PRIVATE\ncc: fatal error: compiler stopped\n')
                raise subprocess.CalledProcessError(1, argv)
            public = io.StringIO()
            with patch.object(wpe.platform, 'system', return_value='Linux'), \
                 patch.object(wpe.platform, 'machine', return_value='x86_64'), \
                 patch.object(wpe.os, 'getuid', return_value=0), \
                 patch.dict(wpe.os.environ, {}, clear=True), \
                 patch.object(wpe.subprocess, 'run', side_effect=failed_build), \
                 patch.object(wpe.sys, 'stderr', public):
                with self.assertRaises(subprocess.CalledProcessError):
                    wpe.prepare(self.args)
            self.assertIn('cc: fatal error: compiler stopped', public.getvalue())
            self.assertNotIn('PRIVATE', public.getvalue())
            self.assertEqual(len(paths), 1)
            self.assertFalse(paths[0].exists())

    def setUp(self):
        self.args = argparse.Namespace(architecture='x86_64', epoch='0',
            cache='registry.tilo.so/tilo/tilo-development-wpe-cache-x86_64',
            repo=Path('/private/source tree'), assets=Path('/private/source tree/assets'),
            wpe='registry.tilo.so/tilo/tilo-wpe-buildroot@sha256:' + '4' * 64)

    def test_stage_inputs(self):
        with self.subTest(phase="only_dependency_target_can_be_cached"):
            argv = wpe.build_command(self.args, Path('/private/image.id'))
            self.assertEqual(argv[argv.index('--target') + 1], 'wpe-builder-1')
            self.assertEqual(argv[argv.index('--source-date-epoch') + 1], '0')
            self.assertIn('TILO_WPE_SOURCE_DATE_EPOCH=0', argv)
            self.assertIn('--layers', argv)
            self.assertIn('--network=none', argv)
            self.assertIn(f'github-assets={self.args.assets}', argv)
            self.assertEqual(argv.count(self.args.cache), 2)
            self.assertFalse(any('TRUST' in arg or 'GENERATION' in arg or 'revision' in arg for arg in argv))
            self.assertNotIn('--label', argv)
            self.assertNotIn('--secret', argv)
            self.assertTrue(any(arg.endswith('os/Containerfile.wpe') for arg in argv))
            self.assertFalse(any(arg.endswith('os/Containerfile') for arg in argv))
            self.assertNotIn('--build-arg TILO_WITH_BROWSER=0', argv)
        restart_fixture(self)
        with self.subTest(phase="isolated_recipe_omits_os_inputs_and_allows_uncached_build"):
            argv = wpe.build_command(self.args, Path('/private/image.id'))
            self.assertFalse(any(arg.startswith('BASE_REF=') or arg.startswith('RPMS_REF=')
                                 or arg.startswith('KERNEL_RPMS_REF=') for arg in argv))
            self.args.cache = None
            argv = wpe.build_command(self.args, Path('/private/image.id'))
            self.assertNotIn('--cache-from', argv)
            self.assertNotIn('--cache-to', argv)
        restart_fixture(self)
        with self.subTest(phase="explicit_jobs_preserve_default_and_reject_invalid"):
            self.assertIn('WPE_BUILD_JOBS=2', wpe.build_command(self.args, Path('/iid')))
            self.args.jobs = 6
            self.assertIn('WPE_BUILD_JOBS=6', wpe.build_command(self.args, Path('/iid')))
            for value in (0, -1, 65, True, '6'):
                self.args.jobs = value
                with self.assertRaises(ValueError):
                    wpe.validate(self.args)
        restart_fixture(self)
        with self.subTest(phase="rejects_wrong_arch_cache_mutable_input_and_invalid_epoch"):
            for field, value in [('cache', self.args.cache + ':latest'),
                                 ('cache', self.args.cache.replace('x86_64', 'aarch64')),
                                 ('cache', 'registry.tilo.so/tilo/tilo-buildcache'),
                                 ('epoch', '01'), ('epoch', '-1'), ('epoch', '253402300800'),
                                 ('epoch', '$(id)'), ('wpe', self.args.wpe.split('@')[0] + ':latest'),
                                 ('wpe', 'registry.tilo.so/tilo/fedora-bootc@sha256:' + '1' * 64)]:
                old = getattr(self.args, field)
                setattr(self.args, field, value)
                with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                    wpe.validate(self.args)
                setattr(self.args, field, old)
        restart_fixture(self)
        with self.subTest(phase="accepts_both_native_identities_but_rejects_mismatch"):
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

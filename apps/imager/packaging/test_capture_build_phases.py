"""Production packaging phase selection; Cargo is observed, never compiled here."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[3]


class CaptureBuildPhases(unittest.TestCase):
    def _assert_build_only(self, with_writer):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            log = root / 'cargo.jsonl'
            cargo = root / 'cargo'
            cargo.write_text('#!/usr/bin/python3\nimport json,os,sys\n'
                             'with open(os.environ["CAPTURE_PHASE_TEST_LOG"],"a") as out:\n'
                             ' out.write(json.dumps(sys.argv[1:])+"\\n")\n')
            cargo.chmod(0o700)
            result = subprocess.run(['bash', str(REPO / 'apps/imager/packaging/package-capture-service.sh'),
                                     *(['--with-writer'] if with_writer else []),
                                     '--build-only', str(root / 'sidecar'), str(root / 'pins.json')],
                                    cwd=REPO, env={**os.environ, 'CARGO': str(cargo),
                                                  'CAPTURE_PHASE_TEST_LOG': str(log)},
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual(len(calls), 2)
            thin, tools = calls
            self.assertEqual(thin[:4], ['build', '--locked', '--profile', 'thin-app'])
            features = thin[thin.index('--features') + 1].split(',')
            self.assertIn('imager/thin', features)
            self.assertIn('imager/capture-worker', features)
            self.assertIn('browser/thin', features)
            self.assertIn('chat/thin', features)
            bins = [thin[i + 1] for i, value in enumerate(thin) if value == '--bin']
            self.assertEqual(bins.count('tilo-runtime-account-capability'), 1)
            self.assertEqual(bins.count('tilo-home-export'), 1)
            self.assertNotIn('imager', bins)
            self.assertEqual(bins.count('word-processor'), int(with_writer))
            self.assertEqual(bins.count('file-explorer'), int(with_writer))
            allowed = {'tilo-home-export', 'tilo-runtime-account-capability'}
            if with_writer:
                allowed.update(('file-explorer', 'word-processor'))
            self.assertTrue(all(binary in allowed
                                or binary.endswith('-check') for binary in bins))
            self.assertIn('imager/capture-worker-applications', features)
            self.assertIn('build.rustc-workspace-wrapper=', next(value for value in thin if 'rustc-workspace-wrapper=' in value))
            self.assertIn('tilo-launcher', tools)
            self.assertIn('tilo-runtime-bundle', tools)
            self.assertIn('tilo-app-package', tools)
            self.assertFalse((root / 'sidecar').exists())
            self.assertFalse((root / 'pins.json').exists())
            self.assertIn('native builder', result.stdout)

    def test_build_only_keeps_unified_graph_without_native_execution(self):
        self._assert_build_only(False)

    def test_writer_uses_the_same_actual_unified_worker_sdk_graph(self):
        self._assert_build_only(True)

    def test_assembly_requires_explicit_operator_authority_before_native_execution(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            env = dict(os.environ, TILO_VERSION="0.1.0")
            env.pop("TILO_CAPTURE_RUNTIME_IDENTITY", None)
            env.pop("TILO_CAPTURE_SERVICE_IDENTITY", None)
            result = subprocess.run(
                ['bash', str(REPO / 'apps/imager/packaging/package-capture-service.sh'),
                 '--assemble-only', str(root / 'sidecar'), str(root / 'pins.json')],
                cwd=REPO, env=env, capture_output=True, text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('explicit capture runtime and service signing identities', result.stderr)
            self.assertFalse((root / 'sidecar').exists())
            self.assertFalse((root / 'pins.json').exists())

    def test_missing_gpgme_is_named_before_native_compilation(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            pkgconfig = root / 'pkg-config'
            pkgconfig.write_text('#!/bin/sh\nexit 1\n')
            pkgconfig.chmod(0o700)
            compiler = root / 'capture-cc'
            marker = root / 'compiler-invoked'
            compiler.write_text('#!/bin/sh\ntouch "$CAPTURE_PREFLIGHT_MARKER"\nexit 1\n')
            compiler.chmod(0o700)
            env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ['PATH'],
                       CC=str(compiler), CAPTURE_PREFLIGHT_MARKER=str(marker))
            result = subprocess.run(
                ['bash', str(REPO / 'apps/imager/packaging/capture-native-preflight.sh')],
                cwd=REPO, env=env, capture_output=True, text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('GPGME development files', result.stderr)
            self.assertIn('gpgme-devel', result.stderr)
            self.assertIn('libgpgme-dev', result.stderr)
            self.assertFalse(marker.exists())

    def test_unknown_thin_phase_refuses_before_build(self):
        result = subprocess.run(['bash', str(REPO / 'os/scripts/build-thin-apps.sh')],
                                env={**os.environ, 'BUILD_PHASE': 'pretend'}, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('invalid thin build phase', result.stderr)


if __name__ == '__main__':
    unittest.main()

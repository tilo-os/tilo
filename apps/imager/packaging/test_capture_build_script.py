"""Exercise the production build script in isolation with rustc, never Cargo."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent


class CaptureBuildScript(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.scratch = tempfile.TemporaryDirectory()
        cls.root = Path(cls.scratch.name)
        cls.binary = cls.root / 'build-script'
        subprocess.run(['rustc', '--edition=2024', str(HERE.parent / 'build.rs'),
                        '-o', str(cls.binary)], check=True, capture_output=True)

    @classmethod
    def tearDownClass(cls):
        cls.scratch.cleanup()

    def execute(self, default, settings):
        with tempfile.TemporaryDirectory(dir=self.root) as scratch:
            root = Path(scratch)
            (root / 'capture-service.json').write_text(default)
            out = root / 'out'
            out.mkdir()
            env = {key: value for key, value in os.environ.items()
                   if not key.startswith(('TILO_CAPTURE_', 'CARGO_FEATURE_CAPTURE_WORKER'))
                   and key != 'TILO_REQUIRE_CAPTURE'}
            env.update(CARGO_MANIFEST_DIR=str(root), OUT_DIR=str(out),
                       CARGO_CFG_TARGET_OS='linux', CARGO_CFG_TARGET_ARCH='x86_64', **settings)
            result = subprocess.run([str(self.binary)], env=env, capture_output=True, text=True)
            embedded = out / 'capture-service.json'
            return result, embedded.read_text() if embedded.exists() else None

    def test_release_cannot_embed_default_null(self):
        result, embedded = self.execute('null\n', {'TILO_REQUIRE_CAPTURE': '1'})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('release migration requires signed capture pins', result.stderr)
        self.assertIsNone(embedded)

    def test_nonnull_default_cannot_bypass_exact_artifact_admission(self):
        result, embedded = self.execute('{"schema":1}\n', {})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('must not bypass signed sidecar admission', result.stderr)
        self.assertIsNone(embedded)

    def test_half_selected_migration_refuses_without_output(self):
        result, embedded = self.execute('null\n', {'TILO_CAPTURE_PINS': '/unadmitted'})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('must both be selected', result.stderr)
        self.assertIsNone(embedded)

    def test_worker_bootstrap_is_not_a_host_readiness_claim(self):
        result, embedded = self.execute('null\n', {'CARGO_FEATURE_CAPTURE_WORKER': '1'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(embedded, 'null\n')

    def test_release_host_cannot_share_null_worker_bootstrap_graph(self):
        result, embedded = self.execute('null\n', {'TILO_REQUIRE_CAPTURE': '1',
                                                  'CARGO_FEATURE_CAPTURE_WORKER': '1'})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('built separately from the capture worker bootstrap graph', result.stderr)
        self.assertIsNone(embedded)


if __name__ == '__main__':
    unittest.main()

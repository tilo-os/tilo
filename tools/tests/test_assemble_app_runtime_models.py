import importlib.util
from pathlib import Path
import tempfile
import types
import unittest

SCRIPT = Path(__file__).parents[2] / 'os/dev/assemble-app-runtime.py'
spec = importlib.util.spec_from_file_location('assemble_app_runtime', SCRIPT)
assembly = importlib.util.module_from_spec(spec)
spec.loader.exec_module(assembly)


class ModelTests(unittest.TestCase):
    def test_a_dev_assembly_builds_the_models_its_apps_declare(self):
        with tempfile.TemporaryDirectory() as d:
            repo, target = Path(d) / 'repo', Path(d) / 'target'
            (repo / 'apps/poker-model-wasm').mkdir(parents=True)
            (repo / 'apps/poker-model-wasm/Cargo.toml').write_text('')
            (repo / 'apps/terminal').mkdir(parents=True)
            poker, terminal = {'dir': 'poker'}, {'dir': 'terminal'}
            self.assertEqual(assembly.model_entries(repo, [poker, terminal]), [poker])

            calls = []

            def fake_run(argv, *, env=None, capture=False):
                calls.append((argv, env))
                runtime = target / 'app-models/app-model-runtime'
                runtime.mkdir(parents=True)
                (runtime / 'runtime.json').write_text('{}')

            args = types.SimpleNamespace(repo=repo, target_dir=target)
            original = assembly.run
            assembly.run = fake_run
            try:
                self.assertIsNone(assembly.prepare_models(args, [terminal]))
                self.assertEqual(calls, [])
                runtime = assembly.prepare_models(args, [poker, terminal])
            finally:
                assembly.run = original
            self.assertEqual(runtime, target.resolve() / 'app-models/app-model-runtime')
            (argv, env), = calls
            self.assertEqual(argv[-1], str(repo.resolve() / 'tools/ci/app-models.sh'))
            self.assertEqual(env['CARGO_TARGET_DIR'], str(target.resolve()))


if __name__ == '__main__':
    unittest.main()

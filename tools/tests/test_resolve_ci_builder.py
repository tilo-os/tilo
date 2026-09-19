import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('resolver', ROOT / 'tools/ci/resolve_builder.py')
resolver = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(resolver)


class BuilderResolutionTests(unittest.TestCase):
    def test_direct_execution_does_not_shadow_python_standard_library(self):
        env = dict(os.environ)
        env.pop('REGISTRY_TOKEN', None)
        result = subprocess.run(
            [sys.executable, str(ROOT / 'tools/ci/resolve_builder.py')],
            cwd=ROOT,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr, '::error::CI builder resolution failed (ValueError)\n')
        self.assertNotIn('Traceback', result.stderr)

    def test_actual_forgejo_manifest_record_contract(self):
        # Fields retained from authenticated package-file API read on 2026-09-19.
        row = {'id': 229897, 'Size': 493, 'name': 'manifest.json',
               'sha256': '027e43c051f1f34ce9ac1539172af0e711958243115e2efd08901ffa58729c5a'}
        seen = []
        def fetch(token):
            seen.append(token)
            return [row]
        self.assertEqual(resolver.resolve(token='fixture', fetch=fetch), resolver.IMAGE + '@sha256:' + row['sha256'])
        self.assertEqual(seen, ['fixture'])
        row['sha256'] = 'a' * 64
        self.assertEqual(resolver.resolve(token='fixture', fetch=fetch), resolver.IMAGE + '@sha256:' + 'a' * 64,
                         'a newly published builder must not be replaced by the cached seed digest')

    def test_ambiguous_or_invalid_api_response_fails_closed(self):
        valid = {'Size': 493, 'name': 'manifest.json', 'sha256': 'a' * 64}
        for rows in (None, {}, [], [valid, valid], [None], [{**valid, 'name': 'other.json'}],
                     [{**valid, 'sha256': 'bad'}], [{**valid, 'Size': 0}]):
            with self.subTest(rows=rows), self.assertRaises(ValueError):
                resolver.resolve(token='fixture', fetch=lambda token: rows)

    def test_manual_override_never_calls_latest_api(self):
        image = resolver.IMAGE + '@sha256:' + 'b' * 64
        def unexpected_fetch(token):
            self.fail('manual immutable override queried mutable latest')
        self.assertEqual(resolver.resolve(image, fetch=unexpected_fetch), image)

    def test_workflow_routes_both_consumers_through_resolved_output(self):
        workflow = (ROOT / '.forgejo/workflows/check.yml').read_text()
        self.assertIn('REGISTRY_TOKEN: ${{ secrets.REGISTRY_TOKEN }}', workflow)
        self.assertIn('run: python3 tools/ci/resolve_builder.py', workflow)
        self.assertEqual(workflow.count('image: ${{ needs.builder.outputs.image }}'), 2)
        self.assertNotIn('image=code.sandbox.example/qos/tilo-builder:latest', workflow)


if __name__ == '__main__':
    unittest.main()

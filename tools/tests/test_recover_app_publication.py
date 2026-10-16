"""Recovery rejects changed provenance/archives without registry writes."""
import copy
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[2]
sys.path[:0] = [str(ROOT / 'tools'), str(ROOT / 'tools/apps')]
SPEC = importlib.util.spec_from_file_location('recover', ROOT / 'tools/apps/recover_app_publication.py')
r = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(r)


class RecoveryTests(unittest.TestCase):
    def setUp(self):
        self.manifest = json.loads((ROOT / 'tools/apps/publication-recovery-97894.json').read_text())
        self.run = {'id': self.manifest['run_id'], 'commit_sha': self.manifest['source'],
                    'workflow_id': 'release-apps.yml', 'event': 'push', 'status': 'failure', 'prettyref': 'main'}
        self.jobs = [{'id': identity, 'status': 'success', 'attempt': 1}
                     for identity in self.manifest['successful_jobs']]
        self.jobs.append({'id': self.manifest['publish_job'], 'status': 'failure', 'attempt': 1, 'name': 'publish'})

    def test_only_the_original_protected_source_and_successful_builds_qualify(self):
        r.validate_run(self.run, self.jobs, self.manifest)
        for field, value in [('commit_sha', '0' * 40), ('event', 'pull_request'),
                             ('prettyref', 'feature'), ('workflow_id', 'check.yml'), ('id', 1)]:
            with self.subTest(field=field), self.assertRaises(ValueError):
                r.validate_run(dict(self.run, **{field: value}), self.jobs, self.manifest)
        for field, value in [('status', 'failure'), ('attempt', 2)]:
            jobs = copy.deepcopy(self.jobs)
            jobs[0][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                r.validate_run(self.run, jobs, self.manifest)
        jobs = copy.deepcopy(self.jobs)
        jobs[-1]['attempt'] = 2
        with self.assertRaises(ValueError):
            r.validate_run(self.run, jobs, self.manifest)

    def test_absent_expired_changed_or_duplicate_artifacts_fail(self):
        items = [{'id': a['id'], 'name': a['name'], 'size_in_bytes': a['bytes'],
                  'expired': False, 'run_id': self.manifest['run_id']} for a in self.manifest['artifacts']]
        r.validate_artifacts(items, self.manifest)
        for field, value in [('expired', True), ('name', 'other'), ('size_in_bytes', 1), ('run_id', 1)]:
            changed = copy.deepcopy(items)
            changed[0][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                r.validate_artifacts(changed, self.manifest)
        for changed in [items[:1], [items[0], items[0]]]:
            with self.assertRaises(ValueError):
                r.validate_artifacts(changed, self.manifest)

    def test_zip_traversal_duplicates_links_and_resource_bombs_fail_before_extraction(self):
        for names in [['../escape'], ['/absolute'], ['index.json', 'index.json']]:
            with self.subTest(names=names):
                data = io.BytesIO()
                with zipfile.ZipFile(data, 'w') as z:
                    for name in names:
                        z.writestr(name, b'x')
                data.seek(0)
                with zipfile.ZipFile(data) as z, self.assertRaises(ValueError):
                    r.inventory(z)
        data = io.BytesIO()
        with zipfile.ZipFile(data, 'w') as z:
            entry = zipfile.ZipInfo('runtime/link')
            entry.external_attr = 0o120777 << 16
            z.writestr(entry, '../escape')
        data.seek(0)
        with zipfile.ZipFile(data) as z, self.assertRaises(ValueError):
            r.inventory(z)

    def test_changed_digest_or_source_never_creates_output(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = root / 'artifact.zip'
            content = {'index.json': json.dumps({'source': '0' * 40, 'architecture': 'x86_64'}).encode(),
                       'publication-receipt.json': json.dumps({'source_sha': '0' * 40, 'architecture': 'x86_64'}).encode()}
            with zipfile.ZipFile(archive, 'w') as z:
                for name, raw in content.items():
                    z.writestr(name, raw)
            expected = {'bytes': archive.stat().st_size, 'sha256': r.channel.sha(archive),
                        'architecture': 'x86_64', 'metadata_sha256': {n: hashlib.sha256(v).hexdigest() for n, v in content.items()}}
            for changed in [dict(expected, sha256='1' * 64), expected]:
                with self.assertRaises(ValueError):
                    r.extract(archive, changed, '2' * 40, root / 'output')
                self.assertFalse((root / 'output').exists())

    def test_existing_policy_failure_stops_before_any_package_execution(self):
        with patch.object(r.bundle, 'fetch', side_effect=ValueError('incompatible pinned verifier')), \
                patch.object(r.final, 'preflight') as execution, self.assertRaises(ValueError):
            r.verify(Path('release'), self.manifest, Path('policy'), '1' * 64, Path('tools'))
        execution.assert_not_called()

    def test_verifier_executes_only_the_digest_pinned_bundle_adapter(self):
        with tempfile.TemporaryDirectory() as temp:
            tools = Path(temp)
            adapter = tools / 'canonical_app_verifier.py'
            adapter.write_text('class CanonicalVerifier:\n'
                               ' def __init__(self, *args): self.origin = "pinned bundle"\n')
            policy = {'verifier_sha256': r.channel.sha(adapter)}
            with patch.object(r.final, 'preflight', return_value=(policy, adapter)):
                _, verifier = r.pinned_verifier(tools, Path('policy'), '1' * 64)
                self.assertEqual(verifier.origin, 'pinned bundle')
                adapter.write_text('raise AssertionError("untrusted adapter executed")\n')
                with self.assertRaisesRegex(ValueError, 'pinned adapter changed'):
                    r.pinned_verifier(tools, Path('policy'), '1' * 64)

    def test_verified_collision_is_idempotent_and_different_bytes_are_rejected(self):
        # Existing immutable publisher exercised using a fake curl executable:
        # no HTTP request or registry entry is submitted by this test.
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            fake = root / 'curl'
            fake.write_text('''#!/usr/bin/env python3
import os,sys
from pathlib import Path
args=sys.argv[1:]
if '--request' in args:
 print('409 ', end='')
else:
 Path(args[args.index('--output')+1]).write_bytes(os.environ['COLLISION_BYTES'].encode())
 print('200', end='')
''')
            fake.chmod(0o755)
            payload = root / 'package'
            payload.write_bytes(b'original signed bytes')
            manifest = root / 'uploads.tsv'
            manifest.write_text(f'{payload}\thttps://unused.invalid/package\n')
            env = dict(os.environ, PATH=str(root) + ':' + os.defpath, RUNNER_TEMP=temp,
                       TILO_PACKAGE_TOKEN='fixture', TILO_PACKAGE_UPLOAD_WORKERS='1')
            for content, expected in [('original signed bytes', 0), ('different', 1)]:
                result = subprocess.run(['bash', str(ROOT / 'tools/upload_immutable_packages.sh'), str(manifest)],
                                        env=dict(env, COLLISION_BYTES=content), capture_output=True, text=True)
                self.assertEqual(result.returncode, expected)
                self.assertIn('already published identically' if expected == 0 else 'immutable package version conflicts',
                              result.stdout if expected == 0 else result.stderr)


if __name__ == '__main__':
    unittest.main()

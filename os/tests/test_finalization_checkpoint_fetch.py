import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
import zipfile

SPEC = importlib.util.spec_from_file_location('fetch_checkpoint',
    Path(__file__).resolve().parents[1] / 'dev/fetch-finalization-checkpoint.py')
fetch = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fetch)


class CheckpointFetchTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.request = {'source_revision': 'a' * 40, 'helpers_revision': 'b' * 40,
            'architecture': 'aarch64', 'new_target': 'ghcr.io/tilo-os/tilo-development:next-aarch64',
            'new_manifest_sha256': 'c' * 64, 'publish_repository': 'ghcr.io/tilo-os/tilo-development'}
        self.request_raw = fetch.canonical(self.request)
        self.request_path = self.root / 'request.json'
        self.request_path.write_bytes(self.request_raw)
        self.request_sha = hashlib.sha256(self.request_raw).hexdigest()
        self.stage = 5
        self.files = self.receipts(self.stage)
        self.archive = self.zip(self.files)
        self.receipt_sha = hashlib.sha256(self.files['05-reseal.json']).hexdigest()

    def receipts(self, stage):
        parent, result = None, {}
        for number, name in enumerate(fetch.STAGES[:stage], 1):
            receipt = {'schema': 1, 'request_sha256': self.request_sha, 'stage': name,
                       'parent_receipt_sha256': parent, 'image_id': 'sha256:' + str(number) * 64}
            if number == stage:
                manifest = 'sha256:' + '6' * 64
                receipt.update(checkpoint_reference=self.request['publish_repository'] + '@' + manifest,
                    checkpoint_manifest_digest=manifest, checkpoint_config_digest=receipt['image_id'],
                    checkpoint_binding={'request_sha256': self.request_sha,
                        'source_revision': self.request['source_revision'],
                        'helpers_revision': self.request['helpers_revision'],
                        'architecture': self.request['architecture'], 'new_target': self.request['new_target'],
                        'new_manifest_sha256': self.request['new_manifest_sha256']},
                    checkpoint_stage=stage, producer_context_sha256='7' * 64)
            result[fetch.ALL_RECEIPTS[number - 1]] = fetch.canonical(receipt)
            parent = hashlib.sha256(fetch.canonical(receipt)).hexdigest()
        return result

    def zip(self, files):
        output = io.BytesIO()
        with zipfile.ZipFile(output, 'w', zipfile.ZIP_DEFLATED) as archive:
            for name, raw in files.items():
                archive.writestr(name, raw)
        return output.getvalue()

    def getter(self, run_status='failure', job_status='failure', archive=None):
        archive = self.archive if archive is None else archive
        values = {
            '/actions/runs/41': {'id': 41, 'workflow_id': 'development-finalize.yml', 'status': run_status},
            '/actions/runs/41/jobs': [{'name': 'finalize', 'attempt': 3, 'task_id': 99,
                                      'status': job_status}],
            '/actions/runs/41/artifacts?page=1&limit=50': [
                {'id': 73, 'name': 'finalization-41-3', 'run_id': 41, 'expired': False}],
        }
        def get(url, token, limit):
            self.assertEqual(token, 'task-token')
            path = url.removeprefix('https://forge.invalid/api/v1/repos/tilo/tilo')
            raw = archive if path == '/actions/artifacts/73/zip' else json.dumps(values[path]).encode()
            self.assertLessEqual(len(raw), limit)
            return raw
        return get

    def retrieve(self, getter=None, digest=None, output=None, current='100', stage=None):
        return fetch.retrieve('41', '3', stage or self.stage, digest or self.receipt_sha,
            self.request_path, output or self.root / 'restored', 'https://forge.invalid/api/v1',
            'tilo/tilo', 'task-token', current, getter or self.getter())

    def test_fetches_exact_failed_attempt_and_materializes_only_receipts_01_to_05(self):
        result = self.retrieve()
        self.assertEqual((result['run_id'], result['attempt'], result['artifact_id']), (41, 3, 73))
        restored = self.root / 'restored'
        self.assertEqual(tuple(sorted(path.name for path in restored.iterdir())), fetch.ALL_RECEIPTS)
        self.assertTrue(all(path.stat().st_mode & 0o777 == 0o600 for path in restored.iterdir()))

    def test_fetches_stage2_without_requiring_later_receipts(self):
        self.stage = 2
        self.files = self.receipts(self.stage)
        self.archive = self.zip(self.files)
        self.receipt_sha = hashlib.sha256(self.files['02-loader.json']).hexdigest()
        result = self.retrieve(output=self.root / 'stage2')
        self.assertEqual(result['stage'], 2)
        self.assertEqual(tuple(sorted(path.name for path in (self.root / 'stage2').iterdir())),
                         fetch.ALL_RECEIPTS[:2])

    def test_rejects_active_same_run_success_and_archive_substitution(self):
        cases = [
            ('same-run', dict(current='41')),
            ('successful-run', dict(getter=self.getter(run_status='success'))),
            ('running-job', dict(getter=self.getter(job_status='running'))),
            ('wrong-archive', dict(digest='0' * 64)),
        ]
        for label, arguments in cases:
            with self.subTest(label=label), self.assertRaises(ValueError):
                self.retrieve(output=self.root / label, **arguments)

    def test_rejects_legacy_checkpoint_wrong_request_and_unexpected_zip_member(self):
        legacy = dict(self.files)
        receipt = json.loads(legacy['05-reseal.json'])
        for key in ('checkpoint_reference', 'checkpoint_manifest_digest', 'checkpoint_config_digest',
                    'checkpoint_binding', 'producer_context_sha256'):
            receipt.pop(key)
        legacy['05-reseal.json'] = fetch.canonical(receipt)
        extra = dict(self.files) | {'secret': b'not public'}
        changed = dict(self.files)
        receipt = json.loads(changed['01-rebind.json'])
        receipt['request_sha256'] = '0' * 64
        changed['01-rebind.json'] = fetch.canonical(receipt)
        for label, files in [('legacy', legacy), ('unexpected', extra), ('request', changed)]:
            archive = self.zip(files)
            with self.subTest(label=label), self.assertRaises(ValueError):
                self.retrieve(getter=self.getter(archive=archive), digest=hashlib.sha256(archive).hexdigest(),
                              output=self.root / label)

    def test_workflow_requires_stage_and_passes_generic_restore_flag(self):
        workflow = (Path(__file__).resolve().parents[2] / '.github/workflows/development-finalize.yml').read_text()
        for name in ('restore_run_id', 'restore_run_attempt', 'restore_receipt_sha256'):
            self.assertIn(name + ':', workflow)
        self.assertIn('restore_stage:', workflow)
        self.assertIn("if: steps.validated.outputs.restore_checkpoint == 'true'", workflow)
        self.assertIn('TASK_TOKEN: ${{ github.token }}', workflow)
        self.assertIn('fetch-finalization-checkpoint.py', workflow)
        self.assertIn('restore_args=(--restore-checkpoint "$RUNNER_TEMP/checkpoint-evidence" --restore-stage "$RESTORE_STAGE")', workflow)
        self.assertIn('"${restore_args[@]}"', workflow)


if __name__ == '__main__':
    unittest.main()

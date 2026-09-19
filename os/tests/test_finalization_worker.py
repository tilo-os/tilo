#!/usr/bin/env python3
"""Cheap contract checks for the actual CI launcher; no engine or network."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tarfile
import unittest
from unittest.mock import patch

path = Path(__file__).parents[1] / 'dev/launch-finalization-worker.py'
spec = importlib.util.spec_from_file_location('worker', path)
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class WorkerContract(unittest.TestCase):
    def request(self):
        return {'schema': 1, 'source_revision': worker.SOURCE, 'helpers_revision': 'a' * 40,
                'source_dir': '/work/source', 'architecture': 'aarch64',
                'receipt': '/work/inputs/retained-rootfs.json',
                'public_bundle': '/work/inputs/original-public',
                'new_public_bundle': '/work/inputs/new-public',
                'signer': '/var/lib/tilo-signing-bridge/remote-development-signer.py',
                'receipt_sha256': 'b' * 64, 'manifest_sha256': 'c' * 64,
                'new_manifest_sha256': 'd' * 64, 'new_owner': 'e' * 64,
                'target': 'code.sandbox.example/qos/tilo-development:old-aarch64',
                'new_target': 'code.sandbox.example/qos/tilo-development:new-aarch64',
                'publish_repository': 'code.sandbox.example/qos/tilo-development',
                'generation': 1, 'source_date_epoch': 1789690573, 'systemd_version': '259.1',
                'minimum_free_bytes': 8 * 1024**3, 'stage_timeout_seconds': 1800,
                **{key: 'registry.invalid/input@sha256:' + 'f' * 64
                   for key in ('base_ref', 'rpms_ref', 'kernel_ref', 'wpe_ref')}}

    def test_incomplete_driver_request_fails_before_worker_creation(self):
        request = self.request()
        del request['kernel_ref']
        with self.assertRaisesRegex(ValueError, 'fields differ'):
            self.check(request)

    def test_misplaced_native_suffix_fails_at_worker_admission(self):
        for field in ('target', 'new_target'):
            request = self.request()
            request[field] = 'code.sandbox.example/qos/tilo-development:installed-ca33fae89a24-aarch64-9787'
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'matching native architecture'):
                self.check(request)

    def check(self, request, helpers='a' * 40):
        raw = json.dumps(request)
        return worker.request_bytes(raw, hashlib.sha256(raw.encode()).hexdigest(), helpers)

    def test_exact_public_contract(self):
        self.assertEqual(json.loads(self.check(self.request())), self.request())

    def test_reviewed_assembly_source_and_epoch_bound_together(self):
        request = self.request()
        request.update(source_revision='8fff1d6001b64a80bc730e87d3ffc529d2d9e70a', source_date_epoch=1789733507)
        self.assertEqual(json.loads(self.check(request)), request)
        request['source_date_epoch'] = 1789690573
        with self.assertRaisesRegex(ValueError, 'source/epoch'):
            self.check(request)
        request.update(source_revision='f' * 40, source_date_epoch=1789733507)
        with self.assertRaisesRegex(ValueError, 'source/epoch'):
            self.check(request)

    def test_reviewed_dns_source_preserves_distinct_epoch(self):
        request = self.request()
        request.update(source_revision='36c58be61ef1b25f24ae130c5ab4dde5d2350d45', source_date_epoch=1789748444)
        self.assertEqual(json.loads(self.check(request)), request)
        request['source_date_epoch'] = 1789733507
        with self.assertRaisesRegex(ValueError, 'source/epoch'):
            self.check(request)

    def test_reviewed_runtime_policy_source_requires_its_epoch(self):
        request = self.request()
        request.update(source_revision='b4f01a7d5437d015950c9b79be37e673b09b829c', source_date_epoch=1789749860)
        self.assertEqual(json.loads(self.check(request)), request)
        request['source_date_epoch'] = 1789748444
        with self.assertRaisesRegex(ValueError, 'source/epoch'):
            self.check(request)

    def test_reviewed_optional_airplay_source_requires_its_epoch(self):
        request = self.request()
        request.update(source_revision='344257f56275919abc63181d8954e838f9b5303e', source_date_epoch=1789753377)
        self.assertEqual(json.loads(self.check(request)), request)
        request['source_date_epoch'] = 1789749860
        with self.assertRaisesRegex(ValueError, 'source/epoch'):
            self.check(request)

    def test_reviewed_ssh_runtime_source_requires_its_epoch(self):
        request = self.request()
        request.update(source_revision='a89fe484328ff4206c35f842c07c3824b9df18ef', source_date_epoch=1789761500)
        self.assertEqual(json.loads(self.check(request)), request)
        request['source_date_epoch'] = 1789753377
        with self.assertRaisesRegex(ValueError, 'source/epoch'):
            self.check(request)

    def test_reviewed_late_trust_source_requires_its_epoch(self):
        request = self.request()
        request.update(source_revision='317eb82f7178b14623350f933088093cd3cdfd23', source_date_epoch=1789765461)
        self.assertEqual(json.loads(self.check(request)), request)
        for wrong_epoch in (1789761500, 1789753377):
            request['source_date_epoch'] = wrong_epoch
            with self.subTest(epoch=wrong_epoch), self.assertRaisesRegex(ValueError, 'source/epoch'):
                self.check(request)

    def test_reviewed_dracut_source_requires_its_epoch(self):
        request = self.request()
        request.update(source_revision='ca33fae89a24415584518a0dbbe4db0535935931', source_date_epoch=1789781336)
        self.assertEqual(json.loads(self.check(request)), request)
        request['source_date_epoch'] = 1789765461
        with self.assertRaisesRegex(ValueError, 'source/epoch'):
            self.check(request)

    def test_driver_evidence_is_owner_only_and_varlib_traversable(self):
        self.assertEqual(worker.DIRECTORY_MODES['/evidence'], 0o700)
        self.assertEqual(worker.DIRECTORY_MODES['/var/lib'], 0o755)
        self.assertEqual(worker.DIRECTORY_MODES['/var/lib/containers'], 0o755)

    def test_wrong_revision_or_path_rejected(self):
        for key, value in [('source_revision', 'b' * 40), ('helpers_revision', 'b' * 40),
                           ('receipt', '/etc/passwd'), ('architecture', 'x86_64'),
                           ('signer', '/work/untrusted.py')]:
            request = self.request()
            request[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.check(request)

    def test_duplicates_and_unbounded_inputs_rejected(self):
        for raw in ('{"x":1,"x":2}', 'x' * 65537):
            with self.assertRaises(ValueError):
                worker.request_bytes(raw, hashlib.sha256(raw.encode()).hexdigest(), 'a' * 40)
        with self.assertRaises(ValueError):
            worker.request_bytes(json.dumps(self.request()), '0' * 64, 'a' * 40)
        with self.assertRaises(ValueError):
            self.check(self.request(), 'main')

    def test_actual_create_argv_uses_only_owned_guest_volumes(self):
        argv = worker.create_argv('tilo-finalization-123-1')
        mounts = [argv[i + 1] for i, value in enumerate(argv) if value == '--mount']
        self.assertEqual(len(mounts), 4)
        self.assertTrue(all(v.startswith('type=volume,src=tilo-finalization-123-1-') for v in mounts))
        self.assertIn('type=volume,src=tilo-finalization-123-1-scratch,dst=/var/lib', mounts)
        self.assertEqual(argv[-3:], [worker.TOOLS, '/work/controller/os/dev/launch-finalization-worker.py', '--inside'])
        self.assertFalse(any('socket' in v or 'type=bind' in v or '--device' in v for v in argv))
        self.assertNotIn('compile-development-os.py', ' '.join(argv))

    def test_evidence_has_no_bulk_or_private_exports(self):
        self.assertIn('07-verified.json', worker.EVIDENCE)
        self.assertIn('signing-pending.json', worker.EVIDENCE)
        self.assertTrue(all('/' not in name and name.endswith('.json') for name in worker.EVIDENCE))
        self.assertFalse(any(word in name for name in worker.EVIDENCE for word in ('key', 'auth', 'log')))

    def archive(self, kind=tarfile.REGTYPE, size=2, count=1):
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode='w') as archive:
            for i in range(count):
                info = tarfile.TarInfo('receipt' + str(i))
                info.type = kind
                info.size = size
                archive.addfile(info, io.BytesIO(b'{}' + b' ' * max(0, size - 2)))
        return buffer.getvalue()

    def test_receipt_rejects_links_multiple_members_and_oversize(self):
        self.assertEqual(worker.receipt_from_tar(self.archive()), {})
        for raw in (self.archive(kind=tarfile.SYMTYPE), self.archive(count=2),
                    self.archive(size=1024 * 1024 + 1)):
            with self.assertRaises(ValueError):
                worker.receipt_from_tar(raw)

    def test_cleanup_attempts_each_owned_resource_after_timeout(self):
        with patch.object(worker.subprocess, 'run', side_effect=[
                subprocess.TimeoutExpired('docker', 30),
                subprocess.CompletedProcess([], 1), subprocess.CompletedProcess([], 0)]) as run:
            failures = worker.cleanup_owned('a' * 64, ['owned-work', 'owned-storage'])
        self.assertEqual(failures, ['a' * 64, 'owned-work'])
        self.assertEqual([call.args[0] for call in run.call_args_list], [
            ['docker', 'rm', '--force', 'a' * 64], ['docker', 'volume', 'rm', 'owned-work'],
            ['docker', 'volume', 'rm', 'owned-storage']])

    def test_success_requires_matching_exported_publication_receipts(self):
        request = {'publish_repository': 'code.sandbox.example/qos/tilo-development',
                   'new_target': 'code.sandbox.example/qos/tilo-development:next'}
        binding = hashlib.sha256(json.dumps(request, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
        published = {'schema': 1, 'request_sha256': binding, 'stage': 'published',
                     'image_id': 'sha256:' + 'a' * 64, 'config_digest': 'sha256:' + 'a' * 64,
                     'manifest_digest': 'sha256:' + 'b' * 64,
                     'published_reference': request['publish_repository'] + '@sha256:' + 'b' * 64,
                     'target_update_ref': request['new_target']}
        result = {**published, 'publication_required': False}
        complete = {'result.json': result, '08-published.json': published}
        worker.require_published_exports(complete, request)
        for incomplete in ({}, {'result.json': result}, {'08-published.json': published},
                           {**complete, 'result.json': []},
                           {**complete, 'result.json': {**result, 'publication_required': True}},
                           {**complete, '08-published.json': {**published, 'request_sha256': '0' * 64}},
                           {**complete, 'result.json': {**result, 'image_id': 'sha256:' + 'c' * 64}}):
            with self.assertRaises(ValueError):
                worker.require_published_exports(incomplete, request)


if __name__ == '__main__':
    unittest.main()

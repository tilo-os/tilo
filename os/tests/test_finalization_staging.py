import base64
import copy
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
from test_finalization_driver import request
SPEC = importlib.util.spec_from_file_location('staging', Path(__file__).resolve().parents[1] / 'dev/stage-finalization-worker.py')
s = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(s)
# The worker accepts only root-owned paths no one else may write: root's home,
# or /var/lib where that is read-only (a sandbox with a writable /var only).
STAGING_PARENT = '/root' if os.access('/root', os.W_OK) else '/var/lib'


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


class StagingTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir=STAGING_PARENT, prefix='tilo-staging-test-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.request = request()
        self.request.update(source_dir='/work/source', receipt='/work/inputs/retained-rootfs.json',
            public_bundle='/work/inputs/original-public', new_public_bundle='/work/inputs/new-public',
            signer=str(s.BRIDGE / 'remote-development-signer.py'))
        public = {n: ('fixture public ' + n).encode() for n in s.trust.FILES}
        public['manifest.json'] = s.spool.canonical({'schema': 1, 'purpose': 'tilo-development',
            'sha256': {n: s.spool.digest(v) for n, v in public.items()}})
        self.request['manifest_sha256'] = self.request['new_manifest_sha256'] = s.spool.digest(public['manifest.json'])
        receipt = b'{"fixture":true}'
        self.request['receipt_sha256'] = s.spool.digest(receipt)
        self.raw = s.spool.canonical(self.request)
        self.context = {'schema': 1, 'job_id': 'fixture-job', 'pod_uid': 'fixture-pod', 'container_id': '1' * 64,
            'image_id': 'sha256:' + '2' * 64, 'revision': self.request['source_revision'], 'architecture': 'aarch64',
            'manifest_sha256': self.request['new_manifest_sha256'], 'target': self.request['new_target']}
        files = {'retained-rootfs.json': receipt}
        files.update({f'{d}/{n}': v for d in ('original-public', 'new-public') for n, v in public.items()})
        self.packet = {'schema': 1, 'request_sha256': s.spool.digest(self.raw), 'context': self.context,
            'files': {n: base64.b64encode(v).decode() for n, v in files.items()}}
        # Crypto and retained-receipt contracts are tested by their existing
        # suites; these fixtures isolate the staging/ready protocol itself.
        self.addCleanup(patch.stopall)
        patch.object(s.trust, 'identities', return_value={'registry-owner.pub': self.request['new_owner']}).start()
        patch.object(s.importer, 'retained_identity', return_value=('reference', 'image')).start()
        self.work = self.root / 'work'
        self.work.mkdir(mode=0o755)
        (self.work / 'finalization-request.json').write_bytes(self.raw)
        helpers = self.work / 'helpers'
        (helpers / 'os/dev').mkdir(parents=True)
        (helpers / '.finalization-helpers.json').write_text(json.dumps({'revision': self.request['helpers_revision']}))
        (self.work / 'source').mkdir()
        (self.work / 'source/.finalization-source.json').write_text(json.dumps({'revision': self.request['source_revision']}))
        for name in s.CODE:
            (helpers / 'os/dev' / name).write_text('# fixture public code\n')
        self.bridge = self.root / 'bridge'

    def test_receive_and_release(self):
        with self.subTest(phase="receive_then_release_atomically_and_refuse_reuse"):
            receipt = s.receive(self.packet, self.work, self.bridge)
            self.assertFalse((self.work / 'operator-ready').exists())
            self.assertEqual((self.bridge / 'requests').stat().st_mode & 0o777, 0o700)
            config = s.spool.document(self.bridge / 'bridge.json')
            self.assertEqual(config['context'], self.context)
            self.assertEqual(config['timeout_seconds'], self.request['signing_response_timeout_seconds'])
            with self.assertRaises(FileExistsError):
                s.receive(self.packet, self.work, self.bridge)
            self.assertTrue(s.release(receipt, self.work)['ready'])
            self.assertEqual((self.work / 'operator-ready').read_text(), s.spool.digest(self.raw))
            self.assertEqual((self.work / 'operator-ready').stat().st_nlink, 1)
            with self.assertRaises(OSError):
                s.release(receipt, self.work)
        restart_fixture(self)
        with self.subTest(phase="checked_bundle_materializes_verified_bytes_for_native_crypto"):
            files = {name: base64.b64decode(self.packet['files'][f'new-public/{name}'])
                     for name in s.PUBLIC}
            observed = {}

            def identities(inputs, paths):
                self.assertEqual(set(inputs), set(s.trust.FILES))
                self.assertEqual(set(paths), set(s.trust.FILES))
                for name in s.trust.FILES:
                    self.assertEqual(Path(paths[name]).read_bytes(), inputs[name])
                    observed[name] = Path(paths[name])
                return {'registry-owner.pub': self.request['new_owner']}

            with patch.object(s.trust, 'identities', side_effect=identities) as native:
                self.assertEqual(
                    s.checked_bundle(files, self.request['new_manifest_sha256']),
                    {'registry-owner.pub': self.request['new_owner']})
                native.assert_called_once()
            self.assertTrue(observed)
            self.assertTrue(all(not path.exists() for path in observed.values()))
        restart_fixture(self)
        with self.subTest(phase="release_rechecks_exact_request_and_receipt"):
            receipt = s.receive(self.packet, self.work, self.bridge)
            with self.assertRaises(ValueError): s.release({**receipt, 'packet_sha256': '0'*64}, self.work)
            (self.work / 'finalization-request.json').write_bytes(self.raw + b' ')
            with self.assertRaises(ValueError): s.release(receipt, self.work)
            self.assertFalse((self.work / 'operator-ready').exists())
        restart_fixture(self)
        with self.subTest(phase="changed_worker_binding_and_incomplete_receive_never_release"):
            binding = {'schema': 1, 'namespace': 'ci', 'pod': 'runner', 'container': 'dind',
                'pod_uid': self.context['pod_uid'], 'container_id': 'containerd://outer',
                'image': 'fixture:public', 'image_id': 'sha256:' + '3'*64, 'restart_count': 0,
                'pod_spec_sha256': '4'*64, 'context_sha256': s.spool.context(self.context),
                'endpoint': str(s.BRIDGE / 'signing-spool-endpoint.py'),
                'docker': {'id': self.context['container_id'], 'image_id': self.context['image_id'],
                           'started_at': 'fixture-start', 'restart_count': 0}}
            s.bound_context(self.context, binding)
            for key in ('id', 'image_id'):
                changed = copy.deepcopy(binding)
                changed['docker'][key] = ('sha256:' if key == 'image_id' else '') + '9'*64
                with self.assertRaises(ValueError): s.bound_context(self.context, changed)
            client = type('Client', (), {'binding': binding})()
            with patch.object(s, 'remote_call', return_value={'incomplete': True}) as call:
                with self.assertRaises(ValueError): s.stage(client, self.packet, self.raw)
                self.assertEqual(call.call_count, 1)
                self.assertEqual(call.call_args.args[1], 'receive')
            with patch.object(s, 'remote_call', side_effect=ValueError('postcheck failed')) as call:
                with self.assertRaises(ValueError): s.stage(client, self.packet, self.raw)
                self.assertEqual(call.call_count, 1)
                self.assertEqual(call.call_args.args[1], 'receive')

    def test_rejects_changed_inputs_before_writes(self):
        with self.subTest(phase="changed_request_receipt_manifest_context_and_extra_files_rejected_before_writes"):
            for mutation in ('request', 'receipt', 'manifest', 'context', 'extra'):
                packet = copy.deepcopy(self.packet)
                if mutation == 'request': packet['request_sha256'] = '0' * 64
                elif mutation == 'receipt': packet['files']['retained-rootfs.json'] = base64.b64encode(b'{}').decode()
                elif mutation == 'manifest': packet['files']['new-public/manifest.json'] = base64.b64encode(b'{}').decode()
                elif mutation == 'context': packet['context']['target'] = 'other:target'
                else: packet['files']['../../escape'] = 'YWJj'
                with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                    s.receive(packet, self.work, self.bridge)
                self.assertFalse((self.work / '.operator-staging').exists())
        restart_fixture(self)
        with self.subTest(phase="helper_symlink_and_existing_ready_are_never_overwritten"):
            source = self.work / 'helpers/os/dev' / s.CODE[0]
            source.unlink()
            source.symlink_to('/etc/hostname')
            with self.assertRaises(ValueError): s.receive(self.packet, self.work, self.bridge)
            self.assertFalse((self.work / 'operator-ready').exists())
        restart_fixture(self)
        with self.subTest(phase="existing_gate_is_not_overwritten"):
            receipt = s.receive(self.packet, self.work, self.bridge)
            (self.work / 'operator-ready').write_bytes(b'existing')
            with self.assertRaises(OSError): s.release(receipt, self.work)
            self.assertEqual((self.work / 'operator-ready').read_bytes(), b'existing')

    def test_transfer_identity_checks(self):
        with self.subTest(phase="failed_post_transfer_identity_is_rejected"):
            class Client:
                binding = {'docker': {'id': '1' * 64}}
                checks = 0
                def check(self):
                    self.checks += 1
                    if self.checks == 2: raise ValueError('worker replaced')
                def exec_prefix(self): return ['existing-authenticated-exec']
                def runner(self, argv, sink, limit, source, timeout): sink.write(b'{"ready":false}')
            with self.assertRaises(ValueError): s.remote_call(Client(), 'receive', b'{}')
        restart_fixture(self)
        with self.subTest(phase="transfer_checks_identity_before_and_after_fixed_nested_command"):
            events = []
            class Client:
                binding = {'docker': {'id': '1' * 64}}
                def check(self): events.append('check')
                def exec_prefix(self): return ['existing-authenticated-exec']
                def runner(self, argv, sink, limit, source, timeout):
                    events.append(argv)
                    self_payload = source.read()
                    assert self_payload == b'{}'
                    sink.write(b'{"ok":true}')
            self.assertEqual(s.remote_call(Client(), 'receive', b'{}'), {'ok': True})
            self.assertEqual(events[0], 'check')
            self.assertEqual(events[-1], 'check')
            self.assertEqual(events[1][-3:], ['/usr/bin/python3', '/work/helpers/os/dev/stage-finalization-worker.py', 'receive'])


if __name__ == '__main__':
    unittest.main()

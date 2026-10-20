import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from types import SimpleNamespace
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'tools/tests'))
from hang import HANG_BOUND  # noqa: E402
import signing_spool as spool
from restart_fixture import restart_fixture
SPEC = importlib.util.spec_from_file_location('transfer', Path(__file__).resolve().parents[1] / 'dev/transfer-signing-spool.py')
transfer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(transfer)


class TransportTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir='/var/lib', prefix='tilo-transfer-test-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / 'remote').mkdir(mode=0o700)
        self.context = {'schema': 1, 'job_id': 'fixture', 'pod_uid': 'uid', 'container_id': 'containerd://abc',
                        'image_id': 'sha256:' + 'a' * 64, 'revision': 'a' * 40, 'architecture': 'aarch64',
                        'manifest_sha256': 'b' * 64, 'target': 'registry.invalid/test'}
        self.config = self.root / 'bridge.json'
        self.config.write_bytes(spool.canonical({'context': self.context, 'spool': str(self.root / 'remote')}))
        self.nonce = 'a' * 32
        self.remote = self.root / 'remote' / self.nonce
        self.remote.mkdir(mode=0o700)
        self.data = b'public unsigned payload'
        self.request = {'schema': 1, 'request_id': self.nonce, 'context_sha256': spool.context(self.context),
                        'operation': 'sign-efi', 'input_sha256': spool.digest(self.data), 'input_bytes': len(self.data)}
        (self.remote / 'request.json').write_bytes(spool.canonical(self.request))
        (self.remote / 'input').write_bytes(self.data)
        spec = {'containers': [{'name': 'tools', 'image': 'image@sha256:abc'}]}
        self.pod = {'metadata': {'uid': 'uid', 'name': 'test-pod', 'namespace': 'test'}, 'spec': spec,
                    'status': {'phase': 'Running', 'containerStatuses': [{'name': 'tools', 'imageID': 'sha256:abc',
                    'containerID': 'containerd://abc', 'restartCount': 0, 'state': {'running': {'startedAt': 'now'}}}]}}
        self.binding = {'schema': 1, 'namespace': 'test', 'pod': 'test-pod', 'container': 'tools', 'pod_uid': 'uid',
                        'container_id': 'containerd://abc', 'image': 'image@sha256:abc', 'image_id': 'sha256:abc',
                        'restart_count': 0, 'pod_spec_sha256': spool.digest(spool.canonical(spec)),
                        'context_sha256': spool.context(self.context), 'endpoint': '/trusted/signing-spool-endpoint.py'}
        self.events = []
        self.transfer_timeouts = []
        self.client = transfer.Transport('existing-context', self.binding, self.runner)
        self.mutate_after = None
        self.nested = None
        self.mutate_nested_after = None

    def runner(self, argv, sink, limit, source=None, **kwargs):
        if 'get' in argv:
            self.events.append('check')
            data = spool.canonical(self.pod)
        elif 'inspect' in argv:
            self.events.append('docker-check')
            self.assertEqual(argv[-1], self.binding['docker']['id'])
            self.assertNotIn('Config', argv[argv.index('--format') + 1])
            data = spool.canonical(self.nested)
        else:
            if self.nested:
                index = argv.index('docker')
                self.assertEqual(argv[index:index + 4], ['docker', 'exec', '-i', self.binding['docker']['id']])
            index = argv.index('/trusted/signing-spool-endpoint.py')
            operation = argv[index + 1]
            self.events.append(operation)
            self.transfer_timeouts.append((operation, kwargs.get('timeout')))
            response = argv[argv.index('--response') + 1] if '--response' in argv else None
            nonce = argv[argv.index('--request-id') + 1] if '--request-id' in argv else None
            output = io.BytesIO()
            transfer.endpoint.execute(self.config, operation, nonce, response, source or io.BytesIO(), output)
            data = output.getvalue()
            if self.mutate_after == operation:
                self.pod['status']['containerStatuses'][0]['restartCount'] += 1
            if self.mutate_nested_after == operation:
                self.nested['State']['StartedAt'] = 'replacement-start'
        if len(data) > limit:
            raise ValueError('fixture transfer exceeds byte admission')
        sink.write(data)

    def test_direct_transport(self):
        with self.subTest(phase="fetch_and_response_delivery_with_every_transfer_bracketed"):
            local = self.root / 'received'
            self.client.fetch(self.nonce, local)
            self.assertEqual((local / 'input').read_bytes(), self.data)
            self.assertIn(('input', transfer.payload_timeout(len(self.data))), self.transfer_timeouts)
            self.assertEqual(self.events, ['check', 'request', 'check', 'check', 'input', 'check'])
            outgoing = self.root / 'outgoing'
            outgoing.mkdir(mode=0o700)
            signed = b'public signed result'
            response = {k: self.request[k] for k in ('schema', 'request_id', 'context_sha256', 'operation', 'input_sha256')}
            response.update(output_sha256=spool.digest(signed), output_bytes=len(signed))
            (outgoing / 'output').write_bytes(signed)
            (outgoing / 'response.json').write_bytes(spool.canonical(response))
            self.events.clear()
            self.client.deliver(outgoing)
            self.assertEqual((self.remote / 'output').read_bytes(), signed)
            self.assertIn(('put-output', transfer.payload_timeout(len(signed))), self.transfer_timeouts)
            self.assertEqual(spool.document(self.remote / 'response.json'), response)
            self.assertEqual(self.events, ['check', 'request', 'check', 'check', 'put-output', 'check', 'check', 'publish-response', 'check'])
            with self.assertRaises(FileExistsError):
                self.client.deliver(outgoing)
        restart_fixture(self)
        with self.subTest(phase="container_restart_after_input_does_not_publish_local_request"):
            self.mutate_after = 'input'
            local = self.root / 'received'
            with self.assertRaisesRegex(ValueError, 'container identity'):
                self.client.fetch(self.nonce, local)
            self.assertFalse((local / 'request.json').exists())
        restart_fixture(self)
        with self.subTest(phase="pod_uid_container_id_image_and_spec_changes_rejected"):
            original = copy.deepcopy(self.pod)
            for kind in ('uid', 'container', 'image', 'spec'):
                self.pod = copy.deepcopy(original)
                if kind == 'uid':
                    self.pod['metadata']['uid'] = 'replacement'
                elif kind == 'container':
                    self.pod['status']['containerStatuses'][0]['containerID'] = 'containerd://replacement'
                elif kind == 'image':
                    self.pod['status']['containerStatuses'][0]['imageID'] = 'replacement'
                else:
                    self.pod['spec']['hostPID'] = True
                with self.subTest(kind=kind), self.assertRaises(ValueError):
                    self.client.transfer('list')
        restart_fixture(self)
        with self.subTest(phase="traversal_and_symlink_input_rejected"):
            with self.assertRaises(ValueError):
                self.client.fetch('../other', self.root / 'received')
            (self.remote / 'input').unlink()
            (self.remote / 'input').symlink_to(self.config)
            with self.assertRaises(OSError):
                self.client.fetch(self.nonce, self.root / 'received')
        restart_fixture(self)
        with self.subTest(phase="corrupt_response_never_published"):
            response = {k: self.request[k] for k in ('schema', 'request_id', 'context_sha256', 'operation', 'input_sha256')}
            response.update(output_sha256='0' * 64, output_bytes=3)
            with self.assertRaises(ValueError):
                transfer.endpoint.execute(self.config, 'put-output', self.nonce, spool.canonical(response).decode(), io.BytesIO(b'bad'), io.BytesIO())
            self.assertFalse((self.remote / 'output').exists())
            self.assertFalse((self.remote / 'response.json').exists())

    def test_transport_bounds_and_deadlines(self):
        with self.subTest(phase="real_process_byte_bound_and_timeout"):
            with self.assertRaisesRegex(ValueError, 'byte admission'):
                transfer.bounded_process([sys.executable, '-c', 'print("x"*100)'], io.BytesIO(), 10)
            with self.assertRaisesRegex(ValueError, 'deadline'):
                transfer.bounded_process([sys.executable, '-c', 'import time; time.sleep(3600)'], io.BytesIO(), 10, timeout=0.1)
        restart_fixture(self)
        with self.subTest(phase="large_payload_deadline_includes_a_conservative_stream_budget"):
            mib = 1024 * 1024
            self.assertEqual(transfer.payload_timeout(256 * mib), 376)
            self.assertEqual(transfer.payload_timeout(spool.MAX_INPUT + 65536), 633)
            with self.assertRaises(ValueError):
                transfer.payload_timeout(spool.MAX_INPUT + 65537)
        restart_fixture(self)
        with self.subTest(phase="timeout_kills_descendant_after_leader_exits"):
            output = io.BytesIO()
            script = ('import subprocess,sys; '
                      'p=subprocess.Popen([sys.executable,"-c","import time; time.sleep(3600)"]); '
                      'print(p.pid,flush=True)')
            # The transfer clock runs out once the leader has said its
            # descendant's pid, never while a loaded runner is starting it.
            clock = SimpleNamespace(monotonic=lambda: 1e9 if output.getvalue() else 0.0)
            with self.assertRaisesRegex(ValueError, 'deadline'), patch.object(transfer, 'time', clock):
                transfer.bounded_process([sys.executable, '-c', script], output, 1024, timeout=HANG_BOUND)
            child = int(output.getvalue())
            status = Path(f'/proc/{child}/stat')
            try:
                deadline = time.monotonic() + HANG_BOUND
                while status.exists() and status.read_text().split()[2] != 'Z' and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue(not status.exists() or status.read_text().split()[2] == 'Z')
            finally:
                try:
                    os.kill(child, 9)
                except ProcessLookupError:
                    pass

    def enable_nested(self):
        self.binding['docker'] = {'id': 'c' * 64, 'image_id': 'sha256:' + 'd' * 64,
                                  'started_at': '2026-09-18T00:00:00Z', 'restart_count': 0}
        self.nested = {'Id': 'c' * 64, 'Image': 'sha256:' + 'd' * 64, 'RestartCount': 0,
                       'State': {'StartedAt': '2026-09-18T00:00:00Z', 'Status': 'running',
                                 'Running': True, 'Paused': False, 'Restarting': False, 'Dead': False}}
        self.client = transfer.Transport('existing-context', self.binding, self.runner)

    def test_nested_docker_transport(self):
        with self.subTest(phase="nested_docker_transfer_checks_both_runtime_identities"):
            self.enable_nested()
            self.client.fetch(self.nonce, self.root / 'received')
            self.assertEqual(self.events, ['check', 'docker-check', 'request', 'check', 'docker-check',
                                          'check', 'docker-check', 'input', 'check', 'docker-check'])
        restart_fixture(self)
        with self.subTest(phase="nested_restart_after_input_quarantines_request"):
            self.enable_nested()
            self.mutate_nested_after = 'input'
            local = self.root / 'received'
            with self.assertRaisesRegex(ValueError, 'nested Docker'):
                self.client.fetch(self.nonce, local)
            self.assertFalse((local / 'request.json').exists())
        restart_fixture(self)
        with self.subTest(phase="nested_identity_and_state_substitution_rejected"):
            self.enable_nested()
            original = copy.deepcopy(self.nested)
            for field, value in (('Id', 'e' * 64), ('Image', 'sha256:' + 'e' * 64),
                                 ('RestartCount', 1), ('Paused', True), ('Running', False),
                                 ('Restarting', True), ('Dead', True), ('StartedAt', 'new')):
                self.nested = copy.deepcopy(original)
                (self.nested if field in original else self.nested['State'])[field] = value
                with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'nested Docker'):
                    self.client.transfer('list')
        restart_fixture(self)
        with self.subTest(phase="nested_container_names_and_short_ids_rejected"):
            self.enable_nested()
            for identifier in ('worker-name', 'abc123', '--privileged'):
                self.binding['docker']['id'] = identifier
                with self.assertRaisesRegex(ValueError, 'nested Docker'):
                    transfer.Transport('existing-context', self.binding, self.runner)

if __name__ == '__main__':
    unittest.main()

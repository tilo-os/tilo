import copy
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
import signing_spool as spool
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
        self.client = transfer.Transport('existing-context', self.binding, self.runner)
        self.mutate_after = None

    def runner(self, argv, sink, limit, source=None, **kwargs):
        if 'get' in argv:
            self.events.append('check')
            data = spool.canonical(self.pod)
        else:
            index = argv.index('/trusted/signing-spool-endpoint.py')
            operation = argv[index + 1]
            self.events.append(operation)
            response = argv[argv.index('--response') + 1] if '--response' in argv else None
            nonce = argv[argv.index('--request-id') + 1] if '--request-id' in argv else None
            output = io.BytesIO()
            transfer.endpoint.execute(self.config, operation, nonce, response, source or io.BytesIO(), output)
            data = output.getvalue()
            if self.mutate_after == operation:
                self.pod['status']['containerStatuses'][0]['restartCount'] += 1
        if len(data) > limit:
            raise ValueError('fixture transfer exceeds byte admission')
        sink.write(data)

    def test_fetch_and_response_delivery_with_every_transfer_bracketed(self):
        local = self.root / 'received'
        self.client.fetch(self.nonce, local)
        self.assertEqual((local / 'input').read_bytes(), self.data)
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
        self.assertEqual(spool.document(self.remote / 'response.json'), response)
        self.assertEqual(self.events, ['check', 'request', 'check', 'check', 'put-output', 'check', 'check', 'publish-response', 'check'])
        with self.assertRaises(FileExistsError):
            self.client.deliver(outgoing)

    def test_container_restart_after_input_does_not_publish_local_request(self):
        self.mutate_after = 'input'
        local = self.root / 'received'
        with self.assertRaisesRegex(ValueError, 'container identity'):
            self.client.fetch(self.nonce, local)
        self.assertFalse((local / 'request.json').exists())

    def test_pod_uid_container_id_image_and_spec_changes_rejected(self):
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

    def test_traversal_and_symlink_input_rejected(self):
        with self.assertRaises(ValueError):
            self.client.fetch('../other', self.root / 'received')
        (self.remote / 'input').unlink()
        (self.remote / 'input').symlink_to(self.config)
        with self.assertRaises(OSError):
            self.client.fetch(self.nonce, self.root / 'received')

    def test_real_process_byte_bound_and_timeout(self):
        with self.assertRaisesRegex(ValueError, 'byte admission'):
            transfer.bounded_process([sys.executable, '-c', 'print("x"*100)'], io.BytesIO(), 10)
        with self.assertRaisesRegex(ValueError, 'deadline'):
            transfer.bounded_process([sys.executable, '-c', 'import time; time.sleep(2)'], io.BytesIO(), 10, timeout=0.1)

    def test_corrupt_response_never_published(self):
        response = {k: self.request[k] for k in ('schema', 'request_id', 'context_sha256', 'operation', 'input_sha256')}
        response.update(output_sha256='0' * 64, output_bytes=3)
        with self.assertRaises(ValueError):
            transfer.endpoint.execute(self.config, 'put-output', self.nonce, spool.canonical(response).decode(), io.BytesIO(b'bad'), io.BytesIO())
        self.assertFalse((self.remote / 'output').exists())
        self.assertFalse((self.remote / 'response.json').exists())

if __name__ == '__main__':
    unittest.main()


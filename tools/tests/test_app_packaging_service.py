import hashlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import app_packaging_service as service


class Response(io.BytesIO):
    def __init__(self, raw, url, headers=None):
        super().__init__(raw)
        self.url, self.headers = url, headers or {}
    def geturl(self):
        return self.url


class TransportTests(unittest.TestCase):
    def test_routes_and_no_redirect_handler(self):
        for path in ('user', service.PREFIX, service.PREFIX + '/branches/main',
                     service.PREFIX + '/actions/jobs/2/logs'):
            self.assertTrue(service.allowed(path, 'GET'))
        for path in ('https://evil/user', '/user', 'repos/other/repo',
                     service.PREFIX + '/contents/..?ref=' + 'a'*40):
            self.assertFalse(service.allowed(path, 'GET'))
        self.assertTrue(service.allowed(service.DISPATCH, 'POST'))
        self.assertFalse(service.allowed('user', 'POST'))
        self.assertFalse(service.allowed(service.DISPATCH, 'DELETE'))
        self.assertIsNone(service.NoRedirect().redirect_request(None, None, None, None, None, None))

    def test_bounded_bytes_and_download_and_error_redaction(self):
        transport = service.Transport('private-sentinel')
        url = service.ORIGIN + '/api/v1/user'
        with patch.object(transport.opener, 'open', return_value=Response(b'12345', url)):
            with self.assertRaisesRegex(ValueError, 'exceeds bound'):
                transport.request('user', maximum=4)
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / 'artifact'
            with patch.object(transport.opener, 'open', return_value=Response(b'1234', url)):
                transport.request('user', maximum=4, output=output)
            self.assertEqual(output.read_bytes(), b'1234')
        with patch.object(transport.opener, 'open', side_effect=OSError('private-sentinel')):
            with self.assertRaisesRegex(ValueError, '^authenticated API request failed$'):
                transport.request('user')
        with patch.object(transport.opener, 'open', return_value=Response(b'ok', 'https://evil/')):
            with self.assertRaisesRegex(ValueError, 'redirect rejected'):
                transport.request('user')
        with patch.object(transport.opener, 'open', return_value=Response(b'ok', url)), \
                patch.object(service.time, 'monotonic', side_effect=[0, 31]):
            with self.assertRaisesRegex(ValueError, 'deadline'):
                transport.request('user')


class ServiceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.state, self.policyfile, self.finalfile, self.tokenfile = [self.root / x for x in ('state', 'policy', 'final', 'token')]
        self.builder = 'code.podesta.ai/qos/tilo-builder@sha256:' + 'a'*64
        descriptor = {'sha256': 'b'*64, 'bytes': 100, 'source_sha': 'c'*40,
            'compatible_builder': self.builder, 'verifier_sha256': 'd'*64,
            'canonical_binaries': {'tilo_pkg_sha256': 'e'*64, 'tilo_runtime_bundle_sha256': 'f'*64}}
        arch = service.bundle.native_arch()
        descriptor['url'] = f"{service.ORIGIN}/api/packages/qos/generic/tilo-canonical-verifiers/{descriptor['sha256']}/{arch}.tar.gz"
        self.finalfile.write_text(json.dumps({'verifier_sha256': 'd'*64, 'canonical_verifier_bundles': {arch: descriptor}}))
        self.policy = {'enabled': True, 'qualification_only': True, 'service_user_id': 42,
            'bundle_compatibility_approved': True, 'builder_image': self.builder,
            'collection': 'default', 'architectures': ['x86_64'],
            'final_receipt_policy_sha256': hashlib.sha256(self.finalfile.read_bytes()).hexdigest()}
        self.write_policy()
        self.tokenfile.write_text('dedicated-token\n')
        self.source = '1'*40
        self.user = {'id': 42, 'is_admin': False}
        self.dispatches = []
        self.uncertain = False
        self.responses = {}
        owner = self

        class FakeTransport:
            def __init__(self, token):
                owner.assertEqual(token, 'dedicated-token')
            def request(self, path, data=None, **kwargs):
                if path == 'user':
                    value = owner.user
                elif path == service.PREFIX:
                    value = {'id': 23, 'full_name': 'qos/qos'}
                elif path == service.PREFIX + '/branches/main':
                    value = {'name': 'main', 'commit': {'id': owner.source}}
                elif data is not None:
                    owner.dispatches.append(data)
                    if owner.uncertain:
                        raise ValueError('lost dispatch response')
                    value = {'id': len(owner.dispatches)}
                    owner.responses[value['id']] = dict(value, status='running')
                else:
                    value = owner.responses[int(path.rsplit('/', 1)[1])]
                return json.dumps(value).encode()
        self.transport = patch.object(service, 'Transport', FakeTransport)
        self.transport.start()
        self.addCleanup(self.transport.stop)
        self.qualify = patch.object(service.ManagedProduct, 'qualify_receipt', lambda *_: None)
        self.qualify.start()
        self.addCleanup(self.qualify.stop)
        self.validate = patch.object(service.ManagedProduct, 'validate_run', lambda *_: None)
        self.validate.start()
        self.addCleanup(self.validate.stop)

    def write_policy(self):
        self.policyfile.write_text(json.dumps(self.policy))

    def invoke(self, apply=True):
        return service.invoke(self.state, self.policyfile, self.finalfile, self.tokenfile, apply)

    def test_readonly_without_files_or_credentials_does_not_create_state(self):
        self.policyfile.unlink()
        self.tokenfile.unlink()
        self.finalfile.unlink()
        result = self.invoke(False)
        self.assertTrue(result['dry_run'])
        self.assertFalse(self.state.exists())
        self.assertFalse(Path(str(self.state) + '.lock').exists())

    def test_repeated_ticks_dispatch_once_and_observe_only_latest_pending(self):
        first = self.invoke()
        self.assertEqual(first['state']['sequence'], 0)
        self.invoke()
        self.source = '2'*40
        pending = self.invoke()['state']
        self.assertEqual(pending['sequence'], 1)
        self.assertEqual(pending['desired']['source_sha'], self.source)
        self.assertEqual(pending['active']['desired']['source_sha'], '1'*40)
        self.assertEqual(len(self.dispatches), 1)
        self.assertEqual(self.dispatches[0]['inputs']['publish'], 'false')

    def test_identity_disabled_compatibility_and_final_policy_rotation_stop_before_state(self):
        self.user['is_admin'] = True
        with self.assertRaisesRegex(ValueError, 'administrative'):
            self.invoke()
        self.user['is_admin'] = False
        for key in ('enabled', 'bundle_compatibility_approved'):
            self.policy[key] = False
            self.write_policy()
            with self.assertRaises(ValueError):
                self.invoke()
            self.policy[key] = True
        self.write_policy()
        self.finalfile.write_text('{}')
        with self.assertRaises(ValueError):
            self.invoke()
        self.assertFalse(self.state.exists())
        self.assertEqual(self.dispatches, [])

    def test_rotation_during_qualification_cannot_dispatch(self):
        def rotate(*args):
            self.policy['enabled'] = False
            self.write_policy()
        with patch.object(service.ManagedProduct, 'qualify_receipt', rotate):
            with self.assertRaisesRegex(ValueError, 'changed during invocation'):
                self.invoke()
        self.assertEqual(self.dispatches, [])

    def test_uncertain_dispatch_is_durable_and_never_resent(self):
        self.uncertain = True
        with self.assertRaisesRegex(ValueError, 'lost dispatch response'):
            self.invoke()
        result = self.invoke()
        self.assertEqual(result['next_action'], 'reconcile-required')
        self.assertEqual(len(self.dispatches), 1)
        self.assertEqual(result['state']['active']['status'], 'dispatch-uncertain')

    def test_policy_rotation_during_acceptance_cannot_advance_baseline(self):
        self.invoke()
        self.responses[1]['status'] = 'success'
        def rotate(*args):
            self.policy['enabled'] = False
            self.write_policy()
            return {'forged': 'verification'}
        with patch.object(service.ManagedProduct, 'validate_run', rotate):
            with self.assertRaisesRegex(ValueError, 'changed during invocation'):
                self.invoke()
        state = self.invoke(False)['state']
        self.assertIsNone(state['baseline'])
        self.assertIsNotNone(state['active'])

    def test_overlapping_invocation_refuses_before_dispatch(self):
        with service.invocation_lock(self.state):
            with self.assertRaisesRegex(ValueError, 'already active'):
                self.invoke()
        self.assertEqual(self.dispatches, [])


if __name__ == '__main__':
    unittest.main()

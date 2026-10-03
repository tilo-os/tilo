import hashlib
import copy
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.append(str(Path(__file__).resolve().parents[1] / 'apps'))
import app_packaging_service as service


class Response(io.BytesIO):
    def __init__(self, raw, url, headers=None):
        super().__init__(raw)
        self.url, self.headers = url, headers or {}
    def geturl(self):
        return self.url


class TransportTests(unittest.TestCase):
    def test_transport_routes_are_bounded_redacted_and_never_redirect(self):
        with self.subTest(phase='recovery transport rejects even allowed dispatch before network'):
            transport = service.Transport('private-sentinel', read_only=True)
            with patch.object(transport.opener, 'open') as opened:
                with self.assertRaisesRegex(ValueError, 'read-only'):
                    transport.request(service.DISPATCH, {'ref': 'a'*40})
                opened.assert_not_called()
        with self.subTest(phase='routes and no redirect handler'):
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
        with self.subTest(phase='bounded bytes and download and error redaction'):
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


class ServiceFixture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.state, self.policyfile, self.finalfile, self.tokenfile = [self.root / x for x in ('state', 'policy', 'final', 'token')]
        self.builder = 'registry.tilo.so/tilo/tilo-builder@sha256:' + 'a'*64
        descriptor = {'sha256': 'b'*64, 'bytes': 100, 'source_sha': 'c'*40,
            'compatible_builder': self.builder, 'verifier_sha256': 'd'*64,
            'canonical_binaries': {'tilo_pkg_sha256': 'e'*64, 'tilo_runtime_bundle_sha256': 'f'*64}}
        arch = service.bundle.native_arch()
        descriptor['url'] = f"{service.ORIGIN}/api/packages/tilo/generic/tilo-canonical-verifiers/{descriptor['sha256']}/{arch}.tar.gz"
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
        self.requests = []
        owner = self

        class FakeTransport:
            def __init__(self, token, read_only=False):
                owner.assertEqual(token, 'dedicated-token')
                self.read_only = read_only
            def request(self, path, data=None, **kwargs):
                owner.requests.append((path, data))
                if self.read_only and data is not None:
                    raise AssertionError('recovery attempted POST')
                if path == 'user':
                    value = owner.user
                elif path == service.PREFIX:
                    value = {'id': 23, 'full_name': 'tilo/tilo'}
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

    def invoke(self, apply=True, reconcile_run_id=None):
        return service.invoke(self.state, self.policyfile, self.finalfile, self.tokenfile, apply, reconcile_run_id)


class ServiceTests(ServiceFixture):
    def test_service_refuses_before_state_or_dispatch_when_not_qualified(self):
        with self.subTest(phase='readonly without files or credentials does not create state'):
            self.doCleanups()
            self.setUp()
            self.policyfile.unlink()
            self.tokenfile.unlink()
            self.finalfile.unlink()
            result = self.invoke(False)
            self.assertTrue(result['dry_run'])
            self.assertFalse(self.state.exists())
            self.assertFalse(Path(str(self.state) + '.lock').exists())
        with self.subTest(phase='identity disabled compatibility and final policy rotation stop before state'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='rotation during qualification cannot dispatch'):
            self.doCleanups()
            self.setUp()
            def rotate(*args):
                self.policy['enabled'] = False
                self.write_policy()
            with patch.object(service.ManagedProduct, 'qualify_receipt', rotate):
                with self.assertRaisesRegex(ValueError, 'changed during invocation'):
                    self.invoke()
            self.assertEqual(self.dispatches, [])
        with self.subTest(phase='overlapping invocation refuses before dispatch'):
            self.doCleanups()
            self.setUp()
            with service.invocation_lock(self.state):
                with self.assertRaisesRegex(ValueError, 'already active'):
                    self.invoke()
            self.assertEqual(self.dispatches, [])

    def test_dispatch_happens_once_and_baseline_advances_only_under_current_policy(self):
        with self.subTest(phase='repeated ticks dispatch once and observe only latest pending'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='uncertain dispatch is durable and never resent'):
            self.doCleanups()
            self.setUp()
            self.uncertain = True
            with self.assertRaisesRegex(ValueError, 'lost dispatch response'):
                self.invoke()
            result = self.invoke()
            self.assertEqual(result['next_action'], 'reconcile-required')
            self.assertEqual(len(self.dispatches), 1)
            self.assertEqual(result['state']['active']['status'], 'dispatch-uncertain')
        with self.subTest(phase='policy rotation during acceptance cannot advance baseline'):
            self.doCleanups()
            self.setUp()
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


class RecoveryTests(ServiceFixture):
    def setUp(self):
        super().setUp()
        # Recovery must exercise the real product identity/input validator.
        self.validate.stop()

    def uncertain_intent(self):
        self.uncertain = True
        with self.assertRaisesRegex(ValueError, 'lost dispatch response'):
            self.invoke()
        data = self.dispatches[0]
        self.responses[17] = {'id': 17, 'repository': {'id': 23, 'full_name': 'tilo/tilo'},
            'workflow_id': 'release-apps.yml', 'trigger_event': 'workflow_dispatch',
            'commit_sha': data['ref'], 'event_payload': json.dumps({'ref': data['ref'], 'inputs': data['inputs']}),
            'trigger_user': {'id': 42, 'is_admin': False}, 'status': 'running'}
        self.requests.clear()
        return self.invoke(False)['state']

    def test_recovery_binds_only_the_exact_run_without_dispatching(self):
        with self.subTest(phase='recovery binds exact run without observing main or dispatching'):
            self.doCleanups()
            self.setUp()
            before = self.uncertain_intent()
            self.source = '2'*40
            result = self.invoke(reconcile_run_id=17)
            self.assertEqual(result['next_action'], 'poll')
            state = result['state']
            self.assertEqual(state['active']['id'], before['active']['id'])
            self.assertEqual(state['active']['run_id'], 17)
            self.assertEqual(state['desired'], before['desired'])
            self.assertEqual(state['sequence'], before['sequence'])
            self.assertEqual(len(self.dispatches), 1)
            self.assertTrue(all(data is None for _, data in self.requests))
            self.assertNotIn((service.PREFIX + '/branches/main', None), self.requests)
        with self.subTest(phase='recovery accepts forgejo normalized inputs without redispatch'):
            self.doCleanups()
            self.setUp()
            before = self.uncertain_intent()
            payload = json.loads(self.responses[17]['event_payload'])
            payload['inputs'] = {key: value for key, value in payload['inputs'].items() if value != ''}
            payload['inputs']['verify_final_receipt'] = 'false'
            self.responses[17]['event_payload'] = json.dumps(payload)
            result = self.invoke(reconcile_run_id=17)
            self.assertEqual(result['state']['active']['id'], before['active']['id'])
            self.assertEqual(result['state']['active']['desired'], before['active']['desired'])
            self.assertEqual(result['state']['active']['run_id'], 17)
            self.assertEqual(result['next_action'], 'poll')
            self.assertEqual(len(self.dispatches), 1)
            self.assertTrue(all(data is None for _, data in self.requests))
        with self.subTest(phase='wrong run actor and immutable intent leave state unchanged'):
            self.doCleanups()
            self.setUp()
            before = self.uncertain_intent()
            good = copy.deepcopy(self.responses[17])
            variants = [dict(good, id=18), dict(good, id=True),
                dict(good, trigger_user={'id': 43, 'is_admin': False}),
                dict(good, trigger_user={'id': 42, 'is_admin': True}),
                dict(good, trigger_user=None), dict(good, trigger_user={'id': 42}),
                dict(good, workflow_id='other.yml'), dict(good, trigger_event='push'),
                dict(good, commit_sha='2'*40), dict(good, repository={'id': 24, 'full_name': 'tilo/tilo'}),
                dict(good, repository={'id': 23, 'full_name': 'other/repo'}),
                dict(good, event_payload=None)]
            for key, value in [('coordinator_intent', 'unrelated-intent'), ('publish', 'true')]:
                payload = json.loads(good['event_payload'])
                payload['inputs'][key] = value
                variants.append(dict(good, event_payload=json.dumps(payload)))
            payload = json.loads(good['event_payload'])
            payload['ref'] = 'main'
            variants.append(dict(good, event_payload=json.dumps(payload)))
            for run in variants:
                with self.subTest(run=run):
                    self.responses[17] = run
                    with self.assertRaises(ValueError):
                        self.invoke(reconcile_run_id=17)
                    self.assertEqual(self.invoke(False)['state'], before)
            self.assertEqual(len(self.dispatches), 1)
            self.assertTrue(all(data is None for _, data in self.requests))
        with self.subTest(phase='recovery obeys lock and existing run binding'):
            self.doCleanups()
            self.setUp()
            self.uncertain_intent()
            with service.invocation_lock(self.state):
                with self.assertRaisesRegex(ValueError, 'already active'):
                    self.invoke(reconcile_run_id=17)
            self.assertEqual(self.requests, [])
            before = self.invoke(reconcile_run_id=17)['state']
            self.requests.clear()
            with self.assertRaisesRegex(ValueError, 'conflicts with active intent'):
                self.invoke(reconcile_run_id=18)
            self.assertEqual(self.requests, [])
            self.assertEqual(self.invoke(False)['state'], before)
        with self.subTest(phase='cli routes explicit recovery without using normal tick'):
            self.doCleanups()
            self.setUp()
            self.uncertain_intent()
            argv = ['app_packaging_service.py', '--state', str(self.state), '--policy', str(self.policyfile),
                '--final-policy', str(self.finalfile), '--token-file', str(self.tokenfile),
                '--apply', '--reconcile-run-id', '17']
            with patch.object(sys, 'argv', argv), patch('sys.stdout', new_callable=io.StringIO) as output, \
                    patch.object(service.core, 'tick', side_effect=AssertionError('recovery entered normal tick')):
                service.main()
            self.assertEqual(json.loads(output.getvalue())['state']['active']['run_id'], 17)
            self.assertTrue(all(data is None for _, data in self.requests))

    def test_recovered_run_advances_baseline_only_after_verified_acceptance(self):
        with self.subTest(phase='verified success advances baseline only after artifact acceptance'):
            self.doCleanups()
            self.setUp()
            before = self.uncertain_intent()
            self.responses[17]['status'] = 'success'
            proof = {'schema': 'tilo.coordinator-verification/v1', 'run_id': 17,
                'desired': before['active']['desired'], 'backend': 'canonical-final-artifact-v1',
                'receipts': {'x86_64': 'e'*64},
                'final_policy_sha256': self.policy['final_receipt_policy_sha256']}
            with patch.object(service.ManagedProduct, 'no_build_evidence', return_value=None), \
                    patch.object(service, 'FinalArtifactAcceptance', return_value=lambda *_: proof):
                state = self.invoke(reconcile_run_id=17)['state']
            self.assertIsNone(state['active'])
            self.assertEqual(state['baseline'], before['active']['desired'])
            self.assertEqual(state['baseline_evidence'], proof)
            self.assertEqual(state['last_attempt']['run_id'], 17)
            self.assertEqual(len(self.dispatches), 1)
            with self.assertRaisesRegex(ValueError, 'no active intent'):
                self.invoke(reconcile_run_id=17)
        with self.subTest(phase='unverified success retains run without baseline'):
            self.doCleanups()
            self.setUp()
            self.uncertain_intent()
            self.responses[17]['status'] = 'success'
            with patch.object(service.ManagedProduct, 'no_build_evidence', return_value=None), \
                    patch.object(service, 'FinalArtifactAcceptance', return_value=None):
                state = self.invoke(reconcile_run_id=17)['state']
            self.assertEqual(state['active']['status'], 'awaiting-verification')
            self.assertEqual(state['active']['run_id'], 17)
            self.assertIsNone(state['baseline'])
            self.assertEqual(len(self.dispatches), 1)

    def test_recovery_refuses_without_active_intent_or_current_qualified_policy(self):
        with self.subTest(phase='no active intent and invalid request cannot create state or call api'):
            self.doCleanups()
            self.setUp()
            for value in (0, -1, True, '17'):
                with self.subTest(value=value), self.assertRaisesRegex(ValueError, 'invalid reconciliation run ID'):
                    self.invoke(reconcile_run_id=value)
            with self.assertRaisesRegex(ValueError, 'requires --apply'):
                self.invoke(False, reconcile_run_id=17)
            with self.assertRaisesRegex(ValueError, 'no active intent'):
                self.invoke(reconcile_run_id=17)
            self.assertFalse(self.state.exists())
            self.assertEqual(self.requests, [])
        with self.subTest(phase='recovery requalifies source and rejects policy rotation'):
            self.doCleanups()
            self.setUp()
            before = self.uncertain_intent()
            with patch.object(service.ManagedProduct, 'qualify_receipt', side_effect=ValueError('untrusted source')):
                with self.assertRaisesRegex(ValueError, 'untrusted source'):
                    self.invoke(reconcile_run_id=17)
            self.assertNotIn((service.PREFIX + '/actions/runs/17', None), self.requests)
            def rotate(*args):
                self.policy['enabled'] = False
                self.write_policy()
            with patch.object(service.ManagedProduct, 'qualify_receipt', rotate):
                with self.assertRaisesRegex(ValueError, 'changed during invocation'):
                    self.invoke(reconcile_run_id=17)
            self.assertEqual(self.invoke(False)['state'], before)
            self.assertEqual(len(self.dispatches), 1)
        with self.subTest(phase='policy rotation while verification is unavailable cannot bind run'):
            self.doCleanups()
            self.setUp()
            before = self.uncertain_intent()
            self.responses[17]['status'] = 'success'
            def unavailable(*args):
                self.policy['enabled'] = False
                self.write_policy()
                raise service.core.VerificationRequired('artifact not available')
            with patch.object(service.ManagedProduct, 'no_build_evidence', unavailable):
                with self.assertRaisesRegex(ValueError, 'changed during invocation'):
                    self.invoke(reconcile_run_id=17)
            self.assertEqual(self.invoke(False)['state'], before)
        with self.subTest(phase='current policy identity must still match active intent'):
            self.doCleanups()
            self.setUp()
            before = self.uncertain_intent()
            self.policy['architectures'] = ['x86_64', 'aarch64']
            self.write_policy()
            with self.assertRaisesRegex(ValueError, 'active trusted policy changed'):
                self.invoke(reconcile_run_id=17)
            self.assertEqual(self.invoke(False)['state'], before)


if __name__ == '__main__':
    unittest.main()

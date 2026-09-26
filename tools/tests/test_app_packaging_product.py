import json
import io
import hashlib
import zipfile
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import app_packaging_coordinator as core
import app_packaging_product as product
import app_packaging_qualification as guard


POLICY = {'enabled': True, 'qualification_only': True, 'trusted_files': {},
          'final_receipt_policy_sha256': 'f' * 64}


def cargo_listing(path):
    if '/contents/?' in path:
        return [{'name': '.cargo', 'type': 'dir'}]
    if '/contents/.cargo?' in path:
        return [{'name': 'config.toml', 'type': 'file'}]
    return None


def desired(char='a'):
    return {'source_sha': char * 40, 'builder_image': 'registry.tilo.so/tilo/tilo-builder@sha256:' + 'd' * 64,
            'collection': 'default', 'architectures': ['x86_64', 'aarch64'],
            'policy_sha256': core.receipt_key(POLICY)}


def proof(intent, run):
    return {'schema': 'tilo.coordinator-verification/v1', 'run_id': run['id'],
            'desired': intent['desired'], 'backend': 'canonical-final-artifact-v1',
            'receipts': {arch: 'e' * 64 for arch in intent['desired']['architectures']},
            'final_policy_sha256': POLICY['final_receipt_policy_sha256']}


class API(product.ProductForgejo):
    def __init__(self):
        super().__init__(POLICY)
        self.runs, self.calls = {}, []
        self.uncertain = False
        self.documents = {}
        self.zero = False

    def artifact_bytes(self, artifact_id):
        output = io.BytesIO()
        with zipfile.ZipFile(output, 'w') as archive:
            archive.writestr('coordinator-plan.json', self.document_bytes(artifact_id))
        return output.getvalue()

    def document_bytes(self, run_id):
        return json.dumps(self.documents[run_id], sort_keys=True).encode()

    def raw_api(self, path, maximum):
        run_id = int(path.split('/')[-2])
        intent = json.loads(self.runs[run_id]['event_payload'])['inputs']['coordinator_intent']
        return f'TILO_COORDINATOR_PLAN {intent} {hashlib.sha256(self.document_bytes(run_id)).hexdigest()}\n'.encode()

    def qualify_receipt(self, receipt):
        product.validate_desired(receipt)

    def call(self, path, data=None):
        if data is None:
            if path.endswith('/jobs'):
                run_id = int(path.split('/')[-2])
                return [{'id': run_id, 'repo_id': 23, 'run_id': run_id, 'name': 'plan', 'status': 'success',
                         'attempt': 1, 'task_id': run_id, 'handle': 'handle-' + str(run_id)}]
            if '/artifacts?' in path:
                run_id = int(path.split('/')[-2])
                return [{'id': run_id, 'run_id': run_id, 'name': 'coordinator-plan-1', 'expired': False,
                         'size_in_bytes': len(self.artifact_bytes(run_id))}]
            return self.runs[int(path.rsplit('/', 1)[1])]
        self.calls.append(data)
        rid = len(self.calls)
        self.runs[rid] = {'id': rid, 'repository': {'id': 23, 'full_name': 'tilo/tilo'},
                         'workflow_id': 'release-apps.yml', 'trigger_event': 'workflow_dispatch',
                         'commit_sha': data['ref'], 'event_payload': json.dumps({'ref': data['ref'], 'inputs': data['inputs']}),
                         'status': 'running'}
        self.documents[rid] = {'schema': 'tilo.coordinator-plan/v1', 'run_id': rid, 'plan_attempt': 1,
                               'source_sha': data['ref'], 'builder_image': data['inputs']['builder_image'],
                               'inputs': {key: value for key, value in data['inputs'].items()
                                          if key != 'verify_final_receipt'}, 'plan': {'count': 0 if self.zero else 1,
                                'apps': [] if self.zero else ['terminal'], 'packages': [] if self.zero else ['terminal'],
                                'catalog': [] if self.zero else [{'name': 'terminal'}], 'features': []}}
        if self.uncertain:
            raise TimeoutError()
        return {'id': rid}


class ProductTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.store = core.Store(Path(self.temp.name) / 'db', writable=True)
        self.addCleanup(self.store.db.close)
        self.api = API()

    def start(self):
        self.store.observe(1, desired())
        core.tick(self.store, self.api)

    def test_dispatch_inputs_bind_only_current_policy_builder_and_verified_baselines(self):
        with self.subTest(phase='same policy baseline with other final policy is rejected before dispatch'):
            self.setUp()
            baseline = desired()
            evidence = proof({'desired': baseline}, {'id': 7})
            evidence['final_policy_sha256'] = '0' * 64
            intent = {'id': 'test', 'desired': desired('b'), 'baseline': baseline,
                      'baseline_evidence': evidence}
            with self.assertRaisesRegex(ValueError, 'final verification policy mismatch'):
                self.api.dispatch(intent)
            self.assertEqual(self.api.calls, [])
        with self.subTest(phase='rotated final policy forces full build without reusing old proof'):
            self.setUp()
            baseline = desired()
            evidence = proof({'desired': baseline}, {'id': 7})
            self.api.policy = dict(POLICY, final_receipt_policy_sha256='0' * 64)
            current = dict(desired('b'), policy_sha256=self.api.policy_sha)
            self.api.dispatch({'id': 'test', 'desired': current, 'baseline': baseline,
                               'baseline_evidence': evidence})
            self.assertEqual(self.api.calls[0]['inputs']['baseline_sha'], '')
            self.assertEqual(self.api.calls[0]['inputs']['baseline_evidence'], '')
        with self.subTest(phase='builder change forces full collection'):
            self.setUp()
            intent = {'id': 'intent', 'desired': desired('b'), 'baseline': desired(), 'baseline_evidence': None}
            intent['baseline_evidence'] = proof({'desired': desired()}, {'id': 1})
            intent['desired']['builder_image'] = 'registry.tilo.so/tilo/tilo-builder@sha256:' + 'f' * 64
            self.assertEqual(product.dispatch_inputs(intent)['baseline_sha'], '')
        with self.subTest(phase='same source new immutable builder dispatches full collection'):
            self.setUp()
            self.start()
            self.api.runs[1]['status'] = 'success'
            self.api.verifier = proof
            core.tick(self.store, self.api)
            updated = dict(desired(), builder_image='registry.tilo.so/tilo/tilo-builder@sha256:' + 'f' * 64)
            self.store.observe(2, updated)
            core.tick(self.store, self.api)
            self.assertEqual(len(self.api.calls), 2)
            request = self.api.calls[-1]
            self.assertEqual(request['ref'], desired()['source_sha'])
            self.assertEqual(request['inputs']['builder_image'], updated['builder_image'])
            self.assertEqual(request['inputs']['baseline_sha'], '')
            self.assertEqual(request['inputs']['baseline_evidence'], '')
        with self.subTest(phase='recovery collection never uses catalog wide base selection'):
            self.setUp()
            previous = dict(desired(), collection='recovery')
            intent = {'id': 'intent', 'desired': dict(desired('b'), collection='recovery'),
                      'baseline': previous, 'baseline_evidence': proof({'desired': previous}, {'id': 1})}
            inputs = product.dispatch_inputs(intent)
            self.assertEqual(inputs['collection'], 'recovery')
            self.assertEqual(inputs['baseline_sha'], '')
            self.assertEqual(inputs['baseline_evidence'], '')
        with self.subTest(phase='dispatch pins false default but plan provenance stays exact'):
            self.setUp()
            self.start()
            self.assertEqual(self.api.calls[0]['inputs']['verify_final_receipt'], 'false')
            intent = self.store.read()['active']
            run = self.api.runs[1]
            payload = json.loads(run['event_payload'])
            payload['inputs'] = {key: value for key, value in payload['inputs'].items() if value != ''}
            run.update(event_payload=json.dumps(payload), status='success')
            self.assertIsNone(self.api.no_build_evidence(intent, run))
            del self.api.documents[1]['inputs']['apps']
            with self.assertRaisesRegex(ValueError, 'plan provenance'):
                self.api.no_build_evidence(intent, run)
        with self.subTest(phase='unverified or incomplete baseline proof rejected'):
            self.setUp()
            intent = {'id': 'intent', 'desired': desired('b'), 'baseline': desired(), 'baseline_evidence': None}
            with self.assertRaises(ValueError):
                product.dispatch_inputs(intent)
            self.start()
            self.api.runs[1]['status'] = 'success'
            self.api.verifier = lambda i, r: dict(proof(i, r), receipts={'x86_64': 'e' * 64})
            with self.assertRaises(ValueError):
                core.tick(self.store, self.api)
            self.assertIsNone(self.store.read()['baseline'])
        with self.subTest(phase='disabled policy refuses before network'):
            self.setUp()
            api = product.ProductForgejo(dict(POLICY, enabled=False))
            api.call = lambda *_: self.fail('network called while disabled')
            with self.assertRaises(ValueError):
                api.qualify_receipt(desired())

    def test_zero_and_no_build_plans_need_trusted_current_provenance(self):
        with self.subTest(phase='zero bootstrap is handled without manufacturing baseline'):
            self.setUp()
            self.api.zero = True
            self.start()
            self.api.runs[1]['status'] = 'success'
            core.tick(self.store, self.api)
            state = self.store.read()
            self.assertIsNone(state['active'])
            self.assertIsNone(state['baseline'])
            self.assertIsNone(state.get('baseline_evidence'))
            self.assertEqual(state['handled'], desired())
            self.assertEqual(state['last_attempt']['outcome'], 'verified-no-build')
            self.assertEqual(core.next_action(state), 'idle')
            reopened = core.Store(self.store.path)
            try:
                self.assertEqual(core.next_action(reopened.read()), 'idle')
            finally:
                reopened.db.close()
            core.tick(self.store, self.api)
            self.assertEqual(len(self.api.calls), 1)
            self.store.observe(2, desired('b'))
            core.tick(self.store, self.api)
            self.assertEqual(self.api.calls[-1]['inputs']['baseline_sha'], '')
        with self.subTest(phase='zero plan preserves verified baseline and next pending uses it'):
            self.setUp()
            self.start()
            self.api.runs[1]['status'] = 'success'
            self.api.verifier = proof
            core.tick(self.store, self.api)
            baseline = self.store.read()['baseline']
            evidence = self.store.read()['baseline_evidence']
            self.api.zero = True
            self.store.observe(2, desired('b'))
            core.tick(self.store, self.api)
            self.store.observe(3, desired('c'))
            self.api.runs[2]['status'] = 'success'
            self.api.verifier = lambda *_: self.fail('zero plan must not require native package verification')
            core.tick(self.store, self.api)
            state = self.store.read()
            self.assertEqual(state['baseline'], baseline)
            self.assertEqual(state['baseline_evidence'], evidence)
            self.assertEqual(state['handled'], desired('b'))
            self.assertEqual(core.next_action(state), 'dispatch')
            core.tick(self.store, self.api)
            self.assertEqual(self.api.calls[-1]['inputs']['baseline_sha'], baseline['source_sha'])
            self.assertEqual(self.api.calls[-1]['inputs']['baseline_run_id'], '1')
        with self.subTest(phase='artifact forged zero count cannot override trusted plan log'):
            self.setUp()
            self.start()
            trusted_log = self.api.raw_api('repos/tilo/tilo/actions/jobs/1/logs', 9999)
            self.api.documents[1]['plan'] = {'count': 0, 'apps': [], 'packages': [], 'catalog': [], 'features': []}
            self.api.raw_api = lambda *_: trusted_log
            self.api.runs[1]['status'] = 'success'
            with self.assertRaisesRegex(ValueError, 'trusted current job log'):
                core.tick(self.store, self.api)
            self.assertIsNotNone(self.store.read()['active'])
            self.assertIsNone(self.store.read()['baseline'])
        with self.subTest(phase='no build rejects mismatched provenance even with matching log digest'):
            self.setUp()
            import copy
            self.api.zero = True
            self.start()
            self.api.runs[1]['status'] = 'success'
            original = copy.deepcopy(self.api.documents[1])
            changes = [('source_sha', 'f' * 40), ('builder_image', 'different-builder'),
                       ('run_id', 123), ('plan_attempt', 2)]
            for key, value in changes:
                self.api.documents[1] = dict(original, **{key: value})
                with self.subTest(key=key), self.assertRaisesRegex(ValueError, 'provenance'):
                    core.tick(self.store, self.api)
            for key in ('coordinator_intent', 'baseline_sha', 'collection', 'architectures'):
                self.api.documents[1] = copy.deepcopy(original)
                self.api.documents[1]['inputs'][key] = 'forged'
                with self.subTest(input=key), self.assertRaisesRegex(ValueError, 'provenance'):
                    core.tick(self.store, self.api)
            self.assertIsNotNone(self.store.read()['active'])
        with self.subTest(phase='no build cannot use stale plan attempt'):
            self.setUp()
            self.api.zero = True
            self.start()
            self.api.runs[1]['status'] = 'success'
            original_call = self.api.call
            def rerun(path, data=None):
                result = original_call(path, data)
                if path.endswith('/jobs'):
                    result[0].update(attempt=2, task_id=20, handle='new')
                return result
            self.api.call = rerun
            with self.assertRaisesRegex(ValueError, 'artifact missing'):
                core.tick(self.store, self.api)
        with self.subTest(phase='success with skipped build is not a no build proof'):
            self.setUp()
            self.api.zero = True
            self.start()
            self.api.runs[1]['status'] = 'success'
            original = self.api.call
            self.api.call = lambda path, data=None: ([{'id': 1, 'name': 'build', 'status': 'skipped'}]
                                                    if path.endswith('/jobs') else original(path, data))
            with self.assertRaisesRegex(ValueError, 'trusted plan job'):
                core.tick(self.store, self.api)
            self.assertIsNotNone(self.store.read()['active'])
            self.assertIsNone(self.store.read().get('handled'))
        with self.subTest(phase='no build rechecks trusted source and current jobs'):
            self.setUp()
            self.api.zero = True
            self.start()
            self.api.runs[1]['status'] = 'success'
            self.api.qualify_receipt = lambda *_: (_ for _ in ()).throw(ValueError('untrusted planner'))
            with self.assertRaisesRegex(ValueError, 'untrusted planner'):
                core.tick(self.store, self.api)
            self.api.qualify_receipt = product.validate_desired
            original = self.api.call
            reads = []
            def changing(path, data=None):
                result = original(path, data)
                if path.endswith('/jobs'):
                    reads.append(path)
                    if len(reads) > 1:
                        result[0]['handle'] = 'replacement'
                return result
            self.api.call = changing
            with self.assertRaisesRegex(ValueError, 'changed during plan verification'):
                core.tick(self.store, self.api)
            self.assertIsNone(self.store.read()['baseline'])

    def test_runs_advance_the_baseline_only_after_exact_verified_acceptance(self):
        with self.subTest(phase='green run does not advance without canonical verification'):
            self.setUp()
            self.start()
            self.api.runs[1]['status'] = 'success'
            self.store.observe(2, desired('b'))
            core.tick(self.store, self.api)
            state = self.store.read()
            self.assertEqual(state['active']['status'], 'awaiting-verification')
            self.assertIsNone(state['baseline'])
            core.tick(self.store, self.api)
            self.assertEqual(len(self.api.calls), 1)
        with self.subTest(phase='verified success baseline and latest pending'):
            self.setUp()
            self.start()
            self.store.observe(2, desired('b'))
            self.store.observe(3, desired('c'))
            self.api.runs[1]['status'] = 'success'
            self.api.verifier = proof
            core.tick(self.store, self.api)
            state = self.store.read()
            self.assertEqual(state['baseline'], desired())
            self.assertEqual(state['baseline_evidence']['run_id'], 1)
            core.tick(self.store, self.api)
            request = self.api.calls[-1]
            self.assertEqual(request['ref'], 'c' * 40)
            self.assertEqual(request['inputs']['baseline_sha'], 'a' * 40)
            self.assertEqual(request['inputs']['baseline_run_id'], '1')
            self.assertEqual(request['inputs']['publish'], 'false')
            self.assertEqual(len(request['inputs']['baseline_receipt_sha256']), 64)
        with self.subTest(phase='run identity and publication mismatch rejected'):
            self.setUp()
            self.start()
            intent = self.store.read()['active']
            for key, value in [('commit_sha', 'f' * 40), ('workflow_id', 'other.yml'), ('trigger_event', 'push')]:
                with self.assertRaises(ValueError):
                    self.api.validate_run(intent, dict(self.api.runs[1], **{key: value}))
            run = dict(self.api.runs[1])
            payload = json.loads(run['event_payload'])
            payload['inputs']['publish'] = 'true'
            run['event_payload'] = json.dumps(payload)
            with self.assertRaises(ValueError):
                self.api.validate_run(intent, run)
        with self.subTest(phase='unknown dispatch never reposts and reconciles exact run'):
            self.setUp()
            self.store.observe(1, desired())
            self.api.uncertain = True
            with self.assertRaises(TimeoutError):
                core.tick(self.store, self.api)
            core.tick(self.store, self.api)
            self.assertEqual(len(self.api.calls), 1)
            state = self.store.read()
            self.assertEqual(core.next_action(state), 'reconcile-required')
            core.accept_run(self.store, state['active']['id'], self.api.runs[1], self.api.validate_run)
            self.assertEqual(self.store.read()['active']['run_id'], 1)
        with self.subTest(phase='unverified success cannot rebind known run id'):
            self.setUp()
            self.start()
            intent = self.store.read()['active']
            other = dict(self.api.runs[1], id=2, status='success')
            with self.assertRaisesRegex(ValueError, 'conflicts'):
                core.accept_run(self.store, intent['id'], other, self.api.validate_run)
            self.assertEqual(self.store.read()['active']['run_id'], 1)
        with self.subTest(phase='concurrent reconciliation cannot overwrite attached run'):
            self.setUp()
            self.start()
            def competing_accept(*args):
                with self.store.transaction() as state:
                    state['active']['run_id'] = 2
                raise core.VerificationRequired()
            with patch.object(core, 'accept_run', side_effect=competing_accept):
                with self.assertRaisesRegex(ValueError, 'concurrent'):
                    product.reconcile(self.store, self.api, 1)
            self.assertEqual(self.store.read()['active']['run_id'], 2)
        with self.subTest(phase='changed policy cannot accept old active run'):
            self.setUp()
            self.start()
            self.api.policy = dict(POLICY, enabled=False)
            with self.assertRaisesRegex(ValueError, 'policy changed or disabled'):
                core.tick(self.store, self.api)
            self.assertIsNone(self.store.read()['baseline'])
        with self.subTest(phase='cli reconcile routes to helper and retains unverified success'):
            self.setUp()
            self.start()
            self.api.runs[1]['status'] = 'success'
            policy = Path(self.temp.name) / 'policy.json'
            policy.write_text(json.dumps(POLICY))
            argv = ['product', '--state', str(self.store.path), '--policy', str(policy),
                    '--apply-qualification', 'reconcile', '--run-id', '1']
            with patch.object(sys, 'argv', argv), patch.object(product, 'ProductForgejo', return_value=self.api), \
                    patch('builtins.print'):
                product.main()
            self.assertEqual(self.store.read()['active']['status'], 'awaiting-verification')

    def test_qualification_requires_exact_trusted_workflow_and_helper_bytes(self):
        with self.subTest(phase='qualification requires exact trusted workflow bytes'):
            self.setUp()
            import base64
            import hashlib
            content = b'[toolchain]\nchannel="1.97.1"\n'
            policy = dict(POLICY, trusted_files={path: hashlib.sha256(content).hexdigest()
                                                for path in product.REQUIRED_PATHS})
            api = product.ProductForgejo(policy)
            receipt = dict(desired(), policy_sha256=api.policy_sha)
            api.call = lambda path: ({'id': 23, 'full_name': product.REPO} if path == 'repos/' + product.REPO
                                     else cargo_listing(path) or {'type': 'file', 'content': base64.b64encode(content).decode()})
            api.qualify_receipt(receipt)
            api.call = lambda path: ({'id': 23, 'full_name': product.REPO} if path == 'repos/' + product.REPO
                                     else cargo_listing(path) or {'type': 'file', 'content': base64.b64encode(b'unreviewed').decode()})
            with self.assertRaises(ValueError):
                api.qualify_receipt(receipt)
        with self.subTest(phase='release trust tracks shared graph not check scheduling'):
            self.setUp()
            import base64
            import hashlib
            content = b'[toolchain]\nchannel="1.97.1"\n'
            self.assertNotIn('tools/ci/affected_packages.py', product.REQUIRED_PATHS)
            self.assertIn('tools/ci/workspace_graph.py', product.REQUIRED_PATHS)
            policy = dict(POLICY, trusted_files={path: hashlib.sha256(content).hexdigest()
                                                for path in product.REQUIRED_PATHS})
            api = product.ProductForgejo(policy)
            seen = []
            def read(path):
                seen.append(path)
                if path == 'repos/' + product.REPO:
                    return {'id': 23, 'full_name': product.REPO}
                return cargo_listing(path) or {'type': 'file', 'content': base64.b64encode(content).decode()}
            api.call = read
            api.qualify_receipt(dict(desired(), policy_sha256=api.policy_sha))
            self.assertTrue(any('/contents/tools/ci/workspace_graph.py?' in path for path in seen))
            self.assertFalse(any('/contents/tools/ci/affected_packages.py?' in path for path in seen))
        with self.subTest(phase='qualification rejects missing or changed assembly helpers'):
            self.setUp()
            import base64
            import hashlib
            content = b'[toolchain]\nchannel="1.97.1"\n'
            files = {path: hashlib.sha256(content).hexdigest() for path in product.REQUIRED_PATHS}
            for helper in ('tools/assemble_verified_apps.py', 'tools/apps/final_app_receipt.py',
                           'tools/apps/app_channel_manifest.py', 'dist/apps/assemble-collection.sh',
                           'tools/apps/assemble_app_collection.py', 'tools/ci/workspace_graph.py',
                           'tools/ci/__init__.py', 'tools/ci/resolve_builder.py', 'tools/ci/python.py'):
                with self.subTest(helper=helper):
                    self.assertIn(helper, files)
                    incomplete = dict(files)
                    del incomplete[helper]
                    api = product.ProductForgejo(dict(POLICY, trusted_files=incomplete))
                    api.call = lambda *_: self.fail('incomplete trust policy reached network')
                    with self.assertRaises(ValueError):
                        api.qualify_receipt(dict(desired(), policy_sha256=api.policy_sha))
                    api = product.ProductForgejo(dict(POLICY, trusted_files=files))
                    def read(path):
                        if path == 'repos/' + product.REPO:
                            return {'id': 23, 'full_name': product.REPO}
                        if cargo_listing(path) is not None:
                            return cargo_listing(path)
                        value = b'changed-helper' if '/contents/' + helper + '?' in path else content
                        return {'type': 'file', 'content': base64.b64encode(value).decode()}
                    api.call = read
                    with self.assertRaisesRegex(ValueError, 'differs from trusted policy'):
                        api.qualify_receipt(dict(desired(), policy_sha256=api.policy_sha))
        with self.subTest(phase='read only cli does not create store'):
            self.setUp()
            path = Path(self.temp.name)
            policy = path / 'policy.json'
            policy.write_text(json.dumps(POLICY))
            result = subprocess.run([sys.executable, product.__file__, '--state', str(path / 'absent.db'),
                                     '--policy', str(policy), 'tick'], capture_output=True, text=True, check=True)
            self.assertTrue(json.loads(result.stdout)['dry_run'])
            self.assertFalse((path / 'absent.db').exists())


class WorkflowGuardTests(unittest.TestCase):
    def env(self):
        return {'COORDINATOR_INTENT': 'a9eb3ab4-4321-4321-8321-123456789abc',
                'COORDINATOR_ENABLED': 'true', 'GITHUB_EVENT_NAME': 'workflow_dispatch',
                'GITHUB_SHA': 'a' * 40, 'GITHUB_REF': 'a' * 40,
                'COORDINATOR_POLICY_SHA256': desired()['policy_sha256'],
                'REQUESTED_COLLECTION': 'default', 'REQUESTED_ARCHITECTURES': 'x86_64,aarch64',
                'REQUESTED_PUBLISH': 'false', 'REQUESTED_BUILDER': desired()['builder_image']}

    def test_workflow_guard_accepts_only_pinned_intended_dispatches(self):
        with self.subTest(phase='first build and verified ancestor'):
            env = self.env()
            self.assertTrue(guard.prevalidate(env))
            self.assertTrue(guard.validate(env))
            previous = proof({'desired': desired('b')}, {'id': 123})
            env.update(BASELINE_SHA='b' * 40, BASELINE_RECEIPT_SHA256=core.receipt_key(previous),
                       BASELINE_RUN_ID='123', BASELINE_EVIDENCE=core.canonical(previous))
            self.assertTrue(guard.validate(env, lambda base, source: True))
            with self.assertRaises(ValueError):
                guard.validate(env, lambda base, source: False)
            env['BASELINE_EVIDENCE'] += ' '
            with self.assertRaisesRegex(ValueError, 'hash mismatch'):
                guard.validate(env, lambda base, source: True)
        with self.subTest(phase='disabled branch publish and unpinned builder rejected'):
            for key, value in [('COORDINATOR_ENABLED', ''), ('REQUESTED_PUBLISH', 'true'),
                               ('GITHUB_REF', 'refs/heads/main'), ('REQUESTED_BUILDER', 'latest'),
                               ('BASELINE_SHA', 'b' * 40), ('REQUESTED_APPS', 'terminal')]:
                with self.subTest(key=key), self.assertRaises(ValueError):
                    guard.validate(dict(self.env(), **{key: value}))
        with self.subTest(phase='ordinary workflow unchanged but extra inputs require intent'):
            self.assertFalse(guard.validate({}))
            with self.assertRaises(ValueError):
                guard.validate({'REQUESTED_BUILDER': desired()['builder_image']})


if __name__ == '__main__':
    unittest.main()

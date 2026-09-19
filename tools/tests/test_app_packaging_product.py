import json
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


POLICY = {'enabled': True, 'qualification_only': True, 'trusted_files': {}}


def desired(char='a'):
    return {'source_sha': char * 40, 'builder_image': 'code.podesta.ai/qos/tilo-builder@sha256:' + 'd' * 64,
            'collection': 'default', 'architectures': ['x86_64', 'aarch64'],
            'policy_sha256': core.receipt_key(POLICY)}


def proof(intent, run):
    return {'schema': 'tilo.coordinator-verification/v1', 'run_id': run['id'],
            'desired': intent['desired'], 'backend': 'canonical-final-artifact-v1',
            'receipts': {arch: 'e' * 64 for arch in intent['desired']['architectures']}}


class API(product.ProductForgejo):
    def __init__(self):
        super().__init__(POLICY)
        self.runs, self.calls = {}, []
        self.uncertain = False

    def qualify_receipt(self, receipt):
        product.validate_desired(receipt)

    def call(self, path, data=None):
        if data is None:
            return self.runs[int(path.rsplit('/', 1)[1])]
        self.calls.append(data)
        rid = len(self.calls)
        self.runs[rid] = {'id': rid, 'repository': {'id': 23, 'full_name': 'qos/qos'},
                         'workflow_id': 'release-apps.yml', 'trigger_event': 'workflow_dispatch',
                         'commit_sha': data['ref'], 'event_payload': json.dumps({'ref': data['ref'], 'inputs': data['inputs']}),
                         'status': 'running'}
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

    def test_green_run_does_not_advance_without_canonical_verification(self):
        self.start()
        self.api.runs[1]['status'] = 'success'
        self.store.observe(2, desired('b'))
        core.tick(self.store, self.api)
        state = self.store.read()
        self.assertEqual(state['active']['status'], 'awaiting-verification')
        self.assertIsNone(state['baseline'])
        core.tick(self.store, self.api)
        self.assertEqual(len(self.api.calls), 1)

    def test_verified_success_baseline_and_latest_pending(self):
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

    def test_builder_change_forces_full_collection(self):
        intent = {'id': 'intent', 'desired': desired('b'), 'baseline': desired(), 'baseline_evidence': None}
        intent['baseline_evidence'] = proof({'desired': desired()}, {'id': 1})
        intent['desired']['builder_image'] = 'code.podesta.ai/qos/tilo-builder@sha256:' + 'f' * 64
        self.assertEqual(product.dispatch_inputs(intent)['baseline_sha'], '')

    def test_same_source_new_immutable_builder_dispatches_full_collection(self):
        self.start()
        self.api.runs[1]['status'] = 'success'
        self.api.verifier = proof
        core.tick(self.store, self.api)
        updated = dict(desired(), builder_image='code.podesta.ai/qos/tilo-builder@sha256:' + 'f' * 64)
        self.store.observe(2, updated)
        core.tick(self.store, self.api)
        self.assertEqual(len(self.api.calls), 2)
        request = self.api.calls[-1]
        self.assertEqual(request['ref'], desired()['source_sha'])
        self.assertEqual(request['inputs']['builder_image'], updated['builder_image'])
        self.assertEqual(request['inputs']['baseline_sha'], '')
        self.assertEqual(request['inputs']['baseline_evidence'], '')

    def test_recovery_collection_never_uses_catalog_wide_base_selection(self):
        previous = dict(desired(), collection='recovery')
        intent = {'id': 'intent', 'desired': dict(desired('b'), collection='recovery'),
                  'baseline': previous, 'baseline_evidence': proof({'desired': previous}, {'id': 1})}
        inputs = product.dispatch_inputs(intent)
        self.assertEqual(inputs['collection'], 'recovery')
        self.assertEqual(inputs['baseline_sha'], '')
        self.assertEqual(inputs['baseline_evidence'], '')

    def test_run_identity_and_publication_mismatch_rejected(self):
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

    def test_unknown_dispatch_never_reposts_and_reconciles_exact_run(self):
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

    def test_unverified_or_incomplete_baseline_proof_rejected(self):
        intent = {'id': 'intent', 'desired': desired('b'), 'baseline': desired(), 'baseline_evidence': None}
        with self.assertRaises(ValueError):
            product.dispatch_inputs(intent)
        self.start()
        self.api.runs[1]['status'] = 'success'
        self.api.verifier = lambda i, r: dict(proof(i, r), receipts={'x86_64': 'e' * 64})
        with self.assertRaises(ValueError):
            core.tick(self.store, self.api)
        self.assertIsNone(self.store.read()['baseline'])

    def test_read_only_cli_does_not_create_store(self):
        path = Path(self.temp.name)
        policy = path / 'policy.json'
        policy.write_text(json.dumps(POLICY))
        result = subprocess.run([sys.executable, product.__file__, '--state', str(path / 'absent.db'),
                                 '--policy', str(policy), 'tick'], capture_output=True, text=True, check=True)
        self.assertTrue(json.loads(result.stdout)['dry_run'])
        self.assertFalse((path / 'absent.db').exists())

    def test_disabled_policy_refuses_before_network(self):
        api = product.ProductForgejo(dict(POLICY, enabled=False))
        api.call = lambda *_: self.fail('network called while disabled')
        with self.assertRaises(ValueError):
            api.qualify_receipt(desired())

    def test_qualification_requires_exact_trusted_workflow_bytes(self):
        import base64
        import hashlib
        content = b'trusted-reviewed-content'
        policy = dict(POLICY, trusted_files={path: hashlib.sha256(content).hexdigest()
                                            for path in product.REQUIRED_PATHS})
        api = product.ProductForgejo(policy)
        receipt = dict(desired(), policy_sha256=api.policy_sha)
        api.call = lambda path: ({'id': 23, 'full_name': product.REPO} if path == 'repos/' + product.REPO
                                 else {'content': base64.b64encode(content).decode()})
        api.qualify_receipt(receipt)
        api.call = lambda path: ({'id': 23, 'full_name': product.REPO} if path == 'repos/' + product.REPO
                                 else {'content': base64.b64encode(b'unreviewed').decode()})
        with self.assertRaises(ValueError):
            api.qualify_receipt(receipt)

    def test_unverified_success_cannot_rebind_known_run_id(self):
        self.start()
        intent = self.store.read()['active']
        other = dict(self.api.runs[1], id=2, status='success')
        with self.assertRaisesRegex(ValueError, 'conflicts'):
            core.accept_run(self.store, intent['id'], other, self.api.validate_run)
        self.assertEqual(self.store.read()['active']['run_id'], 1)

    def test_qualification_rejects_missing_or_changed_assembly_helpers(self):
        import base64
        import hashlib
        content = b'reviewed-helper'
        files = {path: hashlib.sha256(content).hexdigest() for path in product.REQUIRED_PATHS}
        for helper in ('tools/assemble_verified_apps.py', 'tools/final_app_receipt.py',
                       'tools/app_channel_manifest.py', 'dist/apps/assemble-collection.sh',
                       'tools/assemble_app_collection.py', 'tools/ci_select.py'):
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
                    value = b'changed-helper' if '/contents/' + helper + '?' in path else content
                    return {'content': base64.b64encode(value).decode()}
                api.call = read
                with self.assertRaises(ValueError):
                    api.qualify_receipt(dict(desired(), policy_sha256=api.policy_sha))

    def test_concurrent_reconciliation_cannot_overwrite_attached_run(self):
        self.start()
        def competing_accept(*args):
            with self.store.transaction() as state:
                state['active']['run_id'] = 2
            raise core.VerificationRequired()
        with patch.object(core, 'accept_run', side_effect=competing_accept):
            with self.assertRaisesRegex(ValueError, 'concurrent'):
                product.reconcile(self.store, self.api, 1)
        self.assertEqual(self.store.read()['active']['run_id'], 2)

    def test_changed_policy_cannot_accept_old_active_run(self):
        self.start()
        self.api.policy = dict(POLICY, enabled=False)
        with self.assertRaisesRegex(ValueError, 'policy changed or disabled'):
            core.tick(self.store, self.api)
        self.assertIsNone(self.store.read()['baseline'])

    def test_cli_reconcile_routes_to_helper_and_retains_unverified_success(self):
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


class WorkflowGuardTests(unittest.TestCase):
    def env(self):
        return {'COORDINATOR_INTENT': 'a9eb3ab4-4321-4321-8321-123456789abc',
                'COORDINATOR_ENABLED': 'true', 'GITHUB_EVENT_NAME': 'workflow_dispatch',
                'GITHUB_SHA': 'a' * 40, 'GITHUB_REF': 'a' * 40,
                'COORDINATOR_POLICY_SHA256': desired()['policy_sha256'],
                'REQUESTED_COLLECTION': 'default', 'REQUESTED_ARCHITECTURES': 'x86_64,aarch64',
                'REQUESTED_PUBLISH': 'false', 'REQUESTED_BUILDER': desired()['builder_image']}

    def test_first_build_and_verified_ancestor(self):
        env = self.env()
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

    def test_disabled_branch_publish_and_unpinned_builder_rejected(self):
        for key, value in [('COORDINATOR_ENABLED', ''), ('REQUESTED_PUBLISH', 'true'),
                           ('GITHUB_REF', 'refs/heads/main'), ('REQUESTED_BUILDER', 'latest'),
                           ('BASELINE_SHA', 'b' * 40), ('REQUESTED_APPS', 'terminal')]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                guard.validate(dict(self.env(), **{key: value}))

    def test_ordinary_workflow_unchanged_but_extra_inputs_require_intent(self):
        self.assertFalse(guard.validate({}))
        with self.assertRaises(ValueError):
            guard.validate({'REQUESTED_BUILDER': desired()['builder_image']})


if __name__ == '__main__':
    unittest.main()

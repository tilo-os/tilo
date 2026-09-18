import importlib.util
import json
import base64
from pathlib import Path
import tempfile
import subprocess
import sys
import unittest

SPEC = importlib.util.spec_from_file_location('coordinator', Path(__file__).parents[1] / 'app_packaging_coordinator.py')
c = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(c)
A, B, C = ({'source_sha': digit * 40} for digit in ('a', 'b', 'c'))


class API:
    def __init__(self):
        self.runs, self.dispatched = {}, []
        self.uncertain = False
        self.on_dispatch = lambda: None

    def qualify(self, sha):
        pass

    def dispatch(self, intent):
        self.dispatched.append(intent)
        run_id = len(self.dispatched)
        self.runs[run_id] = {
            'id': run_id, 'repository': {'full_name': c.REPO}, 'workflow_id': c.WORKFLOW,
            'trigger_event': 'workflow_dispatch', 'event': '', 'status': 'running',
            'commit_sha': intent['desired']['source_sha'],
            'event_payload': json.dumps({'ref': intent['desired']['source_sha'], 'inputs': {
                'coordinator_intent': intent['id'], 'baseline_sha': c.baseline_sha(intent)}})}
        self.on_dispatch()
        if self.uncertain:
            raise TimeoutError('response lost after accepted dispatch')
        return {'id': run_id}

    def run(self, run_id):
        return self.runs[run_id]


class CoordinatorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / 'state.sqlite'
        self.store = c.Store(self.path, writable=True)
        self.api = API()

    def tearDown(self):
        self.store.db.close()
        self.temp.cleanup()

    def test_latest_pending_preserves_active_and_successful_baseline(self):
        self.store.observe(1, A)
        c.tick(self.store, self.api)
        self.store.observe(2, B)
        self.store.observe(3, C)
        c.tick(self.store, self.api)
        self.assertEqual(len(self.api.dispatched), 1)
        self.assertEqual(self.store.read()['active']['desired'], A)
        self.assertIsNone(self.store.read()['baseline'])
        self.api.runs[1]['status'] = 'success'
        c.tick(self.store, self.api)
        c.tick(self.store, self.api)
        self.assertEqual([x['desired'] for x in self.api.dispatched], [A, C])
        self.assertEqual(self.api.dispatched[1]['baseline'], A)

    def test_failure_never_becomes_baseline_or_retries_same_source(self):
        self.store.observe(1, A)
        c.tick(self.store, self.api)
        self.api.runs[1]['status'] = 'failure'
        c.tick(self.store, self.api)
        self.store.observe(2, A)
        for _ in range(4):
            c.tick(self.store, self.api)
        self.assertEqual(len(self.api.dispatched), 1)
        self.assertIsNone(self.store.read()['baseline'])
        self.assertEqual(c.next_action(self.store.read()), 'failed-source-blocked')
        self.store.observe(3, B)
        c.tick(self.store, self.api)
        self.assertIsNone(self.api.dispatched[1]['baseline'])
        self.assertEqual(self.store.db.execute('SELECT count(*) FROM attempts').fetchone()[0], 2)

    def test_uncertain_dispatch_survives_restart_and_reconciles_without_retry(self):
        self.store.observe(1, A)
        self.api.uncertain = True
        with self.assertRaises(TimeoutError):
            c.tick(self.store, self.api)
        intent = self.store.read()['active']['id']
        self.store.db.close()
        self.store = c.Store(self.path, writable=True)
        self.store.observe(2, B)
        c.tick(self.store, self.api)
        self.assertEqual(len(self.api.dispatched), 1)
        self.assertEqual(c.next_action(self.store.read()), 'reconcile-required')
        self.api.runs[1]['status'] = 'success'
        c.accept_run(self.store, intent, self.api.run(1))
        self.assertEqual(self.store.read()['baseline'], A)
        self.assertEqual(self.store.read()['desired'], B)

    def test_concurrent_worker_cannot_dispatch_committed_active_intent(self):
        self.store.observe(1, A)
        other = c.Store(self.path, writable=True)
        try:
            self.api.on_dispatch = lambda: c.tick(other, self.api)
            c.tick(self.store, self.api)
        finally:
            other.db.close()
        self.assertEqual(len(self.api.dispatched), 1)

    def test_stale_observation_and_unrelated_manual_run_rejected(self):
        self.store.observe(2, B)
        with self.assertRaises(ValueError):
            self.store.observe(1, A)
        with self.assertRaises(ValueError):
            self.store.observe(2, A)
        c.tick(self.store, self.api)
        intent = self.store.read()['active']['id']
        for field, value in [('trigger_event', 'push'), ('commit_sha', A['source_sha']),
                             ('event_payload', json.dumps({'inputs': {}})), ('workflow_id', 'release-apps.yml')]:
            with self.subTest(field=field), self.assertRaises(ValueError):
                c.accept_run(self.store, intent, dict(self.api.run(1), **{field: value}))
        self.assertEqual(self.store.read()['active']['status'], 'running')

    def test_receipt_advance_during_qualification_is_rechecked(self):
        self.store.observe(1, A)
        self.api.qualify = lambda sha: self.store.observe(2, B)
        c.tick(self.store, self.api)
        self.assertEqual(self.api.dispatched, [])

    def test_crash_after_intent_before_post_stays_blocked(self):
        self.store.observe(1, A)
        self.api.dispatch = lambda intent: (_ for _ in ()).throw(KeyboardInterrupt())
        with self.assertRaises(KeyboardInterrupt):
            c.tick(self.store, self.api)
        self.assertEqual(c.next_action(self.store.read()), 'reconcile-required')
        self.assertIsNotNone(self.store.read()['active'])

    def test_dry_read_does_not_create_state(self):
        path = Path(self.temp.name) / 'nonexistent.sqlite'
        store = c.Store(path)
        self.assertEqual(c.next_action(store.read()), 'idle')
        self.assertFalse(path.exists())

    def test_cli_default_does_not_create_database_or_dispatch(self):
        path = Path(self.temp.name) / 'dry.sqlite'
        completed = subprocess.run([sys.executable, str(Path(c.__file__)), '--state', str(path), 'tick'],
                                   capture_output=True, text=True, check=True)
        self.assertTrue(json.loads(completed.stdout)['dry_run'])
        self.assertFalse(path.exists())

    def test_fixture_gate_rejects_public_repository_and_modified_workflow(self):
        api = c.Forgejo()
        api.call = lambda path: {'full_name': c.REPO, 'private': False}
        with self.assertRaisesRegex(ValueError, 'privacy mismatch'):
            api.qualify(A['source_sha'])
        api.call = lambda path: ({'full_name': c.REPO, 'private': True} if path == 'repos/' + c.REPO
                                 else {'content': base64.b64encode(b'changed').decode()})
        with self.assertRaisesRegex(ValueError, 'exact nonpublishing'):
            api.qualify(A['source_sha'])
        api.call = lambda path: ({'full_name': c.REPO, 'private': True} if path == 'repos/' + c.REPO
                                 else {'content': base64.b64encode(c.FIXTURE.read_text().replace('\n', '\r\n').encode()).decode()})
        api.qualify(A['source_sha'])


if __name__ == '__main__':
    unittest.main()

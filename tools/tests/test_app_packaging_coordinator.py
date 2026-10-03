import importlib.util
import json
import base64
from pathlib import Path
import tempfile
import subprocess
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
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
        # Merged tests reset this per phase with doCleanups() + setUp().
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / 'state.sqlite'
        self.store = c.Store(self.path, writable=True)
        # Close whichever store is current, as tearDown did (a phase may reopen it).
        self.addCleanup(lambda: self.store.db.close())
        self.api = API()

    def test_coordinator_state_machine_dispatches_once_and_advances_only_verified_baselines(self):
        with self.subTest(phase='latest pending preserves active and successful baseline'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='failure never becomes baseline or retries same source'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='artifact verification does not block new observations'):
            self.doCleanups()
            self.setUp()
            self.store.observe(1, A)
            c.tick(self.store, self.api)
            self.api.runs[1]['status'] = 'success'
            other = c.Store(self.path, writable=True)
            other.db.execute('PRAGMA busy_timeout=25')
            try:
                def verify(intent, run):
                    other.observe(2, B)
                    c.validate_run(intent, run)
                    return {'verified': True}
                c.accept_run(self.store, self.store.read()['active']['id'], self.api.run(1), verify)
            finally:
                other.db.close()
            self.assertEqual(self.store.read()['desired'], B)
            self.assertEqual(self.store.read()['baseline'], A)
            self.assertEqual(self.store.read()['baseline_evidence'], {'verified': True})
        with self.subTest(phase='changed active intent rejects verification result'):
            self.doCleanups()
            self.setUp()
            self.store.observe(1, A)
            c.tick(self.store, self.api)
            self.api.runs[1]['status'] = 'success'
            other = c.Store(self.path, writable=True)
            other.db.execute('PRAGMA busy_timeout=25')
            try:
                def verify(intent, run):
                    with other.transaction() as state:
                        state['active']['run_id'] = 99
                    return {'verified': True}
                with self.assertRaisesRegex(ValueError, 'changed during verification'):
                    c.accept_run(self.store, self.store.read()['active']['id'], self.api.run(1), verify)
            finally:
                other.db.close()
            self.assertIsNone(self.store.read()['baseline'])
            self.assertEqual(self.store.read()['active']['run_id'], 99)
        with self.subTest(phase='uncertain dispatch survives restart and reconciles without retry'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='concurrent worker cannot dispatch committed active intent'):
            self.doCleanups()
            self.setUp()
            self.store.observe(1, A)
            other = c.Store(self.path, writable=True)
            try:
                self.api.on_dispatch = lambda: c.tick(other, self.api)
                c.tick(self.store, self.api)
            finally:
                other.db.close()
            self.assertEqual(len(self.api.dispatched), 1)
        with self.subTest(phase='stale observation and unrelated manual run rejected'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='receipt advance during qualification is rechecked'):
            self.doCleanups()
            self.setUp()
            self.store.observe(1, A)
            self.api.qualify = lambda sha: self.store.observe(2, B)
            c.tick(self.store, self.api)
            self.assertEqual(self.api.dispatched, [])
        with self.subTest(phase='crash after intent before post stays blocked'):
            self.doCleanups()
            self.setUp()
            self.store.observe(1, A)
            self.api.dispatch = lambda intent: (_ for _ in ()).throw(KeyboardInterrupt())
            with self.assertRaises(KeyboardInterrupt):
                c.tick(self.store, self.api)
            self.assertEqual(c.next_action(self.store.read()), 'reconcile-required')
            self.assertIsNotNone(self.store.read()['active'])

    def test_read_only_paths_and_fixture_gate_create_no_state_or_dispatch(self):
        with self.subTest(phase='dry read does not create state'):
            self.doCleanups()
            self.setUp()
            path = Path(self.temp.name) / 'nonexistent.sqlite'
            store = c.Store(path)
            self.assertEqual(c.next_action(store.read()), 'idle')
            self.assertFalse(path.exists())
        with self.subTest(phase='cli default does not create database or dispatch'):
            self.doCleanups()
            self.setUp()
            path = Path(self.temp.name) / 'dry.sqlite'
            completed = subprocess.run([sys.executable, str(Path(c.__file__)), '--state', str(path), 'tick'],
                                       capture_output=True, text=True, check=True)
            self.assertTrue(json.loads(completed.stdout)['dry_run'])
            self.assertFalse(path.exists())
        with self.subTest(phase='fixture gate rejects public repository and modified workflow'):
            self.doCleanups()
            self.setUp()
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

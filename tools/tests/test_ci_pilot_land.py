"""Exercise pilot landing on changing server evidence and uncertain responses."""
import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock
import urllib.error
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import ci_pilot_land as land
from test_ci_integration_readiness import fixture, HEAD


class LandingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / 'journal.json'
        self.policy, self.observation = fixture()
        self.observation['pr']['base']['repo']['full_name'] = land.REPO
        self.observation['runs'][0]['run']['repository']['full_name'] = land.REPO
        self.rule = dict(enable_push=False, apply_to_admins=True, enable_merge_whitelist=True,
            merge_whitelist_usernames=[land.INTEGRATOR], merge_whitelist_teams=[],
            dismiss_stale_approvals=True, block_on_outdated_branch=True,
            block_on_rejected_reviews=True, enable_approvals_whitelist=True,
            approvals_whitelist_teams=[], enable_status_check=True,
            status_check_contexts=['pilot/ready'], required_approvals=1,
            approvals_whitelist_username=['reviewer'])
        self.pr_reads = 0
        self.merge_calls = 0
        self.mutate = lambda: None
        self.merged = False

    def read(self, host, repo, suffix):
        self.assertEqual(repo, land.REPO)
        if suffix == '': return {'private': True, 'full_name': repo}
        if suffix.startswith('/collaborators/'):
            return {'permission': 'write', 'user': {'login': land.INTEGRATOR, 'id': 81}}
        if suffix == '/branch_protections/main': return copy.deepcopy(self.rule)
        if suffix == '/pulls/3':
            self.pr_reads += 1
            if self.pr_reads == 2: self.mutate()
            pr = copy.deepcopy(self.observation['pr'])
            if self.merged:
                pr.update(merged=True, merge_commit_sha=HEAD, merged_by={'login': land.INTEGRATOR})
            return pr
        if suffix == '/branches/main':
            return {'commit': {'id': HEAD}} if self.merged else copy.deepcopy(self.observation['base'])
        if '/reviews?' in suffix: return copy.deepcopy(self.observation['reviews'])
        if suffix.endswith('/jobs'): return copy.deepcopy(self.observation['runs'][0]['jobs'])
        if suffix == '/actions/runs/12': return copy.deepcopy(self.observation['runs'][0]['run'])
        self.fail('unexpected endpoint ' + suffix)

    def merge(self, number, head, credential):
        self.assertEqual(json.loads(self.path.read_text())['state'], 'merge_intent_do_not_retry')
        self.assertEqual((number, head), (3, HEAD))
        self.merge_calls += 1
        self.merged = True
        return 200

    def run_landing(self, execute=False, merge=None):
        return land.land(land.REPO, 3, Path(self.temp.name), self.policy, self.path,
            execute=execute, read=self.read, provenance=lambda *args: [], merge=merge or self.merge)

    def test_default_dry_run_never_merges(self):
        self.assertEqual(self.run_landing(), 0)
        self.assertEqual(self.merge_calls, 0)
        self.assertEqual(json.loads(self.path.read_text())['state'], 'dry_run_ready')

    def test_fresh_success_records_exact_actor_head(self):
        self.assertEqual(self.run_landing(True), 0)
        self.assertEqual(self.merge_calls, 1)
        self.assertEqual(json.loads(self.path.read_text())['state'], 'merged_verified')

    def test_changes_in_head_base_or_latest_attempt_reject(self):
        mutations = [lambda: self.observation['pr']['head'].update(sha='c'*40),
                     lambda: self.observation['base']['commit'].update(id='c'*40),
                     lambda: self.observation['runs'][0]['jobs'][0].update(attempt=2, handle='new', task_id=31),
                     lambda: self.observation['runs'][0]['jobs'][0].update(status='running')]
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                self.setUp()
                # Bind after resetting the observation used by the lambda.
                self.mutate = mutation
                self.assertEqual(self.run_landing(True), 2)
                self.assertEqual(self.merge_calls, 0)

    def test_forged_status_does_not_replace_failed_execution(self):
        self.observation['commit_status'] = 'success'
        self.observation['runs'][0]['run']['status'] = 'failure'
        self.assertEqual(self.run_landing(True), 2)
        self.assertEqual(self.merge_calls, 0)

    def test_unknown_response_reconciles_without_retry(self):
        def uncertain(*args):
            self.merge(*args)
            raise TimeoutError('transport lost')
        self.assertEqual(self.run_landing(True, uncertain), 0)
        self.assertEqual(self.merge_calls, 1)
        with self.assertRaises(FileExistsError): self.run_landing(True)
        self.assertEqual(self.merge_calls, 1)

    def test_unmerged_readback_after_timeout_remains_uncertain(self):
        def uncertain(*args):
            self.merge_calls += 1
            raise TimeoutError('transport lost')
        self.assertEqual(self.run_landing(True, uncertain), 3)
        self.assertEqual(self.merge_calls, 1)
        self.assertEqual(json.loads(self.path.read_text())['state'], 'not_verified_do_not_retry')

    def test_role_and_protection_changes_reject(self):
        for change in ['author', 'reviewer', 'whitelist', 'outdated']:
            with self.subTest(change=change):
                self.setUp()
                if change == 'author': self.observation['pr']['user']['login'] = land.INTEGRATOR
                elif change == 'reviewer': self.policy['reviewers'].append(land.INTEGRATOR)
                elif change == 'whitelist': self.rule['merge_whitelist_usernames'].append('author')
                else: self.rule['block_on_outdated_branch'] = False
                self.assertEqual(self.run_landing(True), 2)
                self.assertEqual(self.merge_calls, 0)

    def test_merge_wire_contract_and_redirect_rejection(self):
        credential = {'namespace': 'forgejo', 'name': 'qos-integrator-pilot',
                      'username': land.INTEGRATOR, 'token': 'test-only-token'}
        with mock.patch.object(land.urllib.request, 'build_opener') as factory:
            opener = factory.return_value
            opener.open.side_effect = urllib.error.HTTPError('https://code.podesta.ai', 302,
                                                              'redirect denied', {}, None)
            self.assertEqual(land.merge_request(3, HEAD, credential), 302)
            factory.assert_called_once()
            self.assertIsInstance(factory.call_args.args[0], land.NoRedirect)
            opener.open.assert_called_once()
            request = opener.open.call_args.args[0]
            self.assertEqual(request.full_url, f'https://{land.HOST}/api/v1/repos/{land.REPO}/pulls/3/merge')
            self.assertEqual(json.loads(request.data), {'Do': 'fast-forward-only',
                'head_commit_id': HEAD, 'force_merge': False, 'merge_when_checks_succeed': False})
        request = urllib.request.Request('https://code.podesta.ai', headers={'Authorization': 'token fixture'})
        for code in (301, 302, 303, 307, 308):
            with self.subTest(code=code), self.assertRaises(urllib.error.HTTPError) as caught:
                land.NoRedirect().redirect_request(request, None, code, 'redirect', {}, 'https://elsewhere.invalid')
            caught.exception.close()

    def test_product_repository_is_hard_denied(self):
        with self.assertRaises(ValueError):
            land.land('qos/qos', 3, Path(self.temp.name), self.policy, self.path,
                      execute=True, read=self.read, merge=self.merge)
        self.assertFalse(self.path.exists())


if __name__ == '__main__': unittest.main()

import importlib.util
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1]))
import ci_product_report as report


class ProductReportTests(unittest.TestCase):
    def read(self, host, repo, path):
        self.assertEqual((host, repo), (report.HOST, 'qos/qos'))
        if path == '': return {'id': 23, 'full_name': repo, 'private': True}
        if path == '/branches/main': return {'commit': {'id': 'a' * 40}}
        if path == '/branch_protections': return []
        if path.startswith('/collaborators/'): return {'permission': 'none', 'user': {}}
        if path.startswith('/pulls?'): return [dict(self.pr, id=1)]
        if path == '/pulls/1': return self.pr
        if path.startswith('/pulls/1/reviews?'): return []
        if path.startswith('/actions/runs?'): return {'total_count': 0, 'workflow_runs': []}
        self.fail(path)

    def setUp(self):
        self.pr = {'number': 1, 'head': {'sha': 'b' * 40}, 'base': {'ref': 'main'},
                   'state': 'open', 'title': 'candidate', 'user': {'login': 'author'}}

    def test_no_policy_never_invents_readiness(self):
        value = report.collect(read=self.read)
        self.assertFalse(value['merge_authorized'])
        self.assertFalse(value['pull_requests'][0]['audit']['observed_ready'])
        self.assertIn('blocked', report.markdown(value))

    def test_changed_main_invalidates_snapshot(self):
        count = 0
        def read(*args):
            nonlocal count
            if args[2] == '/branches/main':
                count += 1
                return {'commit': {'id': ('a' if count == 1 else 'c') * 40}}
            return self.read(*args)
        result = report.collect(read=read)
        self.assertFalse(result['stable_main'])
        self.assertIn('main advanced', result['pull_requests'][0]['audit']['errors'][-1])

    def test_ignored_head_filter_rejects_incomplete_evidence(self):
        def read(*args):
            if args[2].startswith('/actions/runs?'):
                return {'total_count': 1, 'workflow_runs': [{'id': 7, 'commit_sha': 'c' * 40}]}
            return self.read(*args)
        with self.assertRaisesRegex(ValueError, 'exact-head filter'):
            report.collect(read=read)

    def test_repeated_pagination_fails_instead_of_truncating(self):
        def read(*args):
            return {'total_count': 2, 'workflow_runs': [{'id': 1}]}
        with self.assertRaisesRegex(ValueError, 'pagination repeated'):
            report.pages(read, '/actions/runs', 'workflow_runs')

    def test_policy_cannot_choose_old_green_or_push_instead_of_latest_pr(self):
        runs = [{'id': 10, 'workflow_id': 'check.yml', 'event': 'pull_request'},
                {'id': 11, 'workflow_id': 'check.yml', 'event': 'pull_request'},
                {'id': 12, 'workflow_id': 'lint.yml', 'event': 'pull_request'}]
        policy = {'runs': [{'workflow': 'check.yml', 'run_id': 10, 'events': ['pull_request']},
                           {'workflow': 'lint.yml', 'run_id': 12, 'events': ['pull_request']}]}
        with patch.object(report.audit, 'policy_errors', return_value=[]), \
                patch.object(report.audit, 'collect') as collect:
            result = report.audit_candidate(1, policy, Path('.'), self.read, runs)
            self.assertFalse(result['observed_ready'])
            collect.assert_not_called()
            policy['runs'][0].update(run_id=11, events=['push'])
            self.assertFalse(report.audit_candidate(1, policy, Path('.'), self.read, runs)['observed_ready'])
            collect.assert_not_called()


if __name__ == '__main__':
    unittest.main()

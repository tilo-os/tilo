"""Product captures remain blocked; explicit applicability never excuses missing jobs."""
import copy
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

from test_ci_integration_readiness import AUDIT, fixture


class ApplicabilityTests(unittest.TestCase):
    def setUp(self):
        self.policy, self.observation = fixture()
        self.requirement = self.policy['runs'][0]
        self.requirement.update(jobs=['builder', 'select', 'security', 'checks',
                                      'affected (runner)', 'report-check-result'],
                                allowed_skips=['report-check-result'])
        template = self.observation['runs'][0]['jobs'][0]
        self.jobs = [dict(template, id=100+i, handle=f'current-{i}', name=name)
                     for i, name in enumerate(self.requirement['jobs'])]
        self.jobs[-1].update(status='skipped', task_id=0)
        self.run = self.observation['runs'][0]['run']

    def errors(self):
        return AUDIT.run_errors(self.run, self.jobs, 'qos/qos', self.run['commit_sha'], self.requirement)

    def test_explicit_pr_report_skip_preserves_required_jobs(self):
        self.assertEqual([], AUDIT.policy_errors(self.policy))
        self.assertEqual([], self.errors())
        for index in range(5):
            jobs = copy.deepcopy(self.jobs)
            self.jobs[index].update(status='skipped', task_id=0)
            self.assertTrue(self.errors())
            self.jobs = jobs
        self.jobs.pop(0)
        self.assertTrue(self.errors())

    def test_skip_not_valid_for_push_or_unknown_policy_job(self):
        self.run['event'] = 'push'
        self.assertTrue(self.errors())
        self.requirement['allowed_skips'].append('security')
        self.assertTrue(AUDIT.policy_errors(self.policy))

    def test_missing_zero_rust_proof_rejected(self):
        self.requirement['allowed_skips'].append('affected (runner)')
        self.assertTrue(AUDIT.policy_errors(self.policy))

    def test_actual_failed_pr_cannot_gain_readiness_from_report_skip(self):
        root = Path(__file__).parents[2] / 'experiments/ci-forgejo/results/product-readonly-20260919'
        captured = json.loads((root / 'pr-38.json').read_text())
        self.run = next(r for r in captured['runs']['workflow_runs'] if r['id'] == 16511)
        self.jobs = json.loads((root / 'run-16511-jobs.json').read_text())
        self.requirement['run_id'] = 16511
        errors = self.errors()
        self.assertIn('run not successful', errors)
        self.assertIn('job set differs from trusted expectation', errors)
        self.assertFalse(any("'report-check-result' not successful" in e for e in errors))

    def test_zero_rust_proof_runs_only_committed_clean_base_selector(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*args):
                return subprocess.check_output(['git', '-C', directory, *args], text=True).strip()
            git('init', '-q'); git('config', 'user.name', 'Fixture'); git('config', 'user.email', 'fixture@invalid')
            git('config', 'core.autocrlf', 'false')
            (root / 'tools').mkdir()
            plan = dict(rust_required=False, selected_count=0, selected=[], matrix={'include': []}, changed_files=['docs/designs/a.md'])
            selector = root / 'tools/ci_select.py'
            selector.write_text('import json\nprint(json.dumps(' + repr(plan) + '))\n')
            git('add', 'tools'); git('commit', '-qm', 'trusted selector')
            base = git('rev-parse', 'HEAD')
            (root / 'doc.txt').write_text('candidate')
            git('add', 'doc.txt'); git('commit', '-qm', 'candidate')
            head = git('rev-parse', 'HEAD')
            git('checkout', '-q', '--detach', base)
            self.policy.update(trusted_revision=base, protected_paths=['tools'], zero_rust_selection={
                'base': base, 'head': head, 'trusted_revision': base,
                'selector_sha256': hashlib.sha256(selector.read_bytes()).hexdigest(), 'plan': plan})
            self.requirement['allowed_skips'].append('affected (runner)')
            self.assertEqual([], AUDIT.provenance_errors(root, base, head, self.policy))
            self.policy['zero_rust_selection']['head'] = base
            self.assertTrue(AUDIT.provenance_errors(root, base, head, self.policy))
            selector.write_text('raise Exception("untrusted")')
            self.assertTrue(AUDIT.provenance_errors(root, base, head, self.policy))


if __name__ == '__main__':
    unittest.main()

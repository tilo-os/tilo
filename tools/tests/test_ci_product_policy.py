"""Real reviewed workflow/selector, Cargo metadata and Git graph; no compilation."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).parents[2]
sys.path.insert(0, str(ROOT / 'tools/ci'))
SPEC = importlib.util.spec_from_file_location('product_policy', ROOT / 'tools/ci/product_policy.py')
p = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(p)


class PolicyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.git('init', '-q')
        self.git('config', 'user.name', 'Fixture')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.git('config', 'core.autocrlf', 'false')
        for name in p.CONTRACT:
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes((ROOT / name).read_bytes())
        for name in ('calculator', 'pkg', 'qdoc', 'collab-types'):
            directory = self.root / 'apps' / name
            (directory / 'src').mkdir(parents=True)
            (directory / 'src/lib.rs').write_text('pub fn fixture() {}\n')
            (directory / 'Cargo.toml').write_text(f'[package]\nname="{name}"\nversion="0.1.0"\nedition="2024"\n')
        (self.root / 'Cargo.toml').write_text('[workspace]\nmembers=["apps/*"]\nresolver="3"\n')
        (self.root / 'docs/designs').mkdir(parents=True)
        (self.root / 'docs/designs/fixture.md').write_text('design\n')
        subprocess.run(['cargo', 'metadata', '--no-deps', '--format-version', '1'], cwd=self.root, check=True, capture_output=True)
        self.git('add', '.')
        self.git('commit', '-qm', 'reviewed base')
        self.base = self.git('rev-parse', 'HEAD')

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.root), *args], text=True).strip()

    def candidate(self, path, data='changed\n'):
        (self.root / path).write_text(data)
        self.git('add', '.')
        self.git('commit', '-qm', 'candidate')
        head = self.git('rev-parse', 'HEAD')
        self.git('checkout', '-q', '--detach', self.base)
        return head

    def derive(self, head):
        runs = [{'id': i, 'workflow_id': workflow, 'event': 'pull_request', 'commit_sha': head}
                for i, workflow in ((10, 'check.yml'), (11, 'lint.yml'))]
        return p.derive(self.root, self.base, head, ['claude'], ['apps/calculator/src'], runs)

    def test_current_workflows_real_zero_rust_selector(self):
        policy, plan = self.derive(self.candidate('docs/designs/fixture.md'))
        self.assertFalse(plan['rust_required'])
        self.assertEqual(policy['runs'][0]['allowed_skips'], ['report-check-result', 'affected (runner)'])
        self.assertEqual(policy['runs'][0]['jobs'], ['builder', 'select', 'security', 'checks', 'report-check-result', 'affected (runner)'])
        self.assertEqual(policy['runs'][1]['jobs'], ['quality'])
        self.assertIn('.forgejo', policy['protected_paths'])
        self.assertIn('tools', policy['protected_paths'])
        self.assertIn('apps/calculator/Cargo.toml', policy['protected_paths'])

    def test_current_workflows_real_feedback_matrix(self):
        head = self.candidate('apps/calculator/src/lib.rs', 'pub fn changed() {}\n')
        policy, plan = self.derive(head)
        self.assertTrue(plan['rust_required'])
        self.assertEqual(plan['selected'], ['calculator', 'collab-types', 'pkg', 'qdoc'])
        expected = {'affected (x86_64, 52, 4, -p calculator -p collab-types -p pkg -p qdoc, runner, 1, -p calculator -p collab-types -p pkg -p qdoc)'}
        self.assertEqual(plan['tested'], plan['selected'])
        self.assertEqual(set(policy['runs'][0]['jobs'][5:]), expected)
        self.assertEqual(policy['runs'][0]['allowed_skips'], ['report-check-result'])
        self.assertNotIn('zero_rust_selection', policy)
        jobs = [{'id': i + 1, 'run_id': 10, 'repo_id': 23, 'name': n,
                 'status': 'success', 'attempt': 1, 'handle': f'h{i}', 'task_id': i + 30}
                for i, n in enumerate(policy['runs'][0]['jobs'])]
        jobs[4].update(status='skipped', task_id=0)
        run = {'id': 10, 'repository': {'id': 23, 'full_name': 'qos/qos'}, 'commit_sha': head,
               'workflow_id': 'check.yml', 'event': 'pull_request', 'status': 'success'}
        self.assertEqual([], p.audit.run_errors(run, jobs, 'qos/qos', run['commit_sha'], policy['runs'][0]))
        self.assertTrue(p.audit.run_errors(run, jobs[:-1], 'qos/qos', run['commit_sha'], policy['runs'][0]))
        jobs[-1]['status'] = 'skipped'
        jobs[-1]['task_id'] = 0
        self.assertTrue(p.audit.run_errors(run, jobs, 'qos/qos', run['commit_sha'], policy['runs'][0]))

    def test_reverse_dependant_checked_but_not_added_to_direct_test_scope(self):
        consumer = self.root / 'apps/consumer'
        (consumer / 'src').mkdir(parents=True)
        (consumer / 'src/lib.rs').write_text('pub fn consumer() {}\n')
        (consumer / 'Cargo.toml').write_text('[package]\nname="consumer"\nversion="0.1.0"\nedition="2024"\n[dependencies]\ncalculator={path="../calculator"}\n')
        subprocess.run(['cargo', 'metadata', '--no-deps', '--format-version', '1'], cwd=self.root, check=True, capture_output=True)
        self.git('add', '.')
        self.git('commit', '-qm', 'reviewed dependant')
        self.base = self.git('rev-parse', 'HEAD')
        _, plan = self.derive(self.candidate('apps/calculator/src/lib.rs', 'pub fn changed() {}\n'))
        self.assertIn('consumer', plan['selected'])
        self.assertNotIn('consumer', plan['tested'])
        row = plan['matrix']['include'][0]
        self.assertIn('-p consumer', row['packages'])
        self.assertNotIn('-p consumer', row['test_packages'])
        row['test_packages'] = ''
        with self.assertRaisesRegex(ValueError, 'test scope'):
            p.matrix_names(plan)

    def test_nightly_contract_drift_fails_closed(self):
        workflow = self.root / '.forgejo/workflows/nightly.yml'
        workflow.write_bytes(workflow.read_bytes() + b'\n# unreviewed change\n')
        self.git('add', '.')
        self.git('commit', '-qm', 'unreviewed qualification change')
        self.base = self.git('rev-parse', 'HEAD')
        with self.assertRaisesRegex(ValueError, 'unreviewed workflow/selector contract'):
            self.derive(self.candidate('docs/designs/fixture.md'))

    def test_review_requires_exact_head_official_independent_reviewer(self):
        head = 'a' * 40
        review = {'user': {'login': 'claude'}, 'commit_id': head, 'official': True,
                  'dismissed': False, 'state': 'APPROVED'}
        self.assertEqual([], p.audit.review_errors([review], head, 'author', ['claude']))
        for changed in (dict(review, commit_id='b' * 40), dict(review, dismissed=True),
                        dict(review, official=False), dict(review, user={'login': 'author'})):
            self.assertTrue(p.audit.review_errors([changed], head, 'author', ['claude']))

    def test_harness_and_unapproved_app_edits_rejected(self):
        for path in ('tools/ci/affected_packages.py', '.forgejo/workflows/check.yml', 'apps/pkg/src/lib.rs', 'Cargo.toml'):
            with self.subTest(path=path):
                head = self.candidate(path)
                with self.assertRaisesRegex(ValueError, 'harness, policy or unknown'):
                    self.derive(head)

    def test_deleted_allowed_source_rejected(self):
        (self.root / 'apps/calculator/src/lib.rs').unlink()
        self.git('add', '.')
        self.git('commit', '-qm', 'delete')
        head = self.git('rev-parse', 'HEAD')
        self.git('checkout', '-q', '--detach', self.base)
        with self.assertRaisesRegex(ValueError, 'deletion'):
            self.derive(head)

    def test_run_selection_ignores_old_success_status_and_push(self):
        head = 'a' * 40
        runs = [{'id': i, 'workflow_id': w, 'event': e, 'commit_sha': head}
                for i, w, e in [(1, 'check.yml', 'pull_request'), (2, 'check.yml', 'pull_request'),
                                (3, 'lint.yml', 'pull_request'), (4, 'check.yml', 'push')]]
        self.assertEqual(p.newest_pr_runs(runs, head), {'check.yml': 2, 'lint.yml': 3})
        with self.assertRaises(ValueError):
            p.newest_pr_runs(runs, 'b' * 40)

    def test_native_matrix_completeness_and_long_display_names(self):
        selected = ['package_' + str(i) + '_' + 'x' * 20 for i in range(30)]
        rows = [dict(arch=arch, runner=runner, shard=1, package_count=30, estimated_cost=400,
                     packages=' '.join('-p ' + name for name in selected), test_packages='-p ' + selected[0])
                for arch, runner in [('x86_64', 'runner')]]
        plan = dict(rust_required=True, selected=selected, tested=selected[:1], qualification='developer-feedback', selected_count=30, matrix={'include': rows})
        names = p.matrix_names(plan)
        self.assertTrue(all(len(name.encode()) == 255 and name.endswith('…') for name in names))
        self.assertEqual(names[0], ('affected (' + ', '.join(str(rows[0][k]) for k in sorted(rows[0])) + ')')[:252] + '…')
        plan['selected'] = selected[:-1]
        plan['selected_count'] = 29
        with self.assertRaisesRegex(ValueError, 'omits or duplicates'):
            p.matrix_names(plan)

    def test_routine_package_custom_build_helpers_require_manual_policy(self):
        build = self.root / 'apps/calculator/build.rs'
        build.write_text('include!("src/lib.rs");\nfn main() {}\n')
        self.git('add', '.')
        self.git('commit', '-qm', 'trusted custom build entrypoint')
        self.base = self.git('rev-parse', 'HEAD')
        head = self.candidate('apps/calculator/src/lib.rs', 'pub fn helper() {}\n')
        with self.assertRaisesRegex(ValueError, 'custom build script'):
            self.derive(head)


if __name__ == '__main__':
    unittest.main()

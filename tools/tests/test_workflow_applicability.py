"""Real Git history proves heavy PR work follows the PR diff, not merged main."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('gate', ROOT/'tools/ci/workflow_applicability.py')
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)


class Applicability(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.repo = Path(tmp.name)
        self.git('init', '-q')
        self.git('config', 'user.name', 'Fixture')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.write('docs/change.md', 'base')
        self.base = self.commit()

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.repo), *args], stderr=subprocess.PIPE).decode().strip()

    def write(self, name, text):
        p = self.repo/name
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)

    def commit(self):
        self.git('add', '.')
        self.git('commit', '-qm', 'fixture')
        return self.git('rev-parse', 'HEAD')

    def event(self, base, head):
        return {'pull_request': {'base': {'sha': base}, 'head': {'sha': head}}}

    def test_main_merge_does_not_make_its_changes_part_of_pr(self):
        self.git('checkout', '-qb', 'candidate')
        self.write('docs/change.md', 'candidate')
        self.commit()
        self.git('checkout', '-qb', 'new-main', self.base)
        self.write('os/kernel/new.config', 'main kernel change')
        base = self.commit()
        self.git('checkout', 'candidate')
        self.git('merge', '--no-edit', 'new-main')
        self.assertEqual(gate.applicable(self.repo, 'pull_request', self.event(base, self.git('rev-parse', 'HEAD')), ['os/kernel/**']), (False, []))

    def test_declared_transitive_dependency_and_rename_removal_match(self):
        paths = gate.declared_paths(ROOT/'.forgejo/workflows/tilo-relay.yaml')
        self.write('crates/doc/qdoc/src/lib.rs', 'dependency')
        head = self.commit()
        self.assertTrue(gate.applicable(self.repo, 'pull_request', self.event(self.base, head), paths)[0])
        self.git('mv', 'crates/doc/qdoc/src/lib.rs', 'docs/moved.rs')
        moved = self.commit()
        self.assertTrue(gate.applicable(self.repo, 'pull_request', self.event(head, moved), paths)[0])

    def test_non_pr_events_preserve_builds(self):
        for event in ('push', 'workflow_dispatch'):
            self.assertTrue(gate.applicable(self.repo, event, {}, ['os/kernel/**'])[0])

    def test_invalid_or_missing_history_cannot_report_skip(self):
        for base in ('main', '0'*40):
            with self.assertRaises((ValueError, subprocess.CalledProcessError)):
                gate.applicable(self.repo, 'pull_request', self.event(base, self.base), ['os/kernel/**'])

    def test_globs_and_unsupported_grammar(self):
        for pattern, path, expected in [('a/**/b', 'a/b', True), ('a/**/b', 'a/x/y/b', True),
                                         ('a/*.rs', 'a/x/y.rs', False), ('a/?.rs', 'a/x.rs', True)]:
            self.assertEqual(bool(gate.pattern_regex(pattern).fullmatch(path)), expected)
        for pattern in ('!a/**', 'a/[xy]', 'a/{b,c}'):
            with self.assertRaises(ValueError): gate.pattern_regex(pattern)

    def test_actual_declared_paths_and_unsupported_shapes(self):
        for name in ('build-kernel.yml', 'tilo-relay.yaml'):
            path = ROOT/'.forgejo/workflows'/name
            declared = gate.declared_paths(path)
            self.assertIn('tools/ci/workflow_applicability.py', declared)
            self.assertIn('.forgejo/workflows/'+name, declared)
            fixture = self.repo/'workflow.yml'
            fixture.write_text(path.read_text().replace('  pull_request:', '  pull_request: {}'))
            with self.assertRaises(ValueError): gate.declared_paths(fixture)

    def test_gate_uses_the_fast_lane_only_for_pull_requests(self):
        def job(text, prefix):
            lines = text.splitlines()
            start = next(i for i, line in enumerate(lines) if line.startswith(prefix))
            end = next((i for i in range(start + 1, len(lines))
                        if lines[i].startswith('  ') and not lines[i].startswith('    ')
                        and lines[i].strip()), len(lines))
            return '\n'.join(lines[start:end])

        for name in ('build-kernel.yml', 'tilo-relay.yaml'):
            text = (ROOT/'.forgejo/workflows'/name).read_text(encoding='utf-8')
            gate_job = job(text, '  applicability:')
            self.assertIn("if: ${{ github.event_name == 'pull_request' }}", gate_job)
            self.assertIn('runs-on: runner-quality', gate_job)
            self.assertRegex(gate_job, r'image: code\.podesta\.ai/qos/ci-fast@sha256:[0-9a-f]{64}')
            build = job(text, '  build-and-push')
            self.assertIn("github.event_name != 'pull_request'", build)
            self.assertIn("needs.applicability.outputs.applicable == 'true'", build)

    def test_failed_cli_does_not_emit_false_output(self):
        event = self.repo/'event.json'
        event.write_text(json.dumps(self.event('0'*40, self.base)))
        output = self.repo/'output'
        result = subprocess.run(['python3', '-I', str(ROOT/'tools/ci/workflow_applicability.py'),
            '--workflow', str(ROOT/'.forgejo/workflows/build-kernel.yml')], cwd=self.repo,
            env=dict(os.environ, GITHUB_EVENT_PATH=str(event), GITHUB_EVENT_NAME='pull_request', GITHUB_OUTPUT=str(output)),
            capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(output.exists())


if __name__ == '__main__':
    unittest.main()

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

    @staticmethod
    def workflow(applicability=False, build_command='make image',
                 publish_condition="github.ref == 'refs/heads/main'"):
        helper = '      - "tools/ci/workflow_applicability.py"\n' if applicability else ''
        gate = '''
  applicability:
    runs-on: runner-quality
    steps:
      - run: python3 tools/ci/workflow_applicability.py
''' if applicability else ''
        needs = '[signing-preflight, applicability]' if applicability else 'signing-preflight'
        condition = f"${{{{ always() && ({publish_condition}) }}}}"
        if applicability:
            condition = (
                "${{ always() && (github.event_name != 'pull_request' || "
                "(needs.applicability.result == 'success' && "
                "needs.applicability.outputs.applicable == 'true')) && "
                f"({publish_condition}) }}}}"
            )
        return f'''name: fixture
on:
  push:
    paths:
{helper}      - "src/**"
  pull_request:
    paths:
{helper}      - "src/**"
      - ".forgejo/workflows/fixture.yml"
jobs:
{gate}  signing-preflight:
    runs-on: runner
    steps:
      - run: check signing
  build:
    needs: {needs}
    if: {condition}
    runs-on: runner
    steps:
      - run: {build_command}
'''

    def test_inherited_main_history_skips_but_real_workflow_inputs_build(self):
        workflows = {
            'build-kernel.yml': 'os/kernel/new.config',
            'tilo-relay.yaml': 'crates/hosting/tilo-relay/src/lib.rs',
            'fs-fuse.yml': 'crates/system/fs/src/lib.rs',
            'legacy-app-e2e.yml': 'scripts/legacy-app-caret-e2e.py',
            'package-desktop.yml': 'dist/VERSION',
        }
        self.git('checkout', '-qb', 'candidate')
        self.write('docs/change.md', 'candidate')
        self.commit()
        self.git('checkout', '-qb', 'new-main', self.base)
        for path in workflows.values():
            self.write(path, 'main input change')
        base = self.commit()
        self.git('checkout', 'candidate')
        self.git('merge', '--no-edit', 'new-main')
        inherited_head = self.git('rev-parse', 'HEAD')
        for name in workflows:
            with self.subTest(workflow=name, change='inherited'):
                paths = gate.declared_paths(ROOT/'.forgejo/workflows'/name)
                self.assertEqual(gate.applicable(
                    self.repo, 'pull_request', self.event(base, inherited_head), paths),
                    (False, []))

        self.git('checkout', '-qb', 'real-inputs', base)
        for path in workflows.values():
            self.write(path, 'candidate input change')
        real_head = self.commit()
        for name, path in workflows.items():
            with self.subTest(workflow=name, change='real'):
                paths = gate.declared_paths(ROOT/'.forgejo/workflows'/name)
                applicable, matched = gate.applicable(
                    self.repo, 'pull_request', self.event(base, real_head), paths)
                self.assertTrue(applicable)
                self.assertIn(path, matched)

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
            self.assertEqual(gate.applicable(
                self.repo, event, {}, ['os/kernel/**'],
                '.forgejo/workflows/fixture.yml'), (True, []))

    def test_applicability_only_workflow_and_helper_changes_skip(self):
        self.write('.forgejo/workflows/fixture.yml', self.workflow())
        base = self.commit()
        self.write('.forgejo/workflows/fixture.yml', self.workflow(applicability=True))
        self.write('tools/ci/workflow_applicability.py', 'new gate implementation')
        head = self.commit()
        paths = ['src/**', '.forgejo/workflows/fixture.yml', 'tools/ci/workflow_applicability.py']
        self.assertEqual(gate.applicable(
            self.repo, 'pull_request', self.event(base, head), paths,
            '.forgejo/workflows/fixture.yml'), (False, []))

    def test_first_gate_install_on_unconditional_job_skips(self):
        base_source = self.workflow().replace(
            "    needs: signing-preflight\n"
            "    if: ${{ always() && (github.ref == 'refs/heads/main') }}\n",
            "",
        )
        gated_source = self.workflow(applicability=True).replace(
            "    needs: [signing-preflight, applicability]\n",
            "    needs: applicability\n",
        ).replace(
            "    if: ${{ always() && (github.event_name != 'pull_request' || "
            "(needs.applicability.result == 'success' && "
            "needs.applicability.outputs.applicable == 'true')) && "
            "(github.ref == 'refs/heads/main') }}\n",
            "    if: >-\n"
            "      ${{\n"
            "        always() &&\n"
            "        (github.event_name != 'pull_request' ||\n"
            "         (needs.applicability.result == 'success' && "
            "needs.applicability.outputs.applicable == 'true')) &&\n"
            "        true\n"
            "      }}\n",
        )
        self.write('.forgejo/workflows/fixture.yml', base_source)
        base = self.commit()
        self.write('.forgejo/workflows/fixture.yml', gated_source)
        head = self.commit()
        self.assertEqual(gate.applicable(
            self.repo, 'pull_request', self.event(base, head),
            ['.forgejo/workflows/fixture.yml'], '.forgejo/workflows/fixture.yml'),
            (False, []))

    def test_existing_gate_cannot_be_removed_or_partially_wired(self):
        gated = self.workflow(applicability=True)
        self.write('.forgejo/workflows/fixture.yml', gated)
        base = self.commit()
        cases = (
            gated.replace("    needs: [signing-preflight, applicability]\n",
                          "    needs: signing-preflight\n"),
            gated.replace(
                " && (github.event_name != 'pull_request' || "
                "(needs.applicability.result == 'success' && "
                "needs.applicability.outputs.applicable == 'true')) &&",
                " &&",
            ),
        )
        for source in cases:
            self.git('reset', '--hard', base)
            self.write('.forgejo/workflows/fixture.yml', source)
            head = self.commit()
            self.assertEqual(gate.applicable(
                self.repo, 'pull_request', self.event(base, head),
                ['.forgejo/workflows/fixture.yml'], '.forgejo/workflows/fixture.yml'),
                (True, ['.forgejo/workflows/fixture.yml']))

    def test_build_and_publication_contract_changes_build(self):
        self.write('.forgejo/workflows/fixture.yml', self.workflow())
        base = self.commit()
        cases = (
            self.workflow(applicability=True, build_command='make changed-image'),
            self.workflow(applicability=True, publish_condition="github.ref == 'refs/tags/v1'"),
        )
        for source in cases:
            self.git('reset', '--hard', base)
            self.write('.forgejo/workflows/fixture.yml', source)
            head = self.commit()
            self.assertEqual(gate.applicable(
                self.repo, 'pull_request', self.event(base, head),
                ['.forgejo/workflows/fixture.yml'], '.forgejo/workflows/fixture.yml'),
                (True, ['.forgejo/workflows/fixture.yml']))

    def test_direct_build_input_still_builds_with_applicability_changes(self):
        self.write('.forgejo/workflows/fixture.yml', self.workflow())
        base = self.commit()
        self.write('.forgejo/workflows/fixture.yml', self.workflow(applicability=True))
        self.write('tools/ci/workflow_applicability.py', 'new gate implementation')
        self.write('src/lib.rs', 'build input')
        head = self.commit()
        result, matched = gate.applicable(
            self.repo, 'pull_request', self.event(base, head),
            ['src/**', '.forgejo/workflows/fixture.yml', 'tools/ci/workflow_applicability.py'],
            '.forgejo/workflows/fixture.yml')
        self.assertTrue(result)
        self.assertIn('src/lib.rs', matched)

    def test_unprovable_workflow_contract_builds(self):
        self.write('.forgejo/workflows/fixture.yml', self.workflow())
        base = self.commit()
        self.write('.forgejo/workflows/fixture.yml', self.workflow(applicability=True).replace(
            "needs.applicability.outputs.applicable == 'true'",
            "needs.applicability.outputs.applicable != 'false'"))
        head = self.commit()
        self.assertEqual(gate.applicable(
            self.repo, 'pull_request', self.event(base, head),
            ['.forgejo/workflows/fixture.yml'], '.forgejo/workflows/fixture.yml'),
            (True, ['.forgejo/workflows/fixture.yml']))

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
        for name in ('build-kernel.yml', 'tilo-relay.yaml', 'fs-fuse.yml',
                     'legacy-app-e2e.yml', 'package-desktop.yml'):
            path = ROOT/'.forgejo/workflows'/name
            declared = gate.declared_paths(path)
            self.assertIn('tools/ci/workflow_applicability.py', declared)
            self.assertIn('.forgejo/workflows/'+name, declared)
            gate.workflow_contract(path.read_text(encoding='utf-8'))
            fixture = self.repo/'workflow.yml'
            fixture.write_text(path.read_text().replace('  pull_request:', '  pull_request: {}'))
            with self.assertRaises(ValueError): gate.declared_paths(fixture)

    def test_actual_job_map_gates_every_expensive_pr_job_on_the_fast_lane(self):
        def job(text, prefix):
            lines = text.splitlines()
            start = next(i for i, line in enumerate(lines) if line.startswith(prefix))
            end = next((i for i in range(start + 1, len(lines))
                        if lines[i].startswith('  ') and not lines[i].startswith('    ')
                        and lines[i].strip()), len(lines))
            return '\n'.join(lines[start:end])

        jobs = {
            'build-kernel.yml': ('build-and-push-kernel',),
            'tilo-relay.yaml': ('build-and-push',),
            'fs-fuse.yml': ('real-fuse', 'files-ux'),
            'legacy-app-e2e.yml': ('caret',),
            'package-desktop.yml': ('validate',),
        }
        for name, expensive_jobs in jobs.items():
            text = (ROOT/'.forgejo/workflows'/name).read_text(encoding='utf-8')
            gate_job = job(text, '  applicability:')
            self.assertIn("if: ${{ github.event_name == 'pull_request' }}", gate_job)
            self.assertIn('runs-on: runner-quality', gate_job)
            self.assertIn('timeout-minutes: 5', gate_job)
            self.assertRegex(gate_job, r'image: code\.podesta\.ai/qos/ci-fast@sha256:[0-9a-f]{64}')
            for job_name in expensive_jobs:
                with self.subTest(workflow=name, job=job_name):
                    build = job(text, f'  {job_name}:')
                    self.assertRegex(build, r'needs: (?:\[[^\n]*\bapplicability\b[^\n]*\]|applicability)')
                    self.assertIn('always()', build)
                    self.assertIn("github.event_name != 'pull_request'", build)
                    self.assertIn("needs.applicability.result == 'success'", build)
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

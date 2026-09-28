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
        helper = ('      - "tools/ci/workflow_applicability.py"\n'
                  '      - "tools/ci/prepare_pr_history.py"\n') if applicability else ''
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

    def test_only_applicability_wiring_changes_on_pull_requests_skip_builds(self):
        with self.subTest(phase='inherited main history skips but real workflow inputs build'):
            self.doCleanups()
            self.setUp()
            workflows = {
                'build-kernel.yml': 'os/kernel/new.config',
                'release-relay.yml': 'crates/hosting/tilo-relay/src/lib.rs',
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
        with self.subTest(phase='applicability only workflow and helper changes skip'):
            self.doCleanups()
            self.setUp()
            self.write('.forgejo/workflows/fixture.yml', self.workflow())
            base = self.commit()
            self.write('.forgejo/workflows/fixture.yml', self.workflow(applicability=True))
            self.write('tools/ci/workflow_applicability.py', 'new gate implementation')
            self.write('tools/ci/prepare_pr_history.py', 'new history implementation')
            head = self.commit()
            paths = ['src/**', '.forgejo/workflows/fixture.yml',
                     'tools/ci/workflow_applicability.py', 'tools/ci/prepare_pr_history.py']
            self.assertEqual(gate.applicable(
                self.repo, 'pull_request', self.event(base, head), paths,
                '.forgejo/workflows/fixture.yml'), (False, []))
        with self.subTest(phase='first gate install on unconditional job skips'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='existing gate cannot be removed or partially wired'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='non pr events preserve builds'):
            self.doCleanups()
            self.setUp()
            for event in ('push', 'workflow_dispatch'):
                self.assertEqual(gate.applicable(
                    self.repo, event, {}, ['os/kernel/**'],
                    '.forgejo/workflows/fixture.yml'), (True, []))

    def test_real_build_inputs_or_unprovable_contracts_always_build(self):
        with self.subTest(phase='declared transitive dependency and rename removal match'):
            self.doCleanups()
            self.setUp()
            paths = gate.declared_paths(ROOT/'.forgejo/workflows/release-relay.yml')
            self.write('crates/doc/qdoc/src/lib.rs', 'dependency')
            head = self.commit()
            self.assertTrue(gate.applicable(self.repo, 'pull_request', self.event(self.base, head), paths)[0])
            self.git('mv', 'crates/doc/qdoc/src/lib.rs', 'docs/moved.rs')
            moved = self.commit()
            self.assertTrue(gate.applicable(self.repo, 'pull_request', self.event(head, moved), paths)[0])
        with self.subTest(phase='build and publication contract changes build'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='direct build input still builds with applicability changes'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='unprovable workflow contract builds'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='invalid or missing history cannot report skip'):
            self.doCleanups()
            self.setUp()
            for base in ('main', '0'*40):
                with self.assertRaises((ValueError, subprocess.CalledProcessError)):
                    gate.applicable(self.repo, 'pull_request', self.event(base, self.base), ['os/kernel/**'])
        with self.subTest(phase='failed cli does not emit false output'):
            self.doCleanups()
            self.setUp()
            event = self.repo/'event.json'
            event.write_text(json.dumps(self.event('0'*40, self.base)))
            output = self.repo/'output'
            result = subprocess.run(['python3', '-I', str(ROOT/'tools/ci/workflow_applicability.py'),
                '--workflow', str(ROOT/'.forgejo/workflows/build-kernel.yml')], cwd=self.repo,
                env=dict(os.environ, GITHUB_EVENT_PATH=str(event), GITHUB_EVENT_NAME='pull_request', GITHUB_OUTPUT=str(output)),
                capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())

    def test_declared_paths_grammar_and_pr_job_gating_are_exact(self):
        with self.subTest(phase='globs and unsupported grammar'):
            self.doCleanups()
            self.setUp()
            for pattern, path, expected in [('a/**/b', 'a/b', True), ('a/**/b', 'a/x/y/b', True),
                                             ('a/*.rs', 'a/x/y.rs', False), ('a/?.rs', 'a/x.rs', True)]:
                self.assertEqual(bool(gate.pattern_regex(pattern).fullmatch(path)), expected)
            for pattern in ('!a/**', 'a/[xy]', 'a/{b,c}'):
                with self.assertRaises(ValueError): gate.pattern_regex(pattern)
        with self.subTest(phase='actual declared paths and unsupported shapes'):
            self.doCleanups()
            self.setUp()
            for name in ('build-kernel.yml', 'release-relay.yml', 'fs-fuse.yml',
                         'legacy-app-e2e.yml', 'package-desktop.yml'):
                path = ROOT/'.forgejo/workflows'/name
                declared = gate.declared_paths(path)
                self.assertIn('tools/ci/workflow_applicability.py', declared)
                self.assertIn('tools/ci/prepare_pr_history.py', declared)
                self.assertIn('.forgejo/workflows/'+name, declared)
                gate.workflow_contract(path.read_text(encoding='utf-8'))
                fixture = self.repo/'workflow.yml'
                fixture.write_text(path.read_text().replace('  pull_request:', '  pull_request: {}'))
                with self.assertRaises(ValueError): gate.declared_paths(fixture)
        with self.subTest(phase='history helper only routes pull request gates'):
            self.doCleanups()
            self.setUp()
            def event_paths(source, event):
                lines = source.splitlines()
                start = lines.index(f'  {event}:') + 1
                end = next((i for i in range(start, len(lines))
                            if lines[i].strip()
                            and len(lines[i]) - len(lines[i].lstrip()) <= 2), len(lines))
                block = lines[start:end]
                paths = block.index('    paths:') + 1
                return {json.loads(line[len('      - '):]) for line in block[paths:]
                        if line.startswith('      - "')}

            helper = 'tools/ci/prepare_pr_history.py'
            for name in ('build-kernel.yml', 'release-relay.yml'):
                with self.subTest(workflow=name):
                    source = (ROOT/'.forgejo/workflows'/name).read_text(encoding='utf-8')
                    self.assertNotIn(helper, event_paths(source, 'push'))
                    self.assertIn(helper, event_paths(source, 'pull_request'))
        with self.subTest(phase='actual job map gates every expensive pr job on the fast lane'):
            self.doCleanups()
            self.setUp()
            def job(text, prefix):
                lines = text.splitlines()
                start = next(i for i, line in enumerate(lines) if line.startswith(prefix))
                end = next((i for i in range(start + 1, len(lines))
                            if lines[i].startswith('  ') and not lines[i].startswith('    ')
                            and lines[i].strip()), len(lines))
                return '\n'.join(lines[start:end])

            jobs = {
                'build-kernel.yml': ('build-and-push-kernel',),
                'release-relay.yml': ('build-and-push',),
                'fs-fuse.yml': ('real-fuse', 'files-ux'),
                'legacy-app-e2e.yml': ('caret',),
                'package-desktop.yml': ('validate',),
            }
            shared = (ROOT/'.forgejo/workflows/applicability.yml').read_text(encoding='utf-8')
            gate_job = job(shared, '  compare:')
            self.assertIn("if: ${{ github.event_name == 'pull_request' }}", gate_job)
            self.assertIn('runs-on: runner-quality', gate_job)
            self.assertIn('timeout-minutes: 5', gate_job)
            self.assertRegex(gate_job, r'image: registry\.tilo\.so/tilo/ci-fast@sha256:[0-9a-f]{64}')
            self.assertIn('persist-credentials: false', gate_job)
            self.assertNotIn('REGISTRY_TOKEN', gate_job)
            self.assertIn('sparse-checkout-cone-mode: false', gate_job)
            self.assertIn('tools/ci/prepare_pr_history.py', gate_job)
            self.assertIn('tools/ci/workflow_applicability.py', gate_job)
            self.assertIn('${{ inputs.workflow }}', gate_job)
            self.assertLess(gate_job.index('Prepare targeted PR history'),
                            gate_job.index('Require an exact relevant PR diff'))
            for name, expensive_jobs in jobs.items():
                text = (ROOT/'.forgejo/workflows'/name).read_text(encoding='utf-8')
                caller = job(text, '  applicability:')
                self.assertIn('uses: ./.forgejo/workflows/applicability.yml', caller)
                self.assertIn('workflow: .forgejo/workflows/'+name, caller)
                for job_name in expensive_jobs:
                    with self.subTest(workflow=name, job=job_name):
                        build = job(text, f'  {job_name}:')
                        self.assertRegex(build, r'needs: (?:\[[^\n]*\bapplicability\b[^\n]*\]|applicability)')
                        self.assertIn('always()', build)
                        self.assertIn("github.event_name != 'pull_request'", build)
                        self.assertIn("needs.applicability.result == 'success'", build)
                        self.assertIn("needs.applicability.outputs.applicable == 'true'", build)


if __name__ == '__main__':
    unittest.main()

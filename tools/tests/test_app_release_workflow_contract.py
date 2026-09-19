"""Execute the actual release invariant, including disconnected-builder negatives."""
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class AppReleaseWorkflowContract(unittest.TestCase):
    def test_every_main_pr_plans_but_never_packages(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        pr = source.split('  pull_request:\n', 1)[1].split('\n  push:', 1)[0]
        self.assertRegex(pr, r'(?m)^    branches: \[main\]$')
        self.assertNotRegex(pr, r'(?m)^    (?:paths|paths-ignore):')
        plan = source.split('\n  plan:\n', 1)[1].split('\n  build:\n', 1)[0]
        self.assertNotRegex(plan, r'(?m)^    if:')
        self.assertIn('python3 -I tools/ci_python.py tools/app_release.py "${args[@]}"', plan)
        build = source.split('\n  build:\n', 1)[1].split('\n  publish:\n', 1)[0]
        condition = re.search(r'(?m)^    if: (.+)$', build).group(1)
        # Execute the production predicate, including both nonzero/zero plans.
        predicate = condition.replace('github.event_name', '"$1"').replace(
            'needs.plan.outputs.count', '"$2"')
        for event, count, allowed in [('pull_request', '51', False), ('pull_request', '0', False),
                                      ('workflow_dispatch', '51', True), ('push', '51', True),
                                      ('workflow_dispatch', '0', False)]:
            with self.subTest(event=event, count=count):
                result = subprocess.run(['bash', '-c', '[[ ' + predicate + ' ]]', 'predicate', event, count])
                self.assertEqual(0 if allowed else 1, result.returncode)
        push = source.split('\n  push:\n', 1)[1].split('\n  workflow_dispatch:', 1)[0]
        self.assertNotRegex(push, r'(?m)^    branches:')
        self.assertIn('tags: ["apps-v*", "app-*-v*"]', push)

    def check(self, workflow):
        script = (ROOT / 'os/tests/release_pipeline_invariants.sh').read_text()
        start = script.index("grep -Fq 'dist/apps/app-collections.toml' \"$apps\"")
        end = script.index("\ngrep -Fq 'TILO_APP_COLLECTION_SHA256'", start)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'release-apps.yml'
            path.write_bytes(workflow.encode())
            return subprocess.run(['bash', '-c',
                'set -euo pipefail\napps=$1\nok() { :; }\nbad() { exit 1; }\n'
                + script[start:end], 'contract', str(path)], capture_output=True).returncode

    def test_actual_immutable_builder_contract_passes(self):
        self.assertEqual(self.check((ROOT / '.forgejo/workflows/release-apps.yml').read_text()), 0)

    def test_crlf_and_large_job_do_not_create_false_failures(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        source = source.replace('    outputs:\n', '    # ' + 'x' * 131072 + '\n    outputs:\n', 1)
        self.assertEqual(self.check(source), 0)
        self.assertEqual(self.check(source.replace('\n', '\r\n')), 0)

    def test_publisher_does_not_receive_registry_token_in_steps(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        before, publish = source.split('\n  publish:\n', 1)
        publish = publish.replace('    steps:', '    steps:\n      - run: echo token\n        env:\n          FORBIDDEN: ${{ secrets.REGISTRY_TOKEN }}', 1)
        self.assertNotEqual(self.check(before + '\n  publish:\n' + publish), 0)

    def test_disconnected_or_mutable_builder_fails(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        for before, after in (
            ('      builder_image: ${{ steps.builder.outputs.image }}', '      other: unused'),
            ('        id: builder', '        id: different'),
            ('        run: python3 -I tools/ci_python.py tools/resolve_ci_builder.py', '        run: echo skipped'),
            ('        run: python3 -I tools/ci_python.py tools/resolve_ci_builder.py',
             '        run: python3 tools/resolve_ci_builder.py'),
            ('    needs: plan', '    needs: other'),
            ('      image: ${{ needs.plan.outputs.builder_image }}',
             '      image: code.sandbox.example/qos/tilo-builder:latest'),
        ):
            with self.subTest(before=before):
                self.assertIn(before, source)
                self.assertNotEqual(self.check(source.replace(before, after, 1)), 0)

    def test_required_image_reference_in_comment_cannot_replace_build_input(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        reference = '      image: ${{ needs.plan.outputs.builder_image }}'
        self.assertNotEqual(self.check(source.replace(reference, '# ' + reference)), 0)


if __name__ == '__main__':
    unittest.main()

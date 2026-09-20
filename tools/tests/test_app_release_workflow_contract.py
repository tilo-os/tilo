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
        self.assertIn('- "tools/apps/**"', source)
        pr = source.split('  pull_request:\n', 1)[1].split('\n  push:', 1)[0]
        self.assertRegex(pr, r'(?m)^    branches: \[main\]$')
        self.assertNotRegex(pr, r'(?m)^    (?:paths|paths-ignore):')
        plan = source.split('\n  plan:\n', 1)[1].split('\n  build:\n', 1)[0]
        self.assertNotRegex(plan, r'(?m)^    if:')
        self.assertIn('python3 -I tools/ci/python.py tools/apps/app_release.py "${args[@]}"', plan)
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
        self.assertRegex(push, r'(?m)^    branches: \[main\]$')
        self.assertIn('tags: ["apps-v*", "app-*-v*"]', push)

    def test_pr_plan_skips_builder_resolution_but_release_events_resolve(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        plan = source.split('\n  plan:\n', 1)[1].split('\n  build:\n', 1)[0]
        builder = plan.split('        id: builder\n', 1)[1].split('\n      - ', 1)[0]
        self.assertIn('run: python3 -I tools/ci/python.py tools/ci/resolve_builder.py', builder)
        condition = re.search(r'(?m)^        if: (.+)$', builder).group(1)
        predicate = condition.replace('github.event_name', '"$1"')
        # Execute the actual step predicate. Both publishing releases and the
        # nonpublishing coordinator dispatch still require immutable resolution.
        for event, allowed in [('pull_request', False), ('push', True),
                               ('workflow_dispatch', True)]:
            with self.subTest(event=event):
                result = subprocess.run(['bash', '-c', '[[ ' + predicate + ' ]]', 'predicate', event])
                self.assertEqual(0 if allowed else 1, result.returncode)
        selection = plan.split('        id: plan\n', 1)[1].split('\n      - ', 1)[0]
        self.assertNotIn('steps.builder.outputs', selection)
        provenance = plan.split('      - name: Retain trusted coordinator plan provenance\n', 1)[1]
        self.assertIn("if: github.event.inputs.coordinator_intent != ''", provenance)
        self.assertIn('RESOLVED_BUILDER: ${{ steps.builder.outputs.image }}', provenance)

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

    def test_persisted_collection_urls_use_the_configured_public_origin(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        publish = source.split('\n  publish:\n', 1)[1]
        self.assertIn('TILO_PACKAGE_PUBLIC_BASE_URL: ${{ vars.TILO_PACKAGE_PUBLIC_BASE_URL }}', publish)
        self.assertIn('package_public_api=${TILO_PACKAGE_PUBLIC_BASE_URL%/}', publish)
        self.assertIn('bundle_url="$package_public_api/tilo-app-collections/', publish)
        self.assertIn('bundle_upload_url="$package_api/tilo-app-collections/', publish)
        self.assertIn('channel_url=$bundle_url', publish)
        self.assertIn('update_variable TILO_APP_COLLECTION_URL "$channel_url"', publish)
        self.assertNotIn('channel_url=$bundle_upload_url', publish)
        self.assertIn("$package_public_api != *'.svc.cluster.local'*", publish)

    def test_verifier_package_secret_is_narrow_and_artifact_token_stays_job_scoped(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        binding = '          VERIFIER_BUNDLE_READ_TOKEN: ${{ secrets.TILO_VERIFIER_BUNDLE_READ_TOKEN }}'
        self.assertEqual(source.count(binding), 2)
        for occurrence in (0, 1):
            for replacement in ('${{ github.token }}', '${{ secrets.REGISTRY_TOKEN }}',
                                '${{ secrets.TILO_VERIFIER_BUNDLE_READ_TOKEN || secrets.REGISTRY_TOKEN }}'):
                with self.subTest(occurrence=occurrence, replacement=replacement):
                    parts = source.split(binding)
                    changed = binding.join(parts[:occurrence + 1]) + (
                        '          VERIFIER_BUNDLE_READ_TOKEN: ' + replacement
                    ) + binding.join(parts[occurrence + 1:])
                    self.assertNotEqual(self.check(changed), 0)
        artifact = '          FORGEJO_ARTIFACT_TOKEN: ${{ github.token }}'
        self.assertIn(artifact, source)
        self.assertNotEqual(self.check(source.replace(artifact,
            '          FORGEJO_ARTIFACT_TOKEN: ${{ secrets.TILO_VERIFIER_BUNDLE_READ_TOKEN }}')), 0)

    def test_disconnected_or_mutable_builder_fails(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        for before, after in (
            ('      builder_image: ${{ steps.builder.outputs.image }}', '      other: unused'),
            ('        id: builder', '        id: different'),
            ('        run: python3 -I tools/ci/python.py tools/ci/resolve_builder.py', '        run: echo skipped'),
            ('        run: python3 -I tools/ci/python.py tools/ci/resolve_builder.py',
             '        run: python3 tools/ci/resolve_builder.py'),
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

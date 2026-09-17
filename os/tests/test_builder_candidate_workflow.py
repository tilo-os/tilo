#!/usr/bin/env python3
"""Execute builder workflow shell policies with publication replaced by a stub."""

import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
BUILD = (ROOT / '.forgejo/workflows/build-builder-image.yml').read_text()
CHECK = (ROOT / '.forgejo/workflows/check.yml').read_text()


def step_script(workflow, name):
    step = workflow.split(f'- name: {name}\n', 1)[1]
    lines = step.split('        run: |\n', 1)[1].splitlines()
    body = []
    for line in lines:
        if line and not line.startswith('          '):
            break
        body.append(line)
    return textwrap.dedent('\n'.join(body))


class BuilderCandidates(unittest.TestCase):
    def resolve(self, requested):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'outputs'
            result = subprocess.run(
                ['bash', '-c', step_script(CHECK, 'Resolve the CI builder before starting job containers')],
                env={**os.environ, 'REQUESTED_BUILDER': requested, 'GITHUB_OUTPUT': str(output)},
                text=True, capture_output=True,
            )
            return result, output.read_text() if output.exists() else ''

    def test_only_immutable_same_repository_override_is_accepted(self):
        image = 'code.sandbox.example/qos/tilo-builder@sha256:' + 'a' * 64
        result, output = self.resolve(image)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output, f'image={image}\n')
        for invalid in [image[:-1], image.replace('tilo-builder', 'other'),
                        'code.sandbox.example/qos/tilo-builder:latest', image + '\ninjected=value',
                        '$(touch /tmp/should-not-run)']:
            with self.subTest(invalid=invalid):
                result, output = self.resolve(invalid)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(output, '')

    def test_ordinary_checks_keep_the_shared_default(self):
        result, output = self.resolve('')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output, 'image=code.sandbox.example/qos/tilo-builder:latest\n')

    def publication_tags(self, ref):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            (work / 'rust-toolchain.toml').write_text('channel = "1.97.1"\n')
            # Shell function intercepts the actual promotion invocation. No
            # registry client, signing key, build or network call is executed.
            stub = '''python3() {
              printf '%s\\n' "$@" > arguments
              printf 'candidate-reference\\n' > builder-index.txt
            }
            '''
            result = subprocess.run(
                ['bash', '-c', stub + step_script(BUILD, 'Verify and publish the tested native builder index')],
                cwd=work, env={**os.environ, 'BUILDER_REF': ref,
                               'GITHUB_SHA': 'b' * 40, 'GITHUB_RUN_ID': '42',
                               'GITHUB_RUN_ATTEMPT': '2', 'GITHUB_STEP_SUMMARY': str(work / 'summary')},
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            arguments = (work / 'arguments').read_text().splitlines()
            return [arguments[i + 1] for i, argument in enumerate(arguments) if argument == '--tag']

    def test_feature_refs_never_publish_shared_aliases(self):
        for ref in ['refs/heads/codex/arm64-tilo-integration', 'refs/tags/test-builder', 'refs/pull/1/head']:
            with self.subTest(ref=ref):
                self.assertEqual(self.publication_tags(ref), ['candidate-' + 'b' * 40 + '-42-2'])
        self.assertIn("github.ref == 'refs/heads/main' && matrix.arch || format('candidate-{0}-{1}'", BUILD)

    def test_main_keeps_existing_promotion_policy(self):
        self.assertEqual(self.publication_tags('refs/heads/main'), ['b' * 40, '1.97.1', 'latest'])

    def test_both_architectures_and_consumers_keep_acceptance_gates(self):
        self.assertIn('needs: [build, cache-warm]', BUILD)
        self.assertIn('runner: runner-arm64', BUILD)
        self.assertIn('name: builder-index-${{ github.sha }}', BUILD)
        self.assertEqual(CHECK.count('image: ${{ needs.builder.outputs.image }}'), 2)
        self.assertIn('  select:\n    needs: builder', CHECK)
        self.assertIn('  affected:\n    needs: [builder, select]', CHECK)
        builder_job = CHECK.split('  builder:\n', 1)[1].split('  select:\n', 1)[0]
        self.assertIn('DOCKER_BUILDKIT: "1"', builder_job)
        self.assertIn('run: bash os/tests/named_public_trust_context.sh docker', builder_job)


if __name__ == '__main__':
    unittest.main()

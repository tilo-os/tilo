#!/usr/bin/env python3
"""Execute builder workflow shell policies with publication replaced by a stub."""

import os
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
BUILD = (ROOT / '.forgejo/workflows/build-builder-image.yml').read_text()
CHECK = (ROOT / '.forgejo/workflows/check.yml').read_text()
SPEC = importlib.util.spec_from_file_location('builder_canary', ROOT / 'os/dev/builder-canary-inputs.py')
CANARY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CANARY)


def step_script(workflow, name):
    step = workflow.split(f'- name: {name}\n', 1)[1]
    if step.split('        run: ', 1)[1].splitlines()[0] != '|':
        return step.split('        run: ', 1)[1].splitlines()[0]
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
                env={**os.environ, 'REQUESTED_BUILDER': requested, 'GITHUB_OUTPUT': str(output), 'REGISTRY_TOKEN': ''},
                cwd=ROOT,
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

    def test_ordinary_checks_require_authenticated_resolution(self):
        result, output = self.resolve('')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(output, '')

    def publication_tags(self, ref):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            (work / 'rust-toolchain.toml').write_text('channel = "1.97.1"\n')
            helper = work / 'os/dev/configure-cosign-kms.sh'
            helper.parent.mkdir(parents=True)
            helper.symlink_to(ROOT / 'os/dev/configure-cosign-kms.sh')
            # Shell function intercepts the actual promotion invocation. No
            # registry client, signing key, build or network call is executed.
            stub = '''python3() {
              printf '%s\\n' "$@" > arguments
              cp "$CONTAINERS_STORAGE_CONF" storage-evidence
              printf 'candidate-reference\\n' > builder-index.txt
            }
            '''
            result = subprocess.run(
                ['bash', '-c', stub + step_script(BUILD, 'Verify and publish the tested native builder index')],
                cwd=work, env={**os.environ, 'BUILDER_REF': ref,
                               'GITHUB_SHA': 'b' * 40, 'GITHUB_RUN_ID': '42',
                               'GITHUB_RUN_ATTEMPT': '2', 'GITHUB_STEP_SUMMARY': str(work / 'summary'),
                               'RUNNER_TEMP': str(work), 'COSIGN_KEY_REF': 'gcpkms://test/key'},
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            storage = (work / 'storage-evidence').read_text()
            self.assertIn('driver = "vfs"', storage)
            self.assertIn(f'graphroot = "{work}/tilo-builder-manifests.', storage)
            self.assertIn(f'runroot = "{work}/tilo-builder-manifests.', storage)
            self.assertEqual(list(work.glob('tilo-builder-manifests.*')), [])
            arguments = (work / 'arguments').read_text().splitlines()
            return [arguments[i + 1] for i, argument in enumerate(arguments) if argument == '--tag']

    def test_feature_refs_never_publish_shared_aliases(self):
        for ref in ['refs/heads/codex/arm64-tilo-integration', 'refs/tags/test-builder', 'refs/pull/1/head']:
            with self.subTest(ref=ref):
                self.assertEqual(self.publication_tags(ref), ['candidate-' + 'b' * 40 + '-42-2'])
        self.assertIn("github.ref == 'refs/heads/main' && matrix.arch || format('candidate-{0}-{1}'", BUILD)

    def test_main_keeps_existing_promotion_policy(self):
        self.assertEqual(self.publication_tags('refs/heads/main'), ['b' * 40, '1.97.1', 'latest'])

    def test_candidate_buildcache_requires_a_full_revision(self):
        for revision in ['', 'a' * 40, 'latest', 'a' * 39, 'a' * 40 + '\ninjected=value']:
            with self.subTest(revision=revision), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / 'env'
                result = subprocess.run(
                    ['bash', '-c', step_script(BUILD, 'Select an optional candidate buildcache')],
                    env={**os.environ, 'REQUESTED_CACHE_REVISION': revision,
                         'GITHUB_SHA': 'b' * 40, 'GITHUB_ENV': str(output)},
                    text=True, capture_output=True,
                )
                if revision in ('', 'a' * 40):
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(output.read_text(), f'BUILDER_CACHE_REVISION={revision or "b" * 40}\n')
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(output.exists())

    def test_both_architectures_and_builder_consumers_keep_acceptance_gates(self):
        self.assertIn('needs: [build, images, cache-cold, cache-warm]', BUILD)
        self.assertIn('runner: runner-arm64', BUILD)
        self.assertIn('name: builder-index-${{ github.sha }}', BUILD)
        # Native acceptance consumers must honor the resolved builder override.
        # Selection only reads Cargo metadata and uses the qualified lean image.
        security_job = CHECK.split('  security:\n', 1)[1].split('\n  affected:\n', 1)[0]
        select_job = CHECK.split('  select:\n', 1)[1].split('\n  security:\n', 1)[0]
        self.assertIn('image: ${{ needs.builder.outputs.image }}', security_job)
        self.assertRegex(select_job, r'image: code\.podesta\.ai/qos/ci-fast@sha256:[0-9a-f]{64}\n')
        qualified_image = next(line.strip() for line in
                               (ROOT / '.forgejo/workflows/lint.yml').read_text().splitlines()
                               if line.strip().startswith('image: '))
        self.assertIn(qualified_image, select_job)
        self.assertIn('runs-on: runner-quality', select_job)
        self.assertIn('run: ./scripts/check-workspace-complete.sh', select_job)
        self.assertIn('run: python3 tools/check_repository_layout.py', select_job)
        self.assertIn('python3 tools/ci/affected_packages.py --base "$base" --shards 2', select_job)
        self.assertIn('  select:\n    needs: builder', CHECK)
        self.assertIn('  security:\n    needs: builder', CHECK)
        self.assertNotIn('needs.builder.outputs.image', select_job)
        affected_job = CHECK.split('  affected:\n', 1)[1].split('\n  checks:\n', 1)[0]
        self.assertIn('    needs: [builder, select]', affected_job)
        self.assertIn('image: ${{ needs.builder.outputs.image }}', affected_job)
        builder_job = CHECK.split('  builder:\n', 1)[1].split('  select:\n', 1)[0]
        self.assertIn('image: code.sandbox.example/qos/ci-control@sha256:', builder_job)
        self.assertNotIn('needs.builder.outputs.image', builder_job)
        self.assertIn('DOCKER_BUILDKIT: "1"', builder_job)
        self.assertIn('run: bash os/tests/named_public_trust_context.sh docker', builder_job)


class CanaryIdentity(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.revision = 'a' * 40
        self.images = {arch: CANARY.REPOSITORY + '@sha256:' + char * 64
                       for arch, char in (('x86_64', 'b'), ('aarch64', 'c'))}
        self.selected = {'include': [dict(arch=arch, image=image,
                         runner='runner' if arch == 'x86_64' else 'runner-arm64')
                         for arch, image in self.images.items()]}

    def save(self, directory, filename, value):
        path = self.root / directory / filename
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value))
        return path

    def cold(self, arch, attempt):
        value = CANARY.new_receipt(arch, self.images[arch], self.revision, '42', str(attempt))
        self.save(f'builder-cache-cold-{arch}-{attempt}', 'receipt.json', value)
        return value

    def warm(self, arch, attempt, cold):
        return self.save(f'builder-cache-warm-{arch}-{attempt}', 'warm.json',
                         dict(schema=1, warmAttempt=attempt, cold=cold))

    def test_native_retries_select_actual_latest_digest_per_architecture(self):
        for arch, image in self.images.items():
            self.save(f'builder-native-{arch}-1', 'child.json', dict(
                architecture=arch, digest=image.split('@')[1], sourceRevision=self.revision))
        current = self.save('builder-native-x86_64-3', 'child.json', dict(
            architecture='x86_64', digest='sha256:' + 'd' * 64, sourceRevision=self.revision))
        resolved = CANARY.matrix(self.root, self.revision)
        self.assertEqual(next(x['image'] for x in resolved['include'] if x['arch'] == 'x86_64'),
                         CANARY.REPOSITORY + '@sha256:' + 'd' * 64)
        CANARY.write_descriptors(self.root / 'selected', resolved, self.revision)
        self.assertEqual(CANARY.native.load_children(self.root / 'selected', self.revision)['x86_64'],
                         'sha256:' + 'd' * 64)
        self.save('duplicate/builder-native-x86_64-3', 'child.json', json.loads(current.read_text()))
        with self.assertRaisesRegex(ValueError, 'duplicate native'):
            CANARY.matrix(self.root, self.revision)

    def test_missing_native_architecture_fails(self):
        with self.assertRaisesRegex(ValueError, 'missing successful native'):
            CANARY.matrix(self.root, self.revision)

    def test_cold_retries_are_fresh_and_latest_success_is_selected(self):
        first = self.cold('x86_64', 1)
        latest = self.cold('x86_64', 3)
        self.assertNotEqual(first['namespace'], latest['namespace'])
        actual = CANARY.select_receipt(self.root, 'x86_64', self.images['x86_64'], self.revision, '42')
        self.assertEqual(actual, latest)
        self.save('duplicate/builder-cache-cold-x86_64-3', 'receipt.json', latest)
        with self.assertRaisesRegex(ValueError, 'duplicate successful cold'):
            CANARY.select_receipt(self.root, 'x86_64', self.images['x86_64'], self.revision, '42')

    def test_wrong_run_image_revision_and_architecture_fail_closed(self):
        original = self.cold('x86_64', 1)
        for field, bad in [('runId', '43'), ('revision', 'd' * 40),
                           ('image', self.images['aarch64']), ('architecture', 'aarch64')]:
            self.save('builder-cache-cold-x86_64-1', 'receipt.json', dict(original, **{field: bad}))
            with self.subTest(field=field), self.assertRaises(ValueError):
                CANARY.select_receipt(self.root, 'x86_64', self.images['x86_64'], self.revision, '42')

    def test_native_rebuild_can_supersede_old_cold_image_receipt(self):
        self.cold('x86_64', 1)
        self.images['x86_64'] = CANARY.REPOSITORY + '@sha256:' + 'e' * 64
        with self.assertRaisesRegex(ValueError, 'latest cold receipt used a different'):
            CANARY.select_receipt(self.root, 'x86_64', self.images['x86_64'], self.revision, '42')
        current = self.cold('x86_64', 2)
        self.assertEqual(CANARY.select_receipt(self.root, 'x86_64', self.images['x86_64'],
                                             self.revision, '42'), current)

    def test_promotion_rejects_stale_green_warm_after_cold_retry(self):
        for arch in self.images:
            self.warm(arch, 1, self.cold(arch, 1))
        CANARY.verify_acceptance(self.root, self.selected, self.revision, '42')
        latest = self.cold('x86_64', 2)
        with self.assertRaisesRegex(ValueError, 'warm acceptance does not match'):
            CANARY.verify_acceptance(self.root, self.selected, self.revision, '42')
        self.warm('x86_64', 7, latest)
        CANARY.verify_acceptance(self.root, self.selected, self.revision, '42')

    def test_missing_warm_receipt_cannot_satisfy_promotion(self):
        for arch in self.images:
            self.cold(arch, 1)
        with self.assertRaisesRegex(ValueError, 'warm acceptance does not match'):
            CANARY.verify_acceptance(self.root, self.selected, self.revision, '42')

    def test_workflow_carries_digests_and_all_acceptance_dependencies(self):
        self.assertEqual(BUILD.count('image: ${{ matrix.image }}'), 2)
        self.assertEqual(BUILD.count('matrix: ${{ fromJSON(needs.images.outputs.matrix) }}'), 2)
        self.assertIn('needs: [images, cache-cold]', BUILD)
        self.assertIn('needs: [build, images, cache-cold, cache-warm]', BUILD)
        self.assertIn('builder-canary-inputs.py verify acceptance-receipts', BUILD)
        self.assertLess(BUILD.index('bash tools/ci/sccache-canary.sh cold'),
                        BUILD.index('name: Retain only successful cold acceptance'))
        self.assertLess(BUILD.index('bash tools/ci/sccache-canary.sh warm'),
                        BUILD.index('name: Retain only successful warm acceptance'))


if __name__ == '__main__':
    unittest.main()

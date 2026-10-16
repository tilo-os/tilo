#!/usr/bin/env python3
"""Execute builder workflow shell policies with publication replaced by a stub."""

import os
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
BUILD = (ROOT / '.forgejo/workflows/build-ci-builder.yml').read_text()
CHECK = (ROOT / '.forgejo/workflows/check.yml').read_text()
SPEC = importlib.util.spec_from_file_location('builder_canary', ROOT / 'os/dev/builder-canary-inputs.py')
CANARY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CANARY)


def workflow_job(workflow, name):
    matches = list(re.finditer(rf"^  {re.escape(name)}:\n(.*?)(?=^  [\w-]+:|\Z)",
                               workflow, re.M | re.S))
    if len(matches) != 1:
        raise AssertionError(f"expected exactly one workflow job {name}")
    return matches[0].group(1)


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


def step_block(workflow, name):
    block = workflow.split(f'      - name: {name}\n', 1)[1]
    return block.split('\n      - name: ', 1)[0]


def action_input(block, name):
    marker = f'          {name}: '
    value = block.split(marker, 1)[1]
    first, *remaining = value.splitlines()
    if first != '|':
        return first
    continuation = []
    for line in remaining:
        if line and not line.startswith('            '):
            break
        continuation.append(line.removeprefix('            '))
    return '\n'.join(continuation).rstrip()


def action_inputs(block):
    return {
        match.group(1): action_input(block, match.group(1))
        for match in re.finditer(r'^          ([a-z][a-z0-9-]*):', block, re.MULTILINE)
    }


class BuilderCandidates(unittest.TestCase):
    def test_browser_recipe_and_native_acceptance(self):
        binary = '/usr/lib64/chromium-browser/chromium-browser'
        builder = (ROOT / 'os/Containerfile.builder').read_text()
        self.assertIn(f'ENV CHROME={binary}\n', builder)
        recipe = builder.split(f'ENV CHROME={binary}\nRUN ', 1)[1].split('\n\n', 1)[0]
        # Execute the production recipe with only package installation and cache
        # cleanup stubbed. A missing or broken browser must fail the image build.
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            browser = work / 'browser'
            stubs = 'dnf() { printf "%s\\n" "$*" >> "$RECEIPT"; }; rm() { :; };\n'
            for state in ('working', 'broken', 'missing'):
                with self.subTest(state=state):
                    browser.write_text('#!/bin/sh\nexit ' + ('0' if state == 'working' else '1') + '\n')
                    browser.chmod(0o755)
                    if state == 'missing':
                        browser.unlink()
                    receipt = work / 'receipt'
                    receipt.unlink(missing_ok=True)
                    result = subprocess.run(['bash', '-e', '-c', stubs + recipe],
                                            env={**os.environ, 'CHROME': str(browser),
                                                 'RECEIPT': str(receipt)},
                                            capture_output=True, text=True)
                    self.assertEqual(result.returncode == 0, state == 'working', result.stderr)
                    self.assertIn('install -y --setopt=install_weak_deps=False chromium',
                                  receipt.read_text().splitlines())
        cold = workflow_job(BUILD, 'cache-cold')
        script = step_script(cold, 'Verify the native browser test runtime')
        self.assertIn(f'test "$CHROME" = {binary}', script)
        self.assertIn("import {launch} from './tools/web/chrome.mjs'", script)
        self.assertIn("assert.equal(await tab.evaluate('6 * 7'), 42)", script)
        self.assertIn('await browser.close()', script)
        self.assertNotIn('continue-on-error', step_block(cold, 'Verify the native browser test runtime'))
        self.assertIn("needs.cache-cold.result == 'success'", workflow_job(BUILD, 'accept-builder'))

    def test_native_wrapper_exports_browser_outside_home_and_keeps_overrides(self):
        wrapper = ROOT / 'tools/ci/t-nextest.sh'
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'tools/ci').mkdir(parents=True)
            script = root / 'tools/ci/t-nextest.sh'
            script.write_text(wrapper.read_text())
            # Stand in only for the downstream environment/command. The real
            # wrapper must pass the browser through to the test invocation.
            (root / 'tools/test-env.sh').write_text('printf "%s\\n" "${CHROME-unset}"\n')
            bin_dir = root / 'bin'
            bin_dir.mkdir()
            uname = bin_dir / 'uname'
            uname.write_text('#!/bin/sh\nprintf "%s\\n" "$PLATFORM"\n')
            uname.chmod(0o755)
            for platform, override in ((platform, override)
                                       for platform in ('Linux', 'Darwin', 'FreeBSD')
                                       for override in (None, '', '/custom/verified-browser')):
                with self.subTest(platform=platform, override=override):
                    env = {key: value for key, value in os.environ.items() if key != 'CHROME'}
                    env.update(PATH=f'{bin_dir}:{env["PATH"]}', PLATFORM=platform)
                    if override is not None:
                        env['CHROME'] = override
                    result = subprocess.run(['bash', str(script), 'cargo', 'nextest', 'run',
                                             '-p', 'tilo-agent-participant', '-E', 'test(browser)'],
                                            env=env, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    expected = (override or '/usr/lib64/chromium-browser/chromium-browser'
                                if platform == 'Linux' else
                                'unset' if override is None else override)
                    self.assertEqual(result.stdout.strip(), expected)

    def test_browser_regressions_gate_native_receipts_and_shared_aliases(self):
        cold = workflow_job(BUILD, 'cache-cold')
        name = 'Run native browser regressions'
        script = step_script(cold, name)
        block = step_block(cold, name)
        self.assertNotIn('continue-on-error', block)
        self.assertNotIn('        if:', block)
        self.assertIn('matrix: ${{ fromJSON(needs.images.outputs.matrix) }}', cold)
        self.assertIn('image: ${{ matrix.image }}', cold)
        self.assertLess(cold.index('- name: Populate a new remote cache namespace'),
                        cold.index(f'- name: {name}'))
        self.assertLess(cold.index(f'- name: {name}'),
                        cold.index('- name: Retain only successful cold acceptance'))
        self.assertIn('needs: [images, cache-cold]', workflow_job(BUILD, 'cache-warm'))
        acceptance = workflow_job(BUILD, 'accept-builder')
        for phase in ('cache-cold', 'cache-warm'):
            self.assertIn(f"needs.{phase}.result == 'success'", acceptance)
        self.assertIn("needs.accept-builder.result == 'success'",
                      workflow_job(BUILD, 'promote-builder'))
        for binary in ('end_to_end', 'passkey_root', 'node_workspace', 'voice_browsers'):
            self.assertIn(f'binary({binary})', script)
            self.assertIn(f'--test {binary}', script)
            self.assertTrue((ROOT / f'crates/browser/tilo-browser-peer/tests/{binary}.rs').is_file())
        participant = (ROOT / 'crates/agent/agent-participant/tests/host_agents.rs').read_text()
        for test in ('the_host_runs_agents_a_browser_spawns_and_they_survive_a_restart',
                     'a_browser_sees_every_task_of_an_agent_live_and_after_a_reload',
                     'the_creator_and_a_joined_page_both_see_every_task_live'):
            self.assertIn(f'test(={test})', script)
            self.assertIn(f'async fn {test}()', participant)
        # Execute the actual workflow shell, intercepting only the expensive
        # wrapper. Neither a peer failure nor a participant failure may pass.
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            (work / 'tools/ci').mkdir(parents=True)
            (work / 'tools/ci/t-nextest.sh').write_text(
                'printf "%s\\n" "$*" >> calls\n'
                '[[ "$*" != *"$FAIL_PACKAGE"* ]]\n')
            for failure in ('none', 'tilo-browser-peer', 'tilo-agent-participant'):
                with self.subTest(failure=failure):
                    (work / 'calls').unlink(missing_ok=True)
                    result = subprocess.run(['bash', '-c', script], cwd=work,
                                            env={**os.environ, 'FAIL_PACKAGE': failure},
                                            text=True, capture_output=True)
                    self.assertEqual(result.returncode == 0, failure == 'none', result.stderr)
                    calls = (work / 'calls').read_text().splitlines()
                    self.assertEqual(len(calls), 1 if failure == 'tilo-browser-peer' else 2)
                    for call in calls:
                        self.assertIn('cargo nextest run --locked --profile ci --no-tests=fail', call)

    def test_windows_recipe_preserves_tooling_without_the_crypto_cli(self):
        builder = (ROOT / 'os/Containerfile.builder').read_text()
        block = builder.split('ARG JSIGN_VERSION=', 1)[1].split('\nRUN ', 1)[1].split('\n\n', 1)[0]
        script = '\n'.join(line for line in block.splitlines()
                           if not line.lstrip().startswith('--mount='))
        # Run the actual shell recipe without downloading assets or modifying the
        # host. Only the generated jsign wrapper is written, inside the fixture.
        script = script.replace('/usr/local/bin/jsign', './jsign')
        stubs = r'''
record() { builtin printf '%s\n' "$*" >> calls; [[ $1 != "$FAIL_AT" ]]; }
uname() { builtin printf '%s\n' "$ARCH"; }
dnf() {
  record dnf "$@" || return 1
  if [[ $1 == install ]]; then builtin printf '%s\n' "$@" > packages; fi
}
awk() { builtin printf '%064d\n' 1; }
sha256sum() { command cat >> checksums; record sha256sum "$@"; }
tar() { record tar "$@"; }
bash() { record bash "$@"; }
wine() { record wine "$@"; }
wineserver() { record wineserver "$@"; }
cp() { record cp "$@"; }
rm() { record rm "$@"; }
install() { record install "$@"; }
chmod() { record chmod "$@"; }
jsign() { record jsign "$@"; }
# Neither removed crypto tool may be reintroduced by the recipe.
osslsigncode() { record osslsigncode "$@"; return 1; }
openssl() { record openssl "$@"; return 1; }
test() {
  if [[ $1 == -f && $2 == /opt/inno-setup/ISCC.exe ]]; then
    record inno-present
  else
    builtin test "$@"
  fi
}
'''
        for arch, failure in [('aarch64', 'none')] + [
            ('x86_64', failure) for failure in
            ('none', 'dnf', 'sha256sum', 'bash', 'wine', 'wineserver', 'cp',
             'inno-present', 'install', 'chmod', 'jsign')
        ]:
            with self.subTest(arch=arch, failure=failure), tempfile.TemporaryDirectory() as directory:
                work = Path(directory)
                result = subprocess.run(
                    ['bash', '-e', '-c', stubs + script], cwd=work,
                    env={**os.environ, 'ARCH': arch, 'FAIL_AT': failure,
                         'INNO_SETUP_VERSION': '6.7.3', 'JSIGN_VERSION': '7.6'},
                    capture_output=True, text=True,
                )
                self.assertEqual(result.returncode == 0, failure == 'none', result.stderr)
                if arch == 'aarch64':
                    self.assertEqual(list(work.iterdir()), [])
                elif failure == 'none':
                    self.assertEqual((work / 'packages').read_text().splitlines(), [
                        'install', '-y', '--setopt=install_weak_deps=False',
                        'wine-core', 'wine-common', 'wine-cms', 'java-25-openjdk-headless',
                        'python3-pyyaml', 'python3-jsonschema',
                    ])
                    calls = (work / 'calls').read_text().splitlines()
                    self.assertEqual(calls.count('sha256sum --check --strict'), 2)
                    self.assertEqual((work / 'checksums').read_text().splitlines(), [
                        f'{1:064d}  /tilo-assets/innosetup-6.7.3.exe',
                        f'{1:064d}  /tilo-assets/jsign-7.6.jar',
                    ])
                    self.assertIn('bash /tmp/wine-prefix.sh', calls)
                    self.assertIn('wineserver -w', calls)
                    self.assertIn('cp -r /tmp/inno-prefix/drive_c/inno-setup /opt/inno-setup', calls)
                    self.assertIn('inno-present', calls)
                    self.assertIn('jsign --help', calls)
                    self.assertFalse(any(call.split()[0] in ('openssl', 'osslsigncode')
                                         for call in calls), calls)
                    self.assertEqual((work / 'jsign').read_text(),
                                     '#!/bin/sh\nexec java -jar /usr/local/share/jsign/jsign.jar "$@"\n')

    def test_registry_write_ordering(self):
        with self.subTest(phase="native_registry_writers_are_serialized"):
            build_job = BUILD.split('\n  build:\n', 1)[1].split('\n  images:\n', 1)[0]
            self.assertEqual(build_job.count('\n      max-parallel: 1\n'), 1)
        with self.subTest(phase="image_push_finishes_before_the_same_graph_exports_cache"):
            image = step_block(BUILD, 'Build and push')
            cache = step_block(BUILD, 'Export build cache')
            signing = step_block(BUILD, 'Sign and describe the native builder')

            self.assertIn('id: build', image)
            self.assertEqual(action_input(image, 'push'), 'true')
            self.assertNotIn('cache-to:', image)
            self.assertEqual(action_input(cache, 'push'), 'false')
            self.assertIn('cache-to:', cache)
            self.assertLess(BUILD.index('- name: Build and push'),
                            BUILD.index('- name: Export build cache'))
            self.assertLess(BUILD.index('- name: Export build cache'),
                            BUILD.index('- name: Sign and describe the native builder'))

            # The second invocation resolves entirely from the BuildKit state made
            # by the first. Compare every action input so a future graph option
            # cannot drift between the two exporters unnoticed.
            image_inputs = action_inputs(image)
            cache_inputs = action_inputs(cache)
            self.assertEqual(set(cache_inputs) - set(image_inputs), {'cache-to'})
            self.assertEqual(set(image_inputs) - set(cache_inputs), set())
            for name in sorted(set(image_inputs) - {'push'}):
                self.assertEqual(cache_inputs[name], image_inputs[name], name)

            # Signing remains bound to the pushed image, never to cache metadata.
            self.assertIn('CHILD_DIGEST: ${{ steps.build.outputs.digest }}', signing)
        with self.subTest(phase="candidate_buildcache_requires_a_full_revision"):
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

    def test_ref_override_and_gate_policy(self):
        with self.subTest(phase="legacy_subjects_are_installed_only_for_e2e_and_cannot_be_skipped"):
            subjects = ['gnome-text-editor', 'qt6-qtbase-examples', 'libreoffice-writer']
            # The builder installs the shared script's `fedora test` profile, so the
            # image's test subjects are the recipe plus that script.
            builder = (ROOT / 'os/Containerfile.builder').read_text()
            self.assertIn('install-build-deps.sh fedora test', builder)
            recipe = builder + (ROOT / 'dist/ci/install-build-deps.sh').read_text()
            for subject in subjects[1:]:
                self.assertNotIn(subject, recipe)
            self.assertIn("gnome-text-editor", recipe)
            legacy = (ROOT / '.forgejo/workflows/ci-legacy-apps.yml').read_text()
            script = step_script(legacy, 'Install and verify real app subjects')
            # Execute the workflow script with package installation and subject
            # discovery intercepted. Missing subjects must still fail after dnf.
            stubs = r'''dnf() { printf '%s\n' "$@" > dnf-args; test "$FAIL_AT" != dnf; }
        rpm() { printf '%s\n' "$@" > rpm-args; test "$FAIL_AT" != rpm; }
        command() { test "$2" != "$FAIL_AT"; }
        find() { test "$FAIL_AT" != qt || return 1; echo /qt/widgets/lineedits/lineedits/lineedits; }
        '''
            for failure in ('none', 'dnf', 'rpm', 'gnome-text-editor', 'libreoffice', 'qt'):
                with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                    work = Path(directory)
                    result = subprocess.run(['bash', '-e', '-c', stubs + script], cwd=work,
                                            env={**os.environ, 'FAIL_AT': failure},
                                            capture_output=True, text=True)
                    self.assertEqual(result.returncode == 0, failure == 'none', result.stderr)
                    self.assertEqual((work / 'dnf-args').read_text().splitlines(),
                                     ['install', '-y', '--setopt=install_weak_deps=False', *subjects])
                    if failure == 'none':
                        self.assertEqual((work / 'rpm-args').read_text().splitlines(), ['-q', *subjects])
        with self.subTest(phase="only_immutable_same_repository_override_is_accepted"):
            image = 'registry.tilo.so/tilo/tilo-builder@sha256:' + 'a' * 64
            result, output = self.resolve(image)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(output, f'image={image}\n')
            for invalid in [image[:-1], image.replace('tilo-builder', 'other'),
                            'registry.tilo.so/tilo/tilo-builder:latest', image + '\ninjected=value',
                            '$(touch /tmp/should-not-run)']:
                with self.subTest(invalid=invalid):
                    result, output = self.resolve(invalid)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(output, '')
        with self.subTest(phase="ordinary_checks_resolve_the_builder_anonymously"):
            # The builder is public on registry.tilo.so; no credential reaches
            # the resolver.
            block = CHECK.split('- name: Resolve the CI builder before starting job containers\n', 1)[1]
            block = block.split('\n      - ', 1)[0]
            self.assertIn('run: python3 tools/ci/resolve_builder.py', block)
            self.assertNotIn('REGISTRY_TOKEN', block)
        with self.subTest(phase="feature_refs_never_publish_shared_aliases"):
            for ref in ['refs/heads/codex/arm64-tilo-integration', 'refs/tags/test-builder', 'refs/pull/1/head']:
                with self.subTest(ref=ref):
                    self.assertEqual(self.publication_tags(ref), ['candidate-' + 'b' * 40 + '-42-2'])
            self.assertIn("github.ref == 'refs/heads/main' && format('tilo-builder-{0}', matrix.arch) || format('tilo-builder-candidate-{0}-{1}'", BUILD)
        with self.subTest(phase="main_keeps_existing_promotion_policy"):
            self.assertEqual(self.publication_tags('refs/heads/main'), ['b' * 40, '1.97.1', 'latest'])
        with self.subTest(phase="both_architectures_and_builder_consumers_keep_acceptance_gates"):
            self.assertIn('needs: [plan, build, images, accept-builder]', BUILD)
            self.assertIn('runner: runner-arm64', BUILD)
            self.assertIn('name: builder-index-${{ github.sha }}', BUILD)
            # Native acceptance consumers must honor the resolved builder override.
            # Compile-free selection/security use the qualified lean image.
            # Extract actual jobs: a removed following-name delimiter must not
            # let a security assertion accidentally observe a native job.
            security_job = workflow_job(CHECK, 'security')
            select_job = workflow_job(CHECK, 'select')
            self.assertRegex(select_job, r'image: registry\.tilo\.so/tilo/ci-fast@sha256:[0-9a-f]{64}\n')
            qualified_image = next(line.strip() for line in
                                   (ROOT / '.forgejo/workflows/lint.yml').read_text().splitlines()
                                   if line.strip().startswith('image: '))
            self.assertIn(qualified_image, select_job)
            self.assertIn(qualified_image, security_job)
            self.assertNotIn('needs.builder.outputs.image', security_job)
            self.assertIn('runs-on: runner-quality', security_job)
            self.assertIn('run: bash tools/run-security-boundary-invariants.sh', security_job)
            self.assertIn('runs-on: runner-quality', select_job)
            self.assertIn('run: ./scripts/check-workspace-complete.sh', select_job)
            self.assertIn('run: python3 tools/policy/check_repository_layout.py', select_job)
            self.assertIn('run: bash tools/ci/select.sh', select_job)
            self.assertIn('python3 tools/ci/affected_packages.py --base "$base" --shards 2',
                          (ROOT / 'tools/ci/select.sh').read_text())
            self.assertNotRegex(select_job, r'(?m)^    needs:')
            self.assertNotIn('needs.', select_job)
            self.assertIn("if: needs.select.outputs.security_required == 'true'", security_job)
            self.assertIn('needs: [select]', security_job)
            self.assertNotIn('needs.builder.outputs.image', select_job)
            shard_jobs = {name: workflow_job(CHECK, name)
                          for name in ('test', 'check', 'clippy', 'android', 'wasm')}
            for name, job in shard_jobs.items():
                with self.subTest(job=name):
                    self.assertIn('    needs: [builder, select, security]', job)
                    self.assertIn('image: ${{ needs.builder.outputs.image }}', job)
                    self.assertIn('      - name: Checkout exact change', job)
                    self.assertIn('          fetch-depth: 1', job)
                    self.assertNotIn('          fetch-depth: 0', job)
            clippy_job = shard_jobs['clippy']
            self.assertIn('tools/ci/clippy-check.sh ${{ matrix.packages }}', clippy_job)
            self.assertNotIn('needs.select.outputs.base', clippy_job)
            builder_job = CHECK.split('  builder:\n', 1)[1].split('  select:\n', 1)[0]
            self.assertIn('image: registry.tilo.so/tilo/ci-control@sha256:', builder_job)
            self.assertNotIn('needs.builder.outputs.image', builder_job)
            self.assertIn('DOCKER_BUILDKIT: "1"', builder_job)
            self.assertIn('run: bash os/tests/named_public_trust_context.sh docker', builder_job)

    def resolve(self, requested):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'outputs'
            result = subprocess.run(
                ['bash', '-c', step_script(CHECK, 'Resolve the CI builder before starting job containers')],
                env={**os.environ, 'REQUESTED_BUILDER': requested, 'GITHUB_OUTPUT': str(output)},
                cwd=ROOT,
                text=True, capture_output=True,
            )
            return result, output.read_text() if output.exists() else ''

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


def restart_fixture(test):
    """Give the next subTest phase of a merged test the fresh fixture it had
    as its own test: tear down, run cleanups, reset instance state, set up."""
    test.tearDown()
    test.doCleanups()
    fresh = type(test)(test._testMethodName).__dict__
    keep = {key: test.__dict__[key] for key in ("_outcome", "_subtest") if key in test.__dict__}
    test.__dict__.clear()
    test.__dict__.update(fresh, **keep)
    test.setUp()


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

    def test_retry_digest_selection(self):
        with self.subTest(phase="native_retries_select_actual_latest_digest_per_architecture"):
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
        restart_fixture(self)
        with self.subTest(phase="missing_native_architecture_fails"):
            with self.assertRaisesRegex(ValueError, 'missing successful native'):
                CANARY.matrix(self.root, self.revision)
        restart_fixture(self)
        with self.subTest(phase="cold_retries_are_fresh_and_latest_success_is_selected"):
            first = self.cold('x86_64', 1)
            latest = self.cold('x86_64', 3)
            self.assertNotEqual(first['namespace'], latest['namespace'])
            actual = CANARY.select_receipt(self.root, 'x86_64', self.images['x86_64'], self.revision, '42')
            self.assertEqual(actual, latest)
            self.save('duplicate/builder-cache-cold-x86_64-3', 'receipt.json', latest)
            with self.assertRaisesRegex(ValueError, 'duplicate successful cold'):
                CANARY.select_receipt(self.root, 'x86_64', self.images['x86_64'], self.revision, '42')
        restart_fixture(self)
        with self.subTest(phase="wrong_run_image_revision_and_architecture_fail_closed"):
            original = self.cold('x86_64', 1)
            for field, bad in [('runId', '43'), ('revision', 'd' * 40),
                               ('image', self.images['aarch64']), ('architecture', 'aarch64')]:
                self.save('builder-cache-cold-x86_64-1', 'receipt.json', dict(original, **{field: bad}))
                with self.subTest(field=field), self.assertRaises(ValueError):
                    CANARY.select_receipt(self.root, 'x86_64', self.images['x86_64'], self.revision, '42')
        restart_fixture(self)
        with self.subTest(phase="native_rebuild_can_supersede_old_cold_image_receipt"):
            self.cold('x86_64', 1)
            self.images['x86_64'] = CANARY.REPOSITORY + '@sha256:' + 'e' * 64
            with self.assertRaisesRegex(ValueError, 'latest cold receipt used a different'):
                CANARY.select_receipt(self.root, 'x86_64', self.images['x86_64'], self.revision, '42')
            current = self.cold('x86_64', 2)
            self.assertEqual(CANARY.select_receipt(self.root, 'x86_64', self.images['x86_64'],
                                                 self.revision, '42'), current)

    def test_promotion_receipts_and_reuse(self):
        with self.subTest(phase="promotion_rejects_stale_green_warm_after_cold_retry"):
            for arch in self.images:
                self.warm(arch, 1, self.cold(arch, 1))
            CANARY.verify_acceptance(self.root, self.selected, self.revision, '42')
            latest = self.cold('x86_64', 2)
            with self.assertRaisesRegex(ValueError, 'warm acceptance does not match'):
                CANARY.verify_acceptance(self.root, self.selected, self.revision, '42')
            self.warm('x86_64', 7, latest)
            CANARY.verify_acceptance(self.root, self.selected, self.revision, '42')
        restart_fixture(self)
        with self.subTest(phase="missing_warm_receipt_cannot_satisfy_promotion"):
            for arch in self.images:
                self.cold(arch, 1)
            with self.assertRaisesRegex(ValueError, 'warm acceptance does not match'):
                CANARY.verify_acceptance(self.root, self.selected, self.revision, '42')
        restart_fixture(self)
        with self.subTest(phase="workflow_carries_digests_and_all_acceptance_dependencies"):
            self.assertEqual(BUILD.count('image: ${{ matrix.image }}'), 2)
            self.assertEqual(BUILD.count('matrix: ${{ fromJSON(needs.images.outputs.matrix) }}'), 2)
            self.assertIn('needs: [images, cache-cold]', BUILD)
            self.assertIn('  accept-builder:', BUILD)
            self.assertIn('needs: [plan, build, images, cache-cold, cache-warm]', BUILD)
            self.assertIn('needs: [plan, build, images, accept-builder]', BUILD)
            self.assertIn('builder-canary-inputs.py verify acceptance-receipts', BUILD)
            self.assertLess(BUILD.index('bash tools/ci/sccache-canary.sh cold'),
                            BUILD.index('name: Retain only successful cold acceptance'))
            self.assertLess(BUILD.index('bash tools/ci/sccache-canary.sh warm'),
                            BUILD.index('name: Retain only successful warm acceptance'))
            # The CI hosts pull the builder from registry.tilo.so through their
            # node cache; nothing mirrors it into a cloud registry.
            self.assertNotIn('dkr.ecr.', BUILD)
        restart_fixture(self)
        with self.subTest(phase="reuse_never_reaches_signing_or_promotion"):
            signing = BUILD.split('  signing-preflight:', 1)[1].split('\n  build:', 1)[0]
            build = BUILD.split('  build:', 1)[1].split('\n  images:', 1)[0]
            promotion = BUILD.split('  promote-builder:', 1)[1]
            acceptance = BUILD.split('  accept-builder:', 1)[1].split('\n  promote-builder:', 1)[0]
            self.assertIn("if: needs.plan.outputs.mode == 'build'", signing)
            self.assertIn("needs.plan.outputs.mode == 'build'", build)
            self.assertIn("needs.plan.outputs.mode == 'build'", promotion)
            self.assertNotIn('COSIGN_KEY_REF', acceptance)
            self.assertNotIn('enable-openid-connect', acceptance)
            self.assertNotIn('promote-native-index.py', acceptance)
            self.assertIn("needs.plan.outputs.mode == 'reuse'", acceptance)

    def test_aws_cli_pinning(self):
        with self.subTest(phase="main_promotion_installs_pinned_aws_cli_without_distro_package"):
            tools = step_block(BUILD, 'Install publication tools')
            self.assertIn('curl unzip podman skopeo', tools)
            self.assertNotRegex(tools, r'install .*\bawscli\b')
            self.assertIn('bash ./os/dev/install-aws-cli.sh', tools)
        restart_fixture(self)
        with self.subTest(phase="aws_cli_rejects_corrupt_download_before_extraction"):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                bin_dir = root / 'bin'
                bin_dir.mkdir()
                # A successful transfer containing invalid bytes must never execute.
                scripts = {
                    'curl': '#!/bin/sh\nwhile [ "$1" != "-o" ]; do shift; done\nprintf corrupt > "$2"\n',
                    'unzip': '#!/bin/sh\ntouch "$RUNNER_TEMP/extracted"\nexit 99\n',
                }
                for name, body in scripts.items():
                    path = bin_dir / name
                    path.write_text(body)
                    path.chmod(0o755)
                result = subprocess.run(
                    ['bash', str(ROOT / 'os/dev/install-aws-cli.sh')],
                    env={**os.environ, 'PATH': str(bin_dir) + ':' + os.environ['PATH'],
                         'RUNNER_TEMP': directory, 'GITHUB_PATH': str(root / 'path')},
                    capture_output=True, text=True,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((root / 'extracted').exists())
                self.assertFalse((root / 'path').exists())
                self.assertEqual(list(root.glob('aws-cli-download.*')), [])


if __name__ == '__main__':
    unittest.main()

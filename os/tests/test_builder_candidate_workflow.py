#!/usr/bin/env python3
"""The CI builder's candidate policy: its recipe, the canaries that run inside each candidate
(tools/ci/publish/ci-builder-canary.sh) and the t checks that gate signing and publication on
them (the ci-builder product of tools/ci/products.toml, generated into t.toml)."""

import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
CANARY = (ROOT / 'tools/ci/publish/ci-builder-canary.sh').read_text()
sys.path.insert(0, str(ROOT / 'tools/ci'))
import t_matrix  # noqa: E402
CHECKS = {check['name']: check for check in t_matrix.checks(ROOT)}
ARCHES = ('x86_64', 'aarch64')
# The checks that sign or push a candidate: each must wait for the canaries.
PUBLICATION = ('sign-ci-builder', 'sign-dev-ci-builder', 'publish-ci-builder')
BROWSER = '/usr/lib64/chromium-browser/chromium-browser'


def canary_arm(mode):
    """The body of one `case` arm of ci-builder-canary.sh."""
    match = re.search(rf'^  {re.escape(mode)}\)\n(.*?)^    ;;$', CANARY, re.M | re.S)
    if match is None:
        raise AssertionError(f'ci-builder-canary.sh has no {mode} arm')
    return textwrap.dedent(match.group(1))


def published_tags(run):
    """`channel:tag` pairs of each oci-publish.sh line, by the directory it publishes."""
    tags = {}
    for line in run.splitlines():
        words = line.split()
        if words[:2] == ['bash', 'tools/ci/publish/oci-publish.sh']:
            tags[words[4].rsplit('/', 1)[1]] = [word.strip("'") for word in words[5:]]
    return tags


class BuilderCandidates(unittest.TestCase):
    def test_browser_recipe_and_native_acceptance(self):
        builder = (ROOT / 'os/Containerfile.builder').read_text()
        self.assertIn(f'ENV CHROME={BROWSER}\n', builder)
        recipe = builder.split(f'ENV CHROME={BROWSER}\nRUN ', 1)[1].split('\n\n', 1)[0]
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
        script = canary_arm('browser')
        self.assertIn(f'test "${{CHROME:-}}" = {BROWSER}', script)
        self.assertIn("import {launch} from './tools/web/chrome.mjs'", script)
        self.assertIn("assert.equal(await tab.evaluate('6 * 7'), 42)", script)
        self.assertIn('await browser.close()', script)
        for name in PUBLICATION:
            with self.subTest(check=name):
                self.assertIn('canary-ci-builder-browser-x86_64', CHECKS[name]['needs'])

    def test_native_wrapper_exports_browser_outside_home_and_keeps_overrides(self):
        wrapper = ROOT / 'tools/ci/t-nextest.sh'
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'tools/ci').mkdir(parents=True)
            script = root / 'tools/ci/t-nextest.sh'
            script.write_text(wrapper.read_text())
            (root / 'tools/ci/package_features.py').write_text(
                (ROOT / 'tools/ci/package_features.py').read_text())
            # Stand in only for the downstream environment/command. The real
            # wrapper must pass the browser through to the test invocation.
            (root / 'tools/test-env.sh').write_text('printf "%s\\n" "${CHROME-unset}"\n')
            bin_dir = root / 'bin'
            bin_dir.mkdir()
            uname = bin_dir / 'uname'
            uname.write_text('#!/bin/sh\nprintf "%s\\n" "$PLATFORM"\n')
            uname.chmod(0o755)
            # The wrapper builds a helper binary for this package before testing it; building is
            # not what is checked here, and the check runs where nothing builds.
            cargo = bin_dir / 'cargo'
            cargo.write_text('#!/bin/sh\nexit 0\n')
            cargo.chmod(0o755)
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
                    expected = (override or BROWSER
                                if platform == 'Linux' else
                                'unset' if override is None else override)
                    self.assertEqual(result.stdout.strip(), expected)

    def test_browser_regressions_gate_publication(self):
        script = canary_arm('browser')
        self.assertNotIn('|| true', script)
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
        # Execute the canary's own shell, intercepting only the browser itself, node and the
        # expensive wrapper. Neither a peer failure nor a participant failure may pass.
        stubs = textwrap.dedent(f'''\
            set -euo pipefail
            die() {{ echo "$*" >&2; exit 1; }}
            {BROWSER}() {{ echo chromium; }}
            node() {{ cat > /dev/null; }}
            scratch=$PWD/scratch
            ''')
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            (work / 'tools/ci').mkdir(parents=True)
            (work / 'tools/ci/t-nextest.sh').write_text(
                'printf "%s\\n" "$*" >> calls\n'
                '[[ "$*" != *"$FAIL_PACKAGE"* ]]\n')
            for failure in ('none', 'tilo-browser-peer', 'tilo-agent-participant'):
                with self.subTest(failure=failure):
                    (work / 'calls').unlink(missing_ok=True)
                    result = subprocess.run(['bash', '-c', stubs + script], cwd=work,
                                            env={**os.environ, 'FAIL_PACKAGE': failure,
                                                 'CHROME': BROWSER},
                                            text=True, capture_output=True)
                    self.assertEqual(result.returncode == 0, failure == 'none', result.stderr)
                    calls = (work / 'calls').read_text().splitlines()
                    self.assertEqual(len(calls), 1 if failure == 'tilo-browser-peer' else 2)
                    for call in calls:
                        self.assertIn('cargo nextest run --locked --profile ci --no-tests=fail', call)
            with self.subTest(failure='another browser'):
                result = subprocess.run(['bash', '-c', stubs + script], cwd=work,
                                        env={**os.environ, 'FAIL_PACKAGE': 'none',
                                             'CHROME': '/usr/bin/chromium'},
                                        text=True, capture_output=True)
                self.assertNotEqual(result.returncode, 0)

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
            # The legacy-apps check installs them before it drives them.
            subjects_script = 'scripts/legacy-app-subjects.sh'
            run = CHECKS['legacy-apps']['run'].splitlines()
            self.assertEqual(run[0], f'bash {subjects_script}')
            self.assertIn('scripts/legacy-app-caret-e2e.py', run[1])
            self.assertIn(subjects_script, CHECKS['legacy-apps']['inputs'])
            self.assertNotIn('os/Containerfile.builder', CHECKS['legacy-apps']['inputs'])
            script = (ROOT / subjects_script).read_text()
            # Execute the script with package installation and subject
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
        with self.subTest(phase="main_publishes_development_tags_only"):
            # Everyone pushes to main, so main's publication carries only dev- tags, signed with
            # the development key; the release key and the release tags are stable's alone.
            tags = published_tags(CHECKS['publish-ci-builder']['run'])
            self.assertEqual(set(tags), {'x86_64', 'aarch64', 'index'})
            for directory, pairs in tags.items():
                for pair in pairs:
                    channel, tag = pair.split(':', 1)
                    with self.subTest(directory=directory, tag=pair):
                        self.assertIn(channel, ('dev', 'release'))
                        if channel == 'dev':
                            self.assertTrue(tag.startswith('dev-{commit}'), tag)
                        self.assertNotEqual(tag, 'latest')
            self.assertEqual(tags['index'], ['dev:dev-{commit}', 'release:{commit}', 'release:{rust}'])
            self.assertEqual(CHECKS['sign-ci-builder']['on'], ['stable'])
            self.assertEqual(CHECKS['sign-ci-builder']['id_token'], ['sts.amazonaws.com'])
            self.assertEqual(CHECKS['sign-dev-ci-builder']['id_token'], ['tilo-dev-signing'])
            self.assertNotIn('stable', CHECKS['sign-dev-ci-builder']['on'])
        with self.subTest(phase="both_architectures_are_signed_and_published_together"):
            for name in PUBLICATION:
                needs = CHECKS[name]['needs']
                for need in ('image-ci-builder', *(f'image-ci-builder-{arch}' for arch in ARCHES)):
                    self.assertIn(need, needs, name)
                for arch in ARCHES:
                    self.assertIn(f'/target/publish/ci-builder/{arch}', CHECKS[name]['run'], name)
            self.assertEqual(CHECKS['image-ci-builder']['needs'],
                             [f'image-ci-builder-{arch}' for arch in ARCHES])
            for arch in ARCHES:
                self.assertEqual(CHECKS[f'image-ci-builder-{arch}']['requires'], [f'arch={arch}'])

    def resolve(self, requested):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'outputs'
            result = subprocess.run(
                [sys.executable, 'tools/ci/resolve_builder.py'],
                env={**os.environ, 'REQUESTED_BUILDER': requested, 'GITHUB_OUTPUT': str(output)},
                cwd=ROOT,
                text=True, capture_output=True,
            )
            return result, output.read_text() if output.exists() else ''


class CanaryIdentity(unittest.TestCase):
    def test_canaries_run_in_the_candidate_the_run_built(self):
        for arch in ARCHES:
            for kind in ('cache-cold', 'cache-warm', 'browser', 'x11-bridge'):
                name = f'canary-ci-builder-{kind}-{arch}'
                with self.subTest(check=name):
                    check = CHECKS[name]
                    self.assertEqual(check['image']['check'], f'image-ci-builder-{arch}')
                    self.assertEqual(check['image']['path'],
                                     f'/target/publish/ci-builder/{arch}/image.oci.tar')
                    self.assertEqual(check['requires'], [f'arch={arch}'])
                    # Acceptance runs the candidate's code: it holds no credential.
                    for key in ('id_token', 'secrets'):
                        self.assertNotIn(key, check)

    def test_warm_reads_what_cold_wrote_and_both_gate_publication(self):
        for arch in ARCHES:
            cold, warm = (CHECKS[f'canary-ci-builder-cache-{mode}-{arch}'] for mode in ('cold', 'warm'))
            self.assertEqual(cold['run'],
                             f'bash tools/ci/publish/ci-builder-canary.sh cold /target/publish/ci-builder/{arch}/canary')
            self.assertEqual(warm['run'],
                             f'bash tools/ci/publish/ci-builder-canary.sh warm /target/publish/ci-builder/{arch}/canary')
            self.assertIn(f'canary-ci-builder-cache-cold-{arch}', warm['needs'])
            self.assertIn(f'/target/publish/ci-builder/{arch}/canary/prefix', cold['outputs'])
            for name in PUBLICATION:
                with self.subTest(arch=arch, check=name):
                    self.assertIn(f'canary-ci-builder-cache-warm-{arch}', CHECKS[name]['needs'])
        for name in PUBLICATION:
            self.assertIn('canary-ci-builder-x11-bridge-x86_64', CHECKS[name]['needs'])
        cache = canary_arm('cold | warm')
        for refusal in ("(( failed == 0 )) || die",
                        "(( hits == 0 )) || die 'the cold canary hit entries under its fresh key'",
                        "(( misses == 0 )) || die 'the warm canary missed compiles the cold one wrote'",
                        "(( hits > 0 )) || die 'the warm canary got nothing from the cold one'"):
            self.assertIn(refusal, cache)
        # The CI hosts pull the builder from registry.tilo.so through their node cache;
        # nothing mirrors it into a cloud registry.
        for name in PUBLICATION:
            self.assertNotIn('dkr.ecr.', CHECKS[name]['run'])
        self.assertNotIn('dkr.ecr.', (ROOT / 'tools/ci/publish/ci-builder.sh').read_text())


if __name__ == '__main__':
    unittest.main()

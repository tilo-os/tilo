#!/usr/bin/env python3
"""The CI builder's candidate policy: its recipe, the canaries that run inside each candidate
(tools/ci/publish/ci-builder-canary.sh)."""

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
BROWSER = '/usr/lib64/chromium-browser/chromium-browser'


def canary_arm(mode):
    """The body of one `case` arm of ci-builder-canary.sh."""
    match = re.search(rf'^  {re.escape(mode)}\)\n(.*?)^    ;;$', CANARY, re.M | re.S)
    if match is None:
        raise AssertionError(f'ci-builder-canary.sh has no {mode} arm')
    return textwrap.dedent(match.group(1))


class BuilderCandidates(unittest.TestCase):
    def test_browser_recipe_and_native_acceptance(self):
        builder = (ROOT / 'os/Containerfile.builder').read_text()
        self.assertIn(f'CHROME={BROWSER}\n', builder)
        self.assertIn('install-build-deps.sh fedora test', builder)
        # The test profile installs the browser and fails the image build when
        # the executable CHROME names is missing or does not run.
        deps = (ROOT / 'dist/ci/install-build-deps.sh').read_text()
        runtime = deps.split('install_test_runtime_fedora() {\n', 1)[1].split('\n}\n', 1)[0]
        self.assertIn(f'    {BROWSER} --version\n', runtime)
        self.assertNotIn('chromium', builder.replace(BROWSER, ''))
        script = canary_arm('browser')
        self.assertIn(f'test "${{CHROME:-}}" = {BROWSER}', script)
        self.assertIn("import {launch} from './tools/web/chrome.mjs'", script)
        self.assertIn("assert.equal(await tab.evaluate('6 * 7'), 42)", script)
        self.assertIn('await browser.close()', script)

    def test_native_wrapper_exports_browser_outside_home_and_keeps_overrides(self):
        wrapper = ROOT / 'tools/ci/nextest.sh'
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'tools/ci').mkdir(parents=True)
            script = root / 'tools/ci/nextest.sh'
            script.write_text(wrapper.read_text())
            (root / 'tools/ci/package_features.py').write_text(
                (ROOT / 'tools/ci/package_features.py').read_text())
            bin_dir = root / 'bin'
            bin_dir.mkdir()
            uname = bin_dir / 'uname'
            uname.write_text('#!/bin/sh\nprintf "%s\\n" "$PLATFORM"\n')
            uname.chmod(0o755)
            # Stand in only for the downstream command. The real wrapper must pass the browser
            # through to the test invocation. It builds a helper binary for this package before
            # testing it; building is not what is checked here, and the check runs where nothing
            # builds.
            cargo = bin_dir / 'cargo'
            cargo.write_text('#!/bin/sh\n[ "$1" = nextest ] && printf "%s\\n" "${CHROME-unset}"\nexit 0\n')
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
            (work / 'tools/ci/nextest.sh').write_text(
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


if __name__ == '__main__':
    unittest.main()

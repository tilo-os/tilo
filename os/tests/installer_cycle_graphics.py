#!/usr/bin/python3
"""Validate actual cycle option parsing and both VM boot argument paths."""
from pathlib import Path
import subprocess
import hashlib
import os
import re
import tempfile
import unittest

SCRIPT = (Path(__file__).resolve().parents[1] / 'dev/test-installer-cycle.sh').read_text()


class CycleGraphicsTests(unittest.TestCase):
    def test_shot_ocr_initializes_its_own_name_under_nounset(self):
        library = Path(__file__).resolve().parents[1] / 'dev/installer-cycle-lib.sh'
        with tempfile.TemporaryDirectory() as temporary:
            script = '''set -euo pipefail
source "$1"
run="$2"; out="$2"; vm=fixture
fixture() { printf frame > "$run/$2.png"; }
tesseract() { printf ready; }
shot_ocr fresh
'''
            result = subprocess.run(['bash', '-c', script, 'test', str(library), temporary],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), str(Path(temporary) / 'fresh.png'))
            self.assertEqual((Path(temporary) / 'fresh.txt').read_text(), 'ready')

    def test_screen_observation_rejects_capture_ocr_and_artifact_failures(self):
        library = Path(__file__).resolve().parents[1] / 'dev/installer-cycle-lib.sh'
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            for failure in ('shot', 'ocr', 'copy'):
                with self.subTest(failure=failure):
                    script = r'''
set -euo pipefail
source "$1"
run="$2"; out="$2"; vm=fixture
fixture() {
  if [ "$1" = shot ]; then
    [ "$failure" != shot ] || return 7
    printf 'frame' > "$run/$2.png"
  else
    printf 'stopped\n'
  fi
}
tesseract() { printf 'ready\n'; [ "$failure" != ocr ]; }
cp() { [ "$failure" != copy ] && command cp "$@"; }
failure="$3"
name=observe
printf 'old frame' > "$run/observe.png"
if wait_screen result ready 1 0; then exit 91; fi
'''
                    result = subprocess.run(['bash', '-c', script, 'test', str(library), str(base), failure],
                                            capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_graphics_selection_and_encoder_pin(self):
        with self.subTest(phase="selected_graphics_reaches_every_boot_with_either_tpm_mode"):
            parser = SCRIPT[SCRIPT.index('medium="${1:?'):SCRIPT.index('[ -f "$medium" ]')]
            # The medium's trust decides whether the installer's preflight needs
            # the proof argument on its own: a development-unsigned medium does,
            # while a release medium needs it only for the separately signed
            # target the caller pins. The fixture medium does not exist, so this
            # read is the release case.
            medium_trust = SCRIPT[SCRIPT.index('medium_trust="$(python3'):SCRIPT.index('proof_arg=()')]
            proof_selection = SCRIPT[SCRIPT.index('proof_arg=()'):SCRIPT.index('if [ "$software_tpm" -eq 1 ]; then', SCRIPT.index('proof_arg=()'))]
            # Every boot passes the selected graphics flag. A boot may name
            # more after its TPM mode (the one that serves its serial line as a
            # socket, for instance), so the tail is captured rather than
            # assumed to end at the proof argument.
            pattern = re.compile(
                r'if \[ "\$software_tpm" -eq 1 \]; then\n'
                r'  "\$vm" (boot-(?:installer|target)) "\$display_arg" --tpm( [^\n]*)?\n'
                r'else\n'
                r'  "\$vm" \1 "\$display_arg" --no-tpm( [^\n]*)?\n'
                r'fi')
            matches = list(pattern.finditer(SCRIPT))
            self.assertEqual([match.group(1) for match in matches],
                             ['boot-installer', 'boot-target', 'boot-target', 'boot-target'])
            # Only the installer's boot carries the install proof; only the
            # first installed boot serves its serial line; the rest name no
            # further argument.
            self.assertEqual(
                [((match.group(2) or '').strip(), (match.group(3) or '').strip()) for match in matches],
                [('"${proof_arg[@]}"', '"${proof_arg[@]}"'),
                 ('--serial-socket', '--serial-socket'), ('', ''), ('', '')])
            blocks = [match.group(0) for match in matches]
            script = ('set -euo pipefail\n' + parser + '\n' + medium_trust + '\n' + proof_selection
                      + '\nvm=fixture\nfixture() { printf "%s|%s|%s|%s\\n" "$1" "$2" "$3" "${4:-}"; }\n'
                      + '\n'.join(blocks))
            for graphics, flag in (('gl', '--gl'), ('software', '--2d')):
                for tpm in (False, True):
                    for signed in (False, True):
                        args = ['bash', '-c', script, 'cycle', '/fixture/medium', '--graphics', graphics]
                        if tpm:
                            args.append('--software-tpm')
                        env = dict(os.environ)
                        env.pop('TILO_EXPECTED_INSTALLED_REF', None)
                        if signed:
                            args.append('--guided-alongside')
                            env['TILO_EXPECTED_INSTALLED_REF'] = 'registry.tilo.so/tilo/tilo@sha256:' + 'a' * 64
                        result = subprocess.run(args, text=True, capture_output=True, env=env)
                        self.assertEqual(result.returncode, 0, result.stderr)
                        tpm_flag = '--tpm' if tpm else '--no-tpm'
                        proof_flag = '--install-proof' if signed else ''
                        self.assertEqual(result.stdout.splitlines(),
                            [f'boot-installer|{flag}|{tpm_flag}|{proof_flag}',
                             f'boot-target|{flag}|{tpm_flag}|--serial-socket',
                             f'boot-target|{flag}|{tpm_flag}|',
                             f'boot-target|{flag}|{tpm_flag}|'])
            result = subprocess.run(['bash', '-c', script, 'cycle', '/fixture/medium', '--graphics', 'bad'],
                                    text=True, capture_output=True)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(result.stdout, '')
        with self.subTest(phase="prebuilt_encoder_pin_and_explicit_local_fallback"):
            start = SCRIPT.index('encoder="${TILO_PROFILE_ENCODER:-}"')
            end = SCRIPT.index('command -v tesseract', start)
            admission = SCRIPT[start:end]
            with tempfile.TemporaryDirectory() as temporary:
                encoder = Path(temporary) / 'profile-encoder'
                encoder.write_text('#!/bin/sh\nprintf "encoder:%s\\n" "$1"\n')
                encoder.chmod(0o755)
                digest = hashlib.sha256(encoder.read_bytes()).hexdigest()
                script = 'set -euo pipefail\nbuild_profile_encoder=0\ndev_dir=/fixture/os/dev\n' + admission + '\n"${encoder_command[@]}" fixture-argument\n'
                env = dict(os.environ, TILO_PROFILE_ENCODER=str(encoder), TILO_PROFILE_ENCODER_SHA256=digest)
                good = subprocess.run(['bash', '-c', script], env=env, capture_output=True, text=True)
                self.assertEqual(good.returncode, 0, good.stderr)
                self.assertEqual(good.stdout.strip(), 'encoder:fixture-argument')
                env['TILO_PROFILE_ENCODER_SHA256'] = '0' * 64
                bad = subprocess.run(['bash', '-c', script], env=env, capture_output=True, text=True)
                self.assertEqual(bad.returncode, 2)
                self.assertEqual(bad.stdout, '')
                self.assertIn('SHA256 differs', bad.stderr)
                local_script = 'set -euo pipefail\nbuild_profile_encoder=1\ndev_dir=/fixture/os/dev\n' + admission + '\nprintf "%s\\n" "${encoder_command[@]}"\n'
                clean = dict(os.environ)
                clean.pop('TILO_PROFILE_ENCODER', None)
                clean.pop('TILO_PROFILE_ENCODER_SHA256', None)
                local = subprocess.run(['bash', '-c', local_script], env=clean, capture_output=True, text=True)
                self.assertEqual(local.returncode, 0, local.stderr)
                self.assertIn('/tools/imager-profile/Cargo.toml', local.stdout)
                self.assertNotIn('--example', local.stdout)


if __name__ == '__main__':
    unittest.main()

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
    def test_graphics_selection_and_encoder_pin(self):
        with self.subTest(phase="selected_graphics_reaches_every_boot_with_either_tpm_mode"):
            parser = SCRIPT[SCRIPT.index('medium="${1:?'):SCRIPT.index('[ -f "$medium" ]')]
            proof_selection = SCRIPT[SCRIPT.index('proof_arg=()'):SCRIPT.index('if [ "$software_tpm" -eq 1 ]; then', SCRIPT.index('proof_arg=()'))]
            pattern = re.compile(
                r'if \[ "\$software_tpm" -eq 1 \]; then\n'
                r'  "\$vm" (boot-(?:installer|target)) "\$display_arg" --tpm( "\$\{proof_arg\[@\]\}")?\n'
                r'else\n'
                r'  "\$vm" \1 "\$display_arg" --no-tpm( "\$\{proof_arg\[@\]\}")?\n'
                r'fi')
            matches = list(pattern.finditer(SCRIPT))
            self.assertEqual([match.group(1) for match in matches],
                             ['boot-installer', 'boot-target', 'boot-target', 'boot-target'])
            self.assertEqual([(bool(match.group(2)), bool(match.group(3))) for match in matches],
                             [(True, True), (False, False), (False, False), (False, False)])
            blocks = [match.group(0) for match in matches]
            script = ('set -euo pipefail\n' + parser + '\n' + proof_selection
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
                            [f'boot-installer|{flag}|{tpm_flag}|{proof_flag}']
                            + [f'boot-target|{flag}|{tpm_flag}|'] * 3)
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

#!/usr/bin/python3
"""Validate actual cycle option parsing and both VM boot argument paths."""
from pathlib import Path
import subprocess
import unittest

SCRIPT = (Path(__file__).resolve().parents[1] / 'dev/test-installer-cycle.sh').read_text()


class CycleGraphicsTests(unittest.TestCase):
    def test_selected_graphics_reaches_both_boots_with_either_tpm_mode(self):
        parser = SCRIPT[SCRIPT.index('medium="${1:?'):SCRIPT.index('[ -f "$medium" ]')]
        blocks = []
        for boot in ('boot-installer', 'boot-target'):
            start = SCRIPT.index('if [ "$software_tpm" -eq 1 ]; then\n  "$vm" ' + boot)
            end = SCRIPT.index('\nfi', start) + 3
            blocks.append(SCRIPT[start:end])
        script = 'set -euo pipefail\n' + parser + '\nvm=fixture\nfixture() { printf "%s|%s|%s\\n" "$1" "$2" "$3"; }\n' + '\n'.join(blocks)
        for graphics, flag in (('gl', '--gl'), ('software', '--2d')):
            for tpm in (False, True):
                args = ['bash', '-c', script, 'cycle', '/fixture/medium', '--graphics', graphics]
                if tpm:
                    args.append('--software-tpm')
                result = subprocess.run(args, text=True, capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                tpm_flag = '--tpm' if tpm else '--no-tpm'
                self.assertEqual(result.stdout.splitlines(),
                    [f'boot-installer|{flag}|{tpm_flag}', f'boot-target|{flag}|{tpm_flag}'])
        result = subprocess.run(['bash', '-c', script, 'cycle', '/fixture/medium', '--graphics', 'bad'],
                                text=True, capture_output=True)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, '')


if __name__ == '__main__':
    unittest.main()

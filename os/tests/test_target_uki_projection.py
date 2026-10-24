"""Execute the real target-selection projection block with synthetic files."""
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'dev/sign-target-prediction.sh'


class ProjectionTests(unittest.TestCase):
    def test_projection_program(self):
        with self.subTest(phase="embedded_image_export_is_one_complete_shell_program"):
            script = SCRIPT.read_text()
            start = "podman run --rm -v \"$work:/out:Z\" --entrypoint /bin/bash \"$image\" -ceu '\n"
            end = "\n'\n  generation=\"$(tr -d '[:space:]' < \"$work/generation\")\""
            self.assertEqual(script.count(start), 1)
            payload = script.split(start, 1)[1].split(end, 1)[0]
            self.assertNotIn("'", payload, 'single quote terminates the outer shell argument')
            subprocess.run(['bash', '-n', '-c', payload], check=True)
        with self.subTest(phase="both_paths_project_the_selected_verified_target"):
            script = SCRIPT.read_text()
            block = script[script.index('  target_source="$(cat'):script.index('  measure target.efi.signed')]
            for selected in ('1.efi', 'separate'):
                with self.subTest(selected=selected), tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary)
                    (root / 'live').mkdir()
                    (root / 'target-source').write_text(selected)
                    (root / 'target.efi').write_bytes(b'unsigned-target')
                    (root / 'live/1.efi.signed').write_bytes(b'validated-live-target')
                    signed = b'signed-target' if selected == 'separate' else b'validated-live-target'
                    (root / 'target.efi.signed').write_bytes(signed)
                    setup = '''set -euo pipefail
work="$1"
die() { exit 1; }
verify_signed_efi() { test -s "$2"; }
'''
                    subprocess.run(['bash', '-c', setup + block, 'projection', str(root)], check=True)
                    self.assertEqual((root / 'layer/boot/tilo-target/tilo-target.efi').read_bytes(), signed)
                    if selected != 'separate':
                        (root / 'target.efi.signed').write_bytes(b'another-uki')
                        result = subprocess.run(['bash', '-c', setup + block, 'projection', str(root)])
                        self.assertNotEqual(result.returncode, 0, 'a target that is not its signed live UKI')
        with self.subTest(phase="ambiguous_live_set_rejected_by_actual_guard"):
            line = next(line.strip() for line in SCRIPT.read_text().splitlines()
                        if 'explicit target UKI required when multiple live UKIs exist' in line)
            for count, accepted in ((1, True), (2, False)):
                result = subprocess.run(['bash', '-c', 'n="$1"\n' + line, 'selection', str(count)], capture_output=True)
                self.assertEqual(result.returncode == 0, accepted)

    def test_image_crypto(self):
        with self.subTest(phase="crypto_checks_use_the_image_tilo_crypto"):
            script = SCRIPT.read_text()
            tools = next(line for line in script.splitlines() if line.lstrip().startswith('for tool in '))
            self.assertNotIn('tilo-crypto', tools, 'native release runners carry no host tilo-crypto')
            for number, line in enumerate(script.splitlines(), 1):
                code = line.split('#', 1)[0]
                self.assertNotRegex(code, r'(^|\$\(|[;&|]\s*)\s*tilo-crypto\s',
                                    f'line {number} runs a host tilo-crypto')
        with self.subTest(phase="image_crypto_maps_workspace_paths_read_only"):
            script = SCRIPT.read_text()
            start = script.index('image_crypto() {')
            function = script[start:script.index('\n}\n', start) + 3]
            setup = '''set -euo pipefail
work="$1"
image=candidate
die() { printf 'die: %s\\n' "$*"; exit 3; }
podman() { printf '%s\\n' "$@"; }
'''
            with tempfile.TemporaryDirectory() as temporary:
                accepted = subprocess.run(
                    ['bash', '-c', setup + function
                     + 'image_crypto verify-rsa-sha256 "$work/public.pem" "$work/layer/boot/x.sig"',
                     'crypto', temporary], capture_output=True, text=True, check=True)
                self.assertEqual(accepted.stdout.splitlines(), [
                    'run', '--rm', '-v', f'{temporary}:/work:ro,Z',
                    '--entrypoint', '/usr/libexec/tilo/tilo-crypto', 'candidate',
                    'verify-rsa-sha256', '/work/public.pem', '/work/layer/boot/x.sig'])
                outside = subprocess.run(
                    ['bash', '-c', setup + function + 'image_crypto certificate-sha256 /etc/passwd',
                     'crypto', temporary], capture_output=True, text=True)
                self.assertEqual(outside.returncode, 3)
                self.assertIn('outside the signing workspace', outside.stdout)


if __name__ == '__main__':
    unittest.main()

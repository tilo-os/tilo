"""Execute the real target-selection projection block with synthetic files."""
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'dev/sign-target-prediction.sh'


class ProjectionTests(unittest.TestCase):
    def test_embedded_image_export_is_one_complete_shell_program(self):
        script = SCRIPT.read_text()
        start = "podman run --rm -v \"$work:/out:Z\" --entrypoint /bin/bash \"$image\" -ceu '\n"
        end = "\n'\ngeneration=\"$(tr -d '[:space:]' < \"$work/generation\")\""
        self.assertEqual(script.count(start), 1)
        payload = script.split(start, 1)[1].split(end, 1)[0]
        self.assertNotIn("'", payload, 'single quote terminates the outer shell argument')
        subprocess.run(['bash', '-n', '-c', payload], check=True)

    def test_both_paths_project_the_selected_verified_target(self):
        script = SCRIPT.read_text()
        block = script[script.index('target_source="$(cat'):script.index('# systemd-measure is')]
        for selected in ('1.efi', 'separate'):
            with self.subTest(selected=selected), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / 'live').mkdir()
                (root / 'target-source').write_text(selected)
                (root / 'target.efi').write_bytes(b'unsigned-target')
                (root / 'live/1.efi.signed').write_bytes(b'validated-live-target')
                setup = '''set -euo pipefail
work="$1"
die() { exit 1; }
fixture_signer() { cp "$2" "$3"; }
signer=fixture_signer
verify_signed_efi() { cmp "$1" "$2"; }
'''
                subprocess.run(['bash', '-c', setup + block, 'projection', str(root)], check=True)
                expected = b'unsigned-target' if selected == 'separate' else b'validated-live-target'
                self.assertEqual((root / 'layer/boot/tilo-target/tilo-target.efi').read_bytes(), expected)

    def test_ambiguous_live_set_rejected_by_actual_guard(self):
        line = next(line.strip() for line in SCRIPT.read_text().splitlines()
                    if 'explicit target UKI required when multiple live UKIs exist' in line)
        for count, accepted in ((1, True), (2, False)):
            result = subprocess.run(['bash', '-c', 'n="$1"\n' + line, 'selection', str(count)], capture_output=True)
            self.assertEqual(result.returncode == 0, accepted)


if __name__ == '__main__':
    unittest.main()

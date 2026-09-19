"""Execute the actual release invariant, including disconnected-builder negatives."""
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class AppReleaseWorkflowContract(unittest.TestCase):
    def check(self, workflow):
        script = (ROOT / 'os/tests/release_pipeline_invariants.sh').read_text()
        start = script.index("grep -Fq 'release/app-collections.toml' \"$apps\"")
        end = script.index("\ngrep -Fq 'TILO_APP_COLLECTION_SHA256'", start)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'release-apps.yml'
            path.write_bytes(workflow.encode())
            return subprocess.run(['bash', '-c',
                'set -euo pipefail\napps=$1\nok() { :; }\nbad() { exit 1; }\n'
                + script[start:end], 'contract', str(path)], capture_output=True).returncode

    def test_actual_immutable_builder_contract_passes(self):
        self.assertEqual(self.check((ROOT / '.forgejo/workflows/release-apps.yml').read_text()), 0)

    def test_crlf_and_large_job_do_not_create_false_failures(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        source = source.replace('    outputs:\n', '    # ' + 'x' * 131072 + '\n    outputs:\n', 1)
        self.assertEqual(self.check(source), 0)
        self.assertEqual(self.check(source.replace('\n', '\r\n')), 0)

    def test_disconnected_or_mutable_builder_fails(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        for before, after in (
            ('      builder_image: ${{ steps.builder.outputs.image }}', '      other: unused'),
            ('        id: builder', '        id: different'),
            ('        run: python3 tools/resolve_ci_builder.py', '        run: echo skipped'),
            ('    needs: plan', '    needs: other'),
            ('      image: ${{ needs.plan.outputs.builder_image }}',
             '      image: code.sandbox.example/qos/tilo-builder:latest'),
        ):
            with self.subTest(before=before):
                self.assertIn(before, source)
                self.assertNotEqual(self.check(source.replace(before, after, 1)), 0)

    def test_required_image_reference_in_comment_cannot_replace_build_input(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        reference = '      image: ${{ needs.plan.outputs.builder_image }}'
        self.assertNotEqual(self.check(source.replace(reference, '# ' + reference)), 0)


if __name__ == '__main__':
    unittest.main()

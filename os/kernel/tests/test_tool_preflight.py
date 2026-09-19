"""Missing tools must stop the actual workflow preflight, even before its last item."""
from pathlib import Path
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[3]


class ToolPreflight(unittest.TestCase):
    def check_missing(self, missing):
        workflow = (ROOT / '.forgejo/workflows/development-kernel-acceptance.yml').read_text()
        start = workflow.index('          for tool in modinfo ')
        end = workflow.index('          done\n', start) + len('          done\n')
        production = workflow[start:end]
        # Model one unavailable command while leaving all other commands present.
        setup = '''set -euo pipefail
command() {
  test "$1" = -v
  test "$2" != "$MISSING_TOOL"
}
'''
        result = subprocess.run(['bash', '-c', setup + production + '\necho reached-acceptance'],
                                env={'MISSING_TOOL': missing, 'PATH': '/usr/bin:/bin'},
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn('missing acceptance tool: ' + missing, result.stderr)
        self.assertNotIn('reached-acceptance', result.stdout)

    def test_missing_first(self): self.check_missing('modinfo')
    def test_missing_middle(self): self.check_missing('rpm2cpio')
    def test_missing_last(self): self.check_missing('qemu-system-x86_64')

    def test_sbin_tool_visible_in_production_script(self):
        script = (ROOT / 'os/kernel/tests/test-rpm.sh').read_text()
        preflight = script.split('rpm_file=', 1)[0]
        # This environment has kmod in sbin; a minimal runner PATH must find it.
        result = subprocess.run(['bash', '-c', preflight + '\ncommand -v modinfo\nmodinfo --version'],
                                env={'PATH': '/usr/bin:/bin'}, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('modinfo', result.stdout)


if __name__ == '__main__':
    unittest.main()

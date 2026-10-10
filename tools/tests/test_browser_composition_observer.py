"""Run the production composition observer's hermetic lifecycle regressions."""
from pathlib import Path
import subprocess
import unittest


class BrowserCompositionObserver(unittest.TestCase):
    def test_original_editor_lifecycle_and_native_composition_guard(self):
        root = Path(__file__).resolve().parents[2]
        result = subprocess.run(
            ['node', str(root / 'apps/browser/chromium/tests/composition_observer.mjs')],
            cwd=root, capture_output=True, text=True, timeout=15,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == '__main__':
    unittest.main()

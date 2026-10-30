"""Execution of the hermetic suites outside Cargo (tools/ci/suites.py run).

Apart from tools/tests/test_suites.py, which reads the tree through Git: these run Node.js
(tools/policy/steps.toml, `node_tests`) and read no Git checkout.
"""
from __future__ import annotations

import importlib.util
import io
from pathlib import Path
import sys
import tempfile
import textwrap
import unittest
from contextlib import redirect_stdout

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("ci_suites", ROOT / "tools/ci/suites.py")
assert SPEC and SPEC.loader
suites = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = suites  # dataclasses resolve annotations through the module
SPEC.loader.exec_module(suites)


class RunTests(unittest.TestCase):
    def run_suites(self, files: dict[str, str], names: list[str]) -> tuple[int, str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            listed = [name for name in files if name.endswith((".py", ".sh", ".test.mjs"))]
            (root / "tools/ci").mkdir(parents=True)
            (root / suites.MANIFEST).write_text(
                '[[group]]\nname = "g"\ninputs = ["x/"]\nsuites = [%s]\n'
                % ", ".join(f'"{name}"' for name in listed))
            for name, text in files.items():
                (root / name).parent.mkdir(parents=True, exist_ok=True)
                (root / name).write_text(textwrap.dedent(text))
            output = io.StringIO()
            with redirect_stdout(output):
                code = suites.run(root, names, jobs=2)
            return code, output.getvalue()

    def test_selected_suites_all_run_from_root_and_any_failure_fails(self):
        with self.subTest(phase='all suites run and one failure fails the run'):
            code, output = self.run_suites(
                {"x/ok.sh": "echo fine\n", "x/bad.py": "raise SystemExit(3)\n"},
                ["x/ok.sh", "x/bad.py"])
            self.assertEqual(code, 1)
            self.assertIn("x/ok.sh: ok", output)
            self.assertIn("x/bad.py: FAILED (exit 3)", output)
            self.assertIn("1/2 suites passed", output)
        with self.subTest(phase='suites run from the repository root'):
            code, _ = self.run_suites({"x/where.sh": "test -f tools/ci/suites.toml\n"}, ["x/where.sh"])
            self.assertEqual(code, 0)
        with self.subTest(phase='suites run without the caller\'s desktop session'):
            code, output = self.run_suites(
                {"x/session.sh": 'test -z "${WAYLAND_DISPLAY:-}" && test -n "$TILO_TEST_ENVIRONMENT" '
                                 '&& test "$XDG_RUNTIME_DIR" = "$TILO_TEST_ENVIRONMENT/runtime"\n'},
                ["x/session.sh"])
            self.assertEqual(code, 0, output)
        with self.subTest(phase='JavaScript suites run under node --test'):
            code, output = self.run_suites(
                {"x/ok.test.mjs": "import test from 'node:test';\ntest('passes', () => {});\n",
                 "x/bad.test.mjs": "import test from 'node:test';\n"
                                   "test('fails', () => { throw new Error('no'); });\n"},
                ["x/ok.test.mjs", "x/bad.test.mjs"])
            self.assertEqual(code, 1, output)
            self.assertIn("x/ok.test.mjs: ok", output)
            self.assertIn("x/bad.test.mjs: FAILED", output)
        with self.subTest(phase='undeclared or empty selection fails'):
            self.assertEqual(self.run_suites({"x/ok.sh": "true\n"}, ["x/other.sh"])[0], 2)
            self.assertEqual(self.run_suites({"x/ok.sh": "true\n"}, [])[0], 2)


if __name__ == "__main__":
    unittest.main()

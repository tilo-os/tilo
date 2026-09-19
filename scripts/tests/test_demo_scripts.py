from pathlib import Path
import unittest


DEMO = Path(__file__).resolve().parents[1] / "demo"


class DemoScriptTests(unittest.TestCase):
    def test_python_entrypoints_parse(self) -> None:
        entrypoints = sorted(DEMO.glob("*.py"))
        self.assertTrue(entrypoints)
        for path in entrypoints:
            with self.subTest(path=path.name):
                source = path.read_text(encoding="utf-8")
                compile(source, path, "exec")

    def test_runner_keeps_all_required_debug_targets(self) -> None:
        source = (DEMO / "run.py").read_text(encoding="utf-8")
        self.assertIn("shell::selection=debug", source)
        self.assertIn("file_explorer::drag=debug", source)
        self.assertIn("tilo.state=debug", source)


if __name__ == "__main__":
    unittest.main()

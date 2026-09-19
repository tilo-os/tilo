from pathlib import Path
import unittest


SCRIPTS = Path(__file__).resolve().parents[1]
DEMO = SCRIPTS / "demo"
VISUAL = SCRIPTS / "visual"


class ScriptEntrypointTests(unittest.TestCase):
    def test_python_entrypoints_parse(self) -> None:
        entrypoints = (
            sorted(SCRIPTS.glob("*.py"))
            + sorted(DEMO.glob("*.py"))
            + sorted(VISUAL.glob("*.py"))
        )
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

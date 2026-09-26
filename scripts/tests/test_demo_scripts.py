from pathlib import Path
import unittest


DEMO = Path(__file__).resolve().parents[1] / "demo"


class DemoScriptTests(unittest.TestCase):
    def test_demo_entrypoints_parse_and_runner_keeps_debug_targets(self):
        with self.subTest(phase='python entrypoints parse'):
            entrypoints = sorted(DEMO.glob("*.py"))
            self.assertTrue(entrypoints)
            for path in entrypoints:
                with self.subTest(path=path.name):
                    source = path.read_text(encoding="utf-8")
                    compile(source, path, "exec")
        with self.subTest(phase='runner keeps all required debug targets'):
            source = (DEMO / "run.py").read_text(encoding="utf-8")
            self.assertIn("shell::selection=debug", source)
            self.assertIn("file_explorer::drag=debug", source)
            self.assertIn("tilo.state=debug", source)


if __name__ == "__main__":
    unittest.main()

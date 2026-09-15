import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


class SuiteTests(unittest.TestCase):
    def test_coordinate_routes_do_not_reuse_compact_layout(self):
        spec = importlib.util.spec_from_file_location("suite", Path(__file__).with_name("run-suite.py"))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        scenario = json.loads(Path(__file__).with_name("workspace-settings.json").read_text())
        names = [name for name, _ in module.variants(scenario, True)]
        self.assertNotIn("compact", names)
        self.assertNotIn("large-text", names)
        self.assertIn("retina", names)

    def test_missing_evidence_is_an_error_even_when_process_succeeds(self):
        spec = importlib.util.spec_from_file_location("suite", Path(__file__).with_name("run-suite.py"))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as directory:
            result = module.read_result(Path(directory), 0, "successful process")
            self.assertEqual(result["status"], "error")

    def test_ux_failures_are_retained_separately_from_execution_errors(self):
        spec = importlib.util.spec_from_file_location("suite", Path(__file__).with_name("run-suite.py"))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            (out / "report.json").write_text(json.dumps({"passed": False, "failures": ["click budget exceeded"], "counts": {"clicks": 4}}))
            (out / "index.html").write_text("report")
            result = module.read_result(out, 1, "")
            self.assertEqual(result["status"], "fail")
            self.assertEqual(result["failures"], ["click budget exceeded"])


if __name__ == "__main__":
    unittest.main()

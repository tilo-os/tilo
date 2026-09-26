import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


class SuiteTests(unittest.TestCase):
    def test_all_eight_goals_execute_all_six_variants_without_budget_changes(self):
        spec = importlib.util.spec_from_file_location("suite", Path(__file__).with_name("run-suite.py"))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        sources = sorted(Path(__file__).parent.glob("*.json"))
        self.assertEqual(len(sources), 8)
        for source in sources:
            scenario = json.loads(source.read_text())
            cases = list(module.variants(scenario, True))
            self.assertEqual([name for name, _ in cases],
                             ["baseline", "compact", "light", "high-contrast", "retina", "large-text"], source.name)
            for name, case in cases:
                self.assertEqual(case["budget"], scenario["budget"])
                self.assertEqual(case["goal"], scenario["goal"])
                for index, step in enumerate(case["steps"]):
                    if step["op"] == "input":
                        self.assertNotIn("click_at", step["action"])
                    if step["op"] == "wheel":
                        self.assertIn("target", step)
                        self.assertEqual(step["delta"][1], (2 if name == "retina" else 1) * scenario["steps"][index]["delta"][1])
                    for check in step.get("checks", []):
                        if check["kind"] == "contrast":
                            self.assertEqual(check["min"], 4.5)
                            for sample in (check["foreground"], check["background"]):
                                self.assertTrue(0 <= sample[0] < case["viewport"][0])
                                self.assertTrue(0 <= sample[1] < case["viewport"][1])

    def test_read_result_requires_evidence_and_keeps_ux_failures_separate(self):
        with self.subTest(phase='missing evidence is an error even when process succeeds'):
            spec = importlib.util.spec_from_file_location("suite", Path(__file__).with_name("run-suite.py"))
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            with tempfile.TemporaryDirectory() as directory:
                result = module.read_result(Path(directory), 0, "successful process")
                self.assertEqual(result["status"], "error")
        with self.subTest(phase='success without inspection or resource evidence is an error'):
            spec = importlib.util.spec_from_file_location("suite", Path(__file__).with_name("run-suite.py"))
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            with tempfile.TemporaryDirectory() as directory:
                out = Path(directory)
                (out / "report.json").write_text(json.dumps({"passed": True, "failures": [], "counts": {}, "frames": [{"name": "result", "screenshot": "result.png"}]}))
                (out / "index.html").write_text("report")
                (out / "result.png").write_bytes(b"evidence")
                self.assertEqual(module.read_result(out, 0, "")["status"], "error")
                (out / "result.inspection.json").write_text("{}")
                self.assertEqual(module.read_result(out, 0, "")["status"], "error")
                (out / "result.resources.json").write_text("[]")
                self.assertEqual(module.read_result(out, 0, "")["status"], "pass")
        with self.subTest(phase='ux failures are retained separately from execution errors'):
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

"""Unit tests for the failed checks' evidence printed into the log.

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'
"""

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import report  # noqa: E402
from acceptance.core import Result  # noqa: E402


class FailureEvidence(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.out = Path(self.tmp.name)
        (self.out / "data").mkdir()
        (self.out / "errors").mkdir()
        (self.out / "shots").mkdir()

    def tearDown(self):
        self.tmp.cleanup()

    def result(self, id, status, *evidence):
        return Result(id, "basics", id, status=status, evidence=list(evidence))

    def test_prints_failed_checks_text_and_skips_passes_and_images(self):
        (self.out / "errors" / "B03.txt").write_text("Traceback\n  File x\nTypeError: boom\n")
        (self.out / "data" / "R06-boot-gate.txt").write_text("GATE-EXIT=0\n")
        (self.out / "shots" / "B03.png").write_bytes(b"\x89PNG")
        text = report.failure_evidence(self.out, [
            self.result("B03", "fail", "errors/B03.txt", "shots/B03.png"),
            self.result("R06", "pass", "data/R06-boot-gate.txt"),
        ])
        self.assertIn("==== B03: errors/B03.txt\nTraceback", text)
        self.assertIn("TypeError: boom", text)
        self.assertNotIn("R06", text)
        self.assertNotIn("PNG", text)

    def test_long_log_keeps_its_end_and_json_its_start_compact(self):
        (self.out / "data" / "A01-ush.txt").write_text("x" * 10000 + "the refusal\n")
        (self.out / "data" / "R01-map.json").write_text(
            json.dumps({"summary": "first", "rows": ["r"] * 5000}, indent=1))
        text = report.failure_evidence(self.out, [
            self.result("A01", "fail", "data/A01-ush.txt"),
            self.result("R01", "fail", "data/R01-map.json"),
        ])
        self.assertIn("A01-ush.txt (last 4096 of 10012 bytes)", text)
        self.assertIn("the refusal", text)
        self.assertIn('{"summary":"first","rows":', text)
        self.assertIn("(first 4096 of", text)

    def test_the_whole_is_bounded(self):
        results = []
        for i in range(80):
            for j in range(3):
                (self.out / "data" / f"{i}-{j}.log").write_text("Q" * 5000)
            results.append(self.result(f"C{i:02}", "fail", *(f"data/{i}-{j}.log" for j in range(3))))
        text = report.failure_evidence(self.out, results)
        self.assertLessEqual(text.count("Q"), report.EVIDENCE_TOTAL_BYTES)
        self.assertIn("the run's printed evidence reached", text)

    def test_each_check_has_its_own_share(self):
        # A first check with many large files leaves the next check its own share.
        big = [f"data/R01-{i}.log" for i in range(10)]
        for rel in big:
            (self.out / rel).write_text("m" * 5000)
        (self.out / "data" / "I02-keyboards.json").write_text('{"compositor": "no profile keyboard"}')
        r01 = self.result("R01", "fail", *big)
        i02 = self.result("I02", "fail", "data/I02-keyboards.json")
        text = report.failure_evidence(self.out, [r01, i02])
        self.assertIn("R01's printed evidence reached", text)
        r01_text = text[:text.index("==== I02")]
        self.assertLessEqual(r01_text.count("m"), report.EVIDENCE_CHECK_BYTES)
        self.assertIn('{"compositor":"no profile keyboard"}', text)

    def test_a_checks_diagnosis_comes_before_its_bulky_files(self):
        for rel, body in (("data/R01-shell-memory-map.json", '{"rows": 1}'),
                          ("data/R01-samples.csv", "a,b\n1,2\n"),
                          ("data/R01-idle-empty.json", '{"cpu": 1}'),
                          ("data/R01-named.txt", "the cause"),
                          ("errors/R01.txt", "Traceback")):
            (self.out / rel).write_text(body)
        r = self.result("R01", "fail", "data/R01-shell-memory-map.json", "data/R01-samples.csv",
                        "data/R01-idle-empty.json", "data/R01-named.txt", "errors/R01.txt")
        r.detail = "over budget (R01-named.txt names the cause)"
        order = [line.split(": ", 1)[1].split(" ")[0]
                 for line in report.failure_evidence(self.out, [r]).splitlines()
                 if line.startswith("==== ")]
        self.assertEqual(order[:2], ["data/R01-named.txt", "errors/R01.txt"])
        self.assertEqual(order[2], "data/R01-idle-empty.json")
        self.assertEqual(set(order[3:]), {"data/R01-shell-memory-map.json", "data/R01-samples.csv"})

    def test_nothing_failed_prints_nothing(self):
        self.assertEqual(report.failure_evidence(self.out, [self.result("A", "pass")]), "")


if __name__ == "__main__":
    unittest.main()

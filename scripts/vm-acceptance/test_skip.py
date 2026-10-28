"""A check the run cannot have is skipped: never a pass, shown in the summary.

    python3 -m unittest scripts/vm-acceptance/test_skip.py
"""

import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import core, report  # noqa: E402
from acceptance.checks import startup  # noqa: E402
from acceptance.core import Result  # noqa: E402


class SkipTest(unittest.TestCase):
    def test_r05_is_skipped_on_a_software_rendered_run(self):
        ctx = SimpleNamespace(facts={"software_rendering": True})
        with self.assertRaisesRegex(core.Skipped, "needs a GPU render node; this run renders in software"):
            startup.gpu_adapters(ctx, SimpleNamespace(metrics={}))

    def test_run_check_reports_a_skip_as_skip(self):
        def needs_gpu(ctx, res):
            raise core.Skipped("needs a GPU render node; this run renders in software")

        ctx = SimpleNamespace(facts={}, current=None, state=lambda: {"chrome": {}},
                              vm=SimpleNamespace(root_exec=lambda *a, **k: ""))
        res = core.run_check(ctx, core.Check("X20", "resources", "GPU", needs_gpu))
        self.assertEqual(res.status, "skip")
        self.assertIn("skipped: needs a GPU render node", res.detail)

    def test_a_skip_is_counted_apart_listed_and_does_not_fail_the_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp)
            results = [Result("R05", "resources", "GPU", status="skip", detail="skipped: needs a GPU"),
                       Result("R01", "resources", "Idle", status="pass")]
            doc = report.write(out, {"commit_short": "abc"}, results, report.owners_from(None))
            self.assertEqual(doc["overall"], "pass")
            self.assertEqual(doc["summary"]["resources"]["skip"], 1)
            self.assertEqual(doc["summary"]["resources"]["pass"], 1)
            text = (out / "report.md").read_text()
            self.assertIn("**R05 GPU** (skipped): skipped: needs a GPU", text)
            self.assertIn("⏭️ skip", text)


if __name__ == "__main__":
    unittest.main()

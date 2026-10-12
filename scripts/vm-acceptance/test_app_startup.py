"""Unit tests for the app startup budget gate.

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'
"""

import contextlib
import io
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance import app_startup  # noqa: E402
import app_startup_check  # noqa: E402

LOG = """\
\x1b[2m2026-10-04T18:30:03Z\x1b[0m [app so.tilo.notes stdout] INFO starting
[app so.tilo.notes stderr] APP-STARTUP app=so.tilo.notes total_ms=8842 phases=app-construct-end:8692,first-frame-presented:31
[app so.tilo.calculator stderr] APP-STARTUP app=so.tilo.calculator total_ms=129 phases=window-vello-init-done:10
[app so.tilo.calculator stderr] APP-STARTUP app=so.tilo.calculator total_ms=700 phases=
[app so.tilo.photos stderr] tilo-gpu: window adapter "llvmpipe (LLVM 22.1.8, 256 bits)" type=Cpu backend=Vulkan driver="llvmpipe"
[app so.tilo.photos stderr] APP-STARTUP app=so.tilo.photos total_ms=2621 phases=render-worker-first-blit-copy-done:2031
[app so.tilo.notes stderr] HISTORY-OPEN listed=200 earlier=true ms=340
"""


def judge(log, *args):
    out, stdin = io.StringIO(), sys.stdin
    sys.stdin = io.StringIO(log)
    try:
        with contextlib.redirect_stdout(out):
            status = app_startup_check.main(["app_startup_check.py", *args])
    finally:
        sys.stdin = stdin
    return status, out.getvalue()


class AppStartupTest(unittest.TestCase):
    def test_a_warm_open_on_a_cpu_rasterizer_is_held_tighter_than_a_launch(self):
        warm = app_startup.warm_open_budget_ms("so.tilo.notes", "cpu")
        self.assertEqual(warm, 1000)
        self.assertLess(warm, app_startup.budget_ms("so.tilo.notes", "cpu"),
                        "a session's cold launches keep the per-launch software budget")
        self.assertEqual(app_startup.warm_open_budget_ms("so.tilo.notes", "gpu"),
                         app_startup.budget_ms("so.tilo.notes", "gpu"))

    def test_parses_launches_phases_and_history(self):
        startups = app_startup.parse_startups(LOG)
        self.assertEqual([s["app"] for s in startups],
                         ["so.tilo.notes", "so.tilo.calculator", "so.tilo.calculator", "so.tilo.photos"])
        self.assertEqual([s["renderer"] for s in startups], ["unknown"] * 3 + ["cpu"])
        self.assertEqual(startups[0]["phases"]["app-construct-end"], 8692)
        self.assertEqual(startups[2]["phases"], {})
        self.assertEqual(app_startup.parse_history_opens(LOG),
                         [{"app": "so.tilo.notes", "listed": 200, "earlier": True, "ms": 340}])

    def test_names_every_launch_over_its_budget_and_its_slowest_phase(self):
        problems = app_startup.over_budget(app_startup.parse_startups(LOG))
        self.assertEqual(len(problems), 2)
        self.assertIn("so.tilo.notes took 8842 ms", problems[0])
        self.assertIn("app-construct-end 8692 ms", problems[0])
        # A small app holds the tighter budget.
        self.assertIn("so.tilo.calculator took 700 ms", problems[1])
        # A CPU rasterizer is held to the software budget, so Photos passes.
        self.assertEqual(app_startup.over_budget(app_startup.parse_startups(LOG), scale=20), [])

    def test_repeated_markers_preserve_the_slow_interval_and_trace_order(self):
        log = (
            "[app so.tilo.browser stderr] APP-STARTUP app=so.tilo.browser "
            "total_ms=9000 phases=text-renderer-init:74,text-renderer-init-end:1569,"
            "app-component:135,text-renderer-init:6087,text-renderer-init-end:5\n"
        )
        launch, = app_startup.parse_startups(log)
        self.assertEqual(launch["phases"]["text-renderer-init-end"], 1569)
        self.assertEqual(launch["phase_intervals"], [
            {"marker": "text-renderer-init", "ms": 74},
            {"marker": "text-renderer-init-end", "ms": 1569},
            {"marker": "app-component", "ms": 135},
            {"marker": "text-renderer-init", "ms": 6087},
            {"marker": "text-renderer-init-end", "ms": 5},
        ])
        self.assertEqual(app_startup.slowest_phase(launch["phases"]),
                         "until text-renderer-init 6087 ms")
        self.assertEqual(launch["total_ms"], 9000)
        self.assertEqual(len(app_startup.over_budget([launch])), 1)

    def test_table_lists_the_slowest_app_first(self):
        rows = app_startup.table(app_startup.parse_startups(LOG))
        self.assertTrue(rows[2].startswith("| so.tilo.notes | 1 | 8842 | 8842 | unknown | 1000 |"))
        self.assertTrue(rows[3].startswith("| so.tilo.photos | 1 | 2621 | 2621 | cpu | 3000 |"))
        self.assertTrue(rows[4].startswith("| so.tilo.calculator | 2 |"))

    def test_gate_fails_over_budget_and_reports_only_when_asked(self):
        status, out = judge(LOG)
        self.assertEqual(status, 1)
        self.assertIn("::error::App startup: so.tilo.notes took 8842 ms", out)
        status, out = judge(LOG, "--report-only")
        self.assertEqual(status, 0)
        self.assertIn("::notice::", out)
        status, out = judge("[app so.tilo.files stderr] APP-STARTUP app=so.tilo.files total_ms=300 phases=\n",
                            "--require", "so.tilo.notes")
        self.assertEqual(status, 1)
        self.assertIn("so.tilo.notes logged no APP-STARTUP line", out)
        status, out = judge("[app so.tilo.files stderr] APP-STARTUP app=so.tilo.files total_ms=300 phases=\n")
        self.assertEqual(status, 0)


if __name__ == "__main__":
    unittest.main()

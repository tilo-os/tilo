"""Tests of the browser qualification adapter (tools/perf/adapters.py)."""

from __future__ import annotations

import sys
import tomllib
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import adapters  # noqa: E402

ENV = {"commit": "a" * 40, "profile": "release", "renderer": "swiftshader", "virtualization": "kvm",
       "runner": "runner", "hardware": "test cpu; 4 cpus", "started_at": "2026-11-06T01:15:00Z"}


def report(**workload: float) -> dict:
    """One qualification report that measured every workload it names."""
    samples = {
        "load_ms": [900.0], "open_ms": [120.0], "open_ready_ms": [400.0], "activate_ms": [20.0],
        "idle_per_s": [0.0], "transfer_bytes": [8_400_000.0], "request_count": [70.0],
        "start_module_bytes": [6_900_000.0],
    }
    options = {"rounds": 1, "demoSeconds": 0, "typingKeys": 0, "scrollSteps": 0, **workload}
    if options["demoSeconds"]:
        samples["demo_interval_ms"] = [8.0]
        samples["demo_work_ms"] = [2.0]
    if options["typingKeys"]:
        samples["typing_ms"] = [30.0]
        samples["typing_work_ms"] = [4.0]
        samples["typing_dom_ms"] = [6.0]
        samples["typing_over_dom"] = [24.0]
    if options["scrollSteps"]:
        samples["scroll_ms"] = [24.0]
        samples["scroll_vs_dom"] = [150.0]
    return {"schema": 1, "renderer": "swiftshader",
            "bundle": {"profile": "release", "revision": "a" * 40, "dirty": False, "digest": "d", "runtimeSha256": "r"},
            "browser": {"product": "Chrome/test"},
            "viewport": {"width": 960, "height": 540, "deviceScaleFactor": 1},
            "adapter": {"vendor": "swiftshader"},
            "workload": {**options, "apps": "notes", "inputHz": 120, "cpuThrottle": 1, "network": None},
            "samples": samples}


class BrowserQualification(unittest.TestCase):
    def test_names_a_kpi_for_every_sample_field(self):
        records = adapters.browser_qualification(report(demoSeconds=5, typingKeys=16, scrollSteps=24), ENV)
        names = {item.kpi for item in records}
        self.assertEqual(names, {kpi for kpi, _ in adapters.BROWSER_KPIS.values()})

    def test_a_slowed_run_is_not_the_nightly_series(self):
        for slowed in ({"cpuThrottle": 4}, {"network": {"megabits": 20, "latencyMs": 40, "bytesPerSecond": 2_500_000}}):
            run = report()
            run["workload"].update(slowed)
            with self.assertRaises(ValueError):
                adapters.browser_qualification(run, ENV)

    def test_records_the_typed_key_and_its_frame(self):
        records = adapters.browser_qualification(report(typingKeys=16), ENV)
        typed = {item.kpi: item for item in records if item.kpi.startswith("browser.typing.")}
        self.assertEqual(typed["browser.typing.key_to_frame"].p50, 30.0)
        self.assertEqual(typed["browser.typing.frame_work"].p50, 4.0)
        self.assertEqual(typed["browser.typing.plain_page_key_to_frame"].p50, 6.0)
        self.assertEqual(typed["browser.typing.past_plain_page"].p50, 24.0)

    def test_records_the_scroll_beside_the_plain_page(self):
        records = adapters.browser_qualification(report(typingKeys=16, scrollSteps=24), ENV)
        scrolled = {item.kpi: item for item in records if item.kpi.startswith("browser.scroll.")}
        self.assertEqual(scrolled["browser.scroll.wheel_to_frame"].p50, 24.0)
        self.assertEqual(scrolled["browser.scroll.share_of_plain_page"].p50, 150.0)
        unscrolled = report(typingKeys=16)
        unscrolled["samples"]["scroll_ms"] = [24.0]
        with self.assertRaisesRegex(ValueError, "scroll samples"):
            adapters.browser_qualification(unscrolled, ENV)

    def test_demo_and_typing_workloads_are_required_together_with_their_samples(self):
        misspelled = report()
        misspelled["samples"]["typing_ms"] = [30.0]
        with self.assertRaisesRegex(ValueError, "typing samples"):
            adapters.browser_qualification(misspelled, ENV)
        unmeasured = report(typingKeys=16)
        del unmeasured["samples"]["typing_ms"]
        with self.assertRaisesRegex(ValueError, "no samples"):
            adapters.browser_qualification(unmeasured, ENV)

    def test_every_kpi_the_nightly_records_is_budgeted(self):
        # The demo's two come from a hardware qualification, never the nightly
        # run, so they are compared by hand and have no budget.
        budgets = tomllib.loads((HERE / "kpi-budgets.toml").read_text(encoding="utf-8"))
        budgeted = {name for name in budgets["kpi"] if name.startswith("browser.")}
        recorded = {field: kpi for field, (kpi, _) in adapters.BROWSER_KPIS.items()
                    if field not in adapters.BROWSER_DEMO_FIELDS}
        self.assertEqual(budgeted, set(recorded.values()))


if __name__ == "__main__":
    unittest.main()

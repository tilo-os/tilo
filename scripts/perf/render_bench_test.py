"""The render bench records only verified, complete runs and gates only on counters."""
import argparse
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import render_bench

REPO = Path(__file__).resolve().parents[2]
ENV = {"commit": "b" * 40, "profile": "release", "renderer": "lavapipe", "virtualization": "kvm",
       "runner": "runner", "hardware": "test cpu", "started_at": "2026-10-09T01:15:00Z"}
VERIFIED = "damage-verify: PASS checked=40/40 differing_frames=0 worst_differing_pixels=0\n"
UI_OUTPUT = ('UI renderer adapter: name="llvmpipe (LLVM 19.1.7, 256 bits)" backend=Vulkan\n'
             "redrawn events:  3  (30.0%)\n")
REDRAW_OUTPUT = ('DRM bridge adapter: name="llvmpipe (LLVM 19.1.7, 256 bits)" backend=Vulkan\n'
                 "gates: mean<=25000us p99<=50000us ({gate})\n")


class FakeHarness:
    """Answers each run like the harness would, writing samples where asked."""

    def __init__(self, verify=VERIFIED, ui_samples="7000\n9000\n8000\n", redraw_gate="Pass",
                 redraw_samples=1000):
        self.verify = verify
        self.ui_samples = ui_samples
        self.redraw_gate = redraw_gate
        self.redraw_samples = redraw_samples
        self.labels = []

    def __call__(self, label, argv, overrides, out):
        self.labels.append(label)
        if label == "verify":
            return (0 if "PASS" in self.verify else 2), self.verify
        if label == "ui-timing":
            Path(overrides[render_bench.UI_SAMPLES_ENV]).write_text(self.ui_samples)
            return 0, UI_OUTPUT
        if label == "shell-redraw":
            Path(overrides[render_bench.REDRAW_SAMPLES_ENV]).write_text(
                "".join(f"{4000 + index}\n" for index in range(self.redraw_samples)))
            return (0 if self.redraw_gate == "Pass" else 1), REDRAW_OUTPUT.format(gate=self.redraw_gate)
        if label == "strip-pan":
            out = Path(argv[argv.index("--out") + 1])
            out.mkdir(parents=True, exist_ok=True)
            for _, _, stem, _ in render_bench.PAN_RECORDS:
                (out / f"{stem}.txt").write_text("3000\n5000\n4000\n")
            return 0, 'adapter: name="llvmpipe (LLVM 19.1.7, 256 bits)" backend=Vulkan driver="Mesa"\n'
        raise AssertionError(label)


class Timing(unittest.TestCase):
    def timing(self, harness, strip_pan_bench=None):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "env.json").write_text(json.dumps(ENV))
            args = argparse.Namespace(ui_perf="ui-perf", redraw_bench="redraw_bench", out=root / "out",
                                      env_json=root / "env.json", records=root / "records.jsonl",
                                      strip_pan_bench=strip_pan_bench)
            with mock.patch.object(render_bench, "run", harness):
                status = render_bench.timing(args)
            records = (render_bench.record.read(args.records) if args.records.exists() else [])
            return status, records

    def test_a_verified_run_records_both_kpis(self):
        status, records = self.timing(FakeHarness())
        self.assertEqual(status, 0)
        ui, redraw = records
        self.assertEqual((ui.kpi, ui.unit, ui.n, ui.p50, ui.max), ("render.ui.frame_us.4k", "us", 3, 8000, 9000))
        self.assertEqual(ui.meta["redraws"], "3")
        self.assertTrue(ui.meta["adapter"].startswith("llvmpipe"))
        self.assertEqual((redraw.kpi, redraw.n), ("render.shell.redraw_us.4k", 1000))
        self.assertEqual(redraw.meta["gate"], "mean<=25000us p99<=50000us Pass")
        self.assertEqual(redraw.renderer, "lavapipe")

    def test_the_pan_harness_adds_its_frame_time_and_pixel_records(self):
        status, records = self.timing(FakeHarness(), strip_pan_bench="strip_pan_bench")
        self.assertEqual(status, 0)
        pan = {item.kpi: item for item in records[2:]}
        self.assertEqual(sorted(pan), sorted(kpi for kpi, _, _, _ in render_bench.PAN_RECORDS))
        self.assertEqual((pan["render.shell.pan_pixels.4k"].unit, pan["render.shell.pan_pixels.4k"].n),
                         ("pixels", 3))
        self.assertEqual(pan["render.shell.pan_us.4k"].p50, 4000)
        self.assertTrue(pan["render.shell.pan_us.4k"].meta["adapter"].startswith("llvmpipe"))

    def test_a_failed_verification_records_nothing(self):
        harness = FakeHarness(verify="damage-verify: FAIL checked=40/40 differing_frames=2\n")
        status, records = self.timing(harness)
        self.assertEqual((status, records), (1, []))
        self.assertEqual(harness.labels, ["verify"])

    def test_samples_that_miss_redraws_fail_the_run(self):
        status, records = self.timing(FakeHarness(ui_samples="7000\n"))
        self.assertEqual((status, records), (1, []))

    def test_an_incomplete_redraw_set_is_not_recorded(self):
        status, records = self.timing(FakeHarness(redraw_samples=999))
        self.assertEqual(status, 1)
        self.assertEqual([item.kpi for item in records], ["render.ui.frame_us.4k"])

    def test_the_harness_gate_outcome_is_recorded_not_enforced(self):
        status, records = self.timing(FakeHarness(redraw_gate="Fail"))
        self.assertEqual(status, 0)
        self.assertEqual(records[1].meta["gate"], "mean<=25000us p99<=50000us Fail")


class Counters(unittest.TestCase):
    def test_every_budget_line_must_pass(self):
        passing = ("raster-pixel-budget: PASS budget=600000px/redraw observed=1px/redraw\n"
                   "allocation-call-redraw-budget: PASS budget=650calls/redraw observed=1calls/redraw\n"
                   "allocation-traffic-byte-redraw-budget: PASS budget=70000B/redraw observed_ceiling=1B/redraw\n")
        self.assertEqual(render_bench.counter_failures("redraw", 0, passing), [])
        self.assertEqual(len(render_bench.counter_failures("redraw", 2, passing)), 1)
        # A harness that stopped checking a budget is a failure, not a pass.
        self.assertEqual(len(render_bench.counter_failures("redraw", 0, passing.splitlines()[0])), 2)
        self.assertEqual(render_bench.counter_failures("idle", 0, "allocation-budget: PASS budget=1calls/1000\n"), [])

    def test_counter_runs_set_no_clock_budget(self):
        for overrides in (render_bench.REDRAW_COUNTERS, render_bench.IDLE_COUNTERS):
            self.assertFalse({name for name in overrides if name.endswith("_US")})

    def test_inherited_switches_never_reach_a_run(self):
        env = render_bench.bench_env({"TILO_UI_DASH_ITERS": "1"},
                                     {"PATH": "/bin", "TILO_UI_DASH_FRAME_BUDGET_US": "1",
                                      "VELLO_SPARSE_FINE": "0", "TILO_REDRAW_BENCH_PROFILE": "1"})
        self.assertEqual(env, {"PATH": "/bin", "TILO_UI_DASH_ITERS": "1"})


class HarnessContract(unittest.TestCase):
    """The names this script relies on are the harnesses' own."""

    def test_harnesses_read_the_sample_switches_and_print_the_parsed_lines(self):
        ui = (REPO / "crates/dev/ui-examples/examples/dashboard_perf_bench.rs").read_text()
        shell = (REPO / "crates/session/shell/examples/redraw_bench.rs").read_text()
        self.assertIn(f'"{render_bench.UI_SAMPLES_ENV}"', ui)
        self.assertIn(f'"{render_bench.REDRAW_SAMPLES_ENV}"', shell)
        for line in ("redrawn events:", "damage-verify: {}", "raster-pixel-budget: {}",
                     "allocation-call-redraw-budget: {}", "allocation-traffic-byte-redraw-budget: {}",
                     "allocation-budget: {}", "UI renderer adapter: name="):
            self.assertIn(line, ui)
        self.assertIn('"gates: mean<={MEAN_LIMIT_US}us p99<={P99_LIMIT_US}us ({gate:?})"', shell)
        self.assertIn('"DRM bridge adapter: name=', shell)
        self.assertIn(f"const SAMPLE_COUNT: usize = {render_bench.REDRAW_SAMPLE_COUNT:_}", shell)
        pan = (REPO / "crates/session/shell/examples/strip_pan_bench.rs").read_text()
        self.assertIn('"adapter: name=', pan)
        # Labels are `pan.<size>[.s<percent>][.strips<n>].w<windows>.<mode>`,
        # joined from these parts; the default cases include every scale and
        # strip count PAN_CASES reads.
        for stem in ('case.label("pan", mode)', 'case.label("strip-move", mode)',
                     'format!("s{}", (self.scale * 100.0).round())', 'format!("strips{}", self.strips)',
                     'format!("w{}", self.windows)', 'parts.join(".")',
                     '[(1.5, 1), (2.0, 1), (1.0, 2), (2.0, 2)]',
                     '("frame-us", ', '("pixels", ', '("4k", (3840, 2160))', '"layered"'):
            self.assertIn(stem, pan)


if __name__ == "__main__":
    unittest.main()

"""Tests of the same-job A/B lane (tools/perf/ab.py)."""

from __future__ import annotations

import argparse
import contextlib
import dataclasses
import io
import json
import os
import re
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import ab  # noqa: E402
import record  # noqa: E402

ENV = {"commit": "a" * 40, "profile": "release", "renderer": "none", "virtualization": "kvm",
       "runner": "runner", "hardware": "test cpu", "started_at": "2026-10-10T01:00:00Z"}
# A shared runner's speed per round: 1.9x between the fastest and slowest,
# as the relay bench's 15.9 vs 31.2 s on unchanged code.
SPEED = [1.0, 1.6, 0.9, 1.9, 1.2, 1.0, 1.7, 1.1]


def item(kpi: str, side: str, round_: int, *samples: float, unit: str = "s") -> record.Record:
    return record.from_samples(kpi, unit, samples, meta={"ab_side": side, "ab_round": str(round_)}, **ENV)


def rounds(kpi: str, base: list[float], candidate: list[float], unit: str = "s") -> list[record.Record]:
    out = []
    for number, (b, c) in enumerate(zip(base, candidate, strict=True), 1):
        out += [item(kpi, "base", number, b, unit=unit), item(kpi, "candidate", number, c, unit=unit)]
    return out


class Statistics(unittest.TestCase):
    def test_the_signed_rank_null_distribution(self):
        self.assertEqual(ab.signed_rank_null(3), [1, 1, 1, 2, 1, 1, 1])
        for n in range(1, 12):
            counts = ab.signed_rank_null(n)
            self.assertEqual(sum(counts), 2 ** n)
            self.assertEqual(len(counts), n * (n + 1) // 2 + 1)

    def test_six_rounds_give_the_range_at_969_percent(self):
        values = [-0.3, -0.1, -0.2, -0.25, -0.05, -0.15]
        interval = ab.hodges_lehmann(values)
        self.assertEqual((interval.low, interval.high), (min(values), max(values)))
        self.assertAlmostEqual(interval.confidence, 1 - 2 / 64)

    def test_eight_rounds_trim_three_walsh_averages_each_side(self):
        values = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]
        walsh = ab.walsh_averages(values)
        self.assertEqual(len(walsh), 36)
        interval = ab.hodges_lehmann(values)
        # P(T+ <= 3) = 5/256 <= 2.5 % < P(T+ <= 4) = 7/256.
        self.assertEqual((interval.low, interval.high), (walsh[3], walsh[32]))
        self.assertAlmostEqual(interval.confidence, 1 - 10 / 256)
        self.assertEqual(interval.estimate, 4.5)

    def test_five_rounds_cannot_reach_95_percent(self):
        self.assertEqual(ab.min_rounds(), 6)
        interval = ab.hodges_lehmann([-1.0] * 5)
        self.assertIsNone(interval.low)
        self.assertIsNone(interval.confidence)

    def test_the_exact_sign_test(self):
        self.assertEqual(ab.sign_test([-1.0] * 6), (6, 0, 0, 2 / 64))
        negative, positive, zero, p = ab.sign_test([-1.0, -1.0, 1.0, 0.0])
        self.assertEqual((negative, positive, zero), (2, 1, 1))
        self.assertAlmostEqual(p, 1.0)


class Verdict(unittest.TestCase):
    def test_a_gain_smaller_than_the_runner_spread_is_proven_when_paired(self):
        """15 % less CPU while the runner's speed swings 1.9x between rounds."""
        base = [10.0 * speed for speed in SPEED]
        candidate = [8.5 * speed * (1.02 if n % 2 else 0.98) for n, speed in enumerate(SPEED)]
        paired = ab.judge("hosting.relay.cpu_s", "s", "p50", list(range(1, 9)), base, candidate)
        self.assertEqual(paired.verdict, "improved")
        self.assertEqual(paired.scale, "ratio")
        self.assertAlmostEqual(paired.estimate, 0.85, delta=0.02)
        self.assertLess(paired.high, 1.0)
        self.assertEqual((paired.better_rounds, paired.worse_rounds), (8, 0))
        # Unpaired, the same numbers overlap: the slowest candidate run is
        # slower than the fastest base run.
        self.assertGreater(max(candidate), min(base))

    def test_a_cost_that_grows_regressed(self):
        base = [10.0 * speed for speed in SPEED]
        paired = ab.judge("x.y", "ms", "p50", list(range(1, 9)), base, [b * 1.3 for b in base])
        self.assertEqual(paired.verdict, "regressed")
        self.assertGreater(paired.low, 1.0)

    def test_noise_without_a_change_is_inconclusive(self):
        base = [10.0 * speed for speed in SPEED]
        candidate = [b * factor for b, factor in zip(base, [1.1, 0.9, 1.05, 0.95, 1.2, 0.85, 1.0, 1.02])]
        paired = ab.judge("x.y", "s", "p50", list(range(1, 9)), base, candidate)
        self.assertEqual(paired.verdict, "inconclusive")
        self.assertLess(paired.low, 1.0)
        self.assertGreater(paired.high, 1.0)

    def test_identical_rounds_are_unchanged(self):
        paired = ab.judge("x.bytes", "bytes", "p50", list(range(1, 7)), [5.0] * 6, [5.0] * 6)
        self.assertEqual(paired.verdict, "unchanged")

    def test_frames_per_second_are_better_higher(self):
        base = [60.0 * speed for speed in SPEED]
        paired = ab.judge("x.fps", "fps", "p50", list(range(1, 9)), base, [b * 1.2 for b in base])
        self.assertEqual(paired.verdict, "improved")
        self.assertEqual(paired.better_rounds, 8)

    def test_too_few_rounds_are_inconclusive_however_clear(self):
        paired = ab.judge("x.y", "s", "p50", [1, 2, 3, 4, 5], [10.0] * 5, [5.0] * 5)
        self.assertEqual(paired.verdict, "inconclusive")
        self.assertIn("6 are needed", paired.note)

    def test_a_zero_falls_back_to_differences(self):
        paired = ab.judge("x.count", "count", "p50", list(range(1, 7)), [2.0] * 6, [0.0] * 6)
        self.assertEqual(paired.scale, "difference")
        self.assertEqual(paired.estimate, -2.0)
        self.assertEqual(paired.verdict, "improved")


class Pairing(unittest.TestCase):
    def test_kpis_pair_by_round_and_a_one_sided_kpi_is_listed(self):
        records = rounds("hosting.cpu", [10.0] * 6, [8.0] * 6)
        records += [item("hosting.fsyncs", "candidate", n, 2.1, unit="count") for n in range(1, 7)]
        analysis = ab.analyse(records)
        self.assertEqual([(p.kpi, p.stat) for p in analysis.paired], [("hosting.cpu", "p50"), ("hosting.cpu", "p99")])
        self.assertEqual(analysis.only, {"base": [], "candidate": ["hosting.fsyncs"]})
        self.assertEqual(analysis.rounds, 6)

    def test_a_round_one_side_missed_is_not_paired(self):
        records = rounds("x.cpu", [10.0] * 7, [8.0] * 7)
        records = [r for r in records if not (r.meta["ab_round"] == "4" and r.meta["ab_side"] == "base")]
        [p50, _] = ab.analyse(records).paired
        self.assertEqual(p50.rounds, [1, 2, 3, 5, 6, 7])

    def test_the_p99_is_judged_beside_the_p50(self):
        records = []
        for n in range(1, 7):
            records += [item("x.ms", "base", n, 1, 1, 1, 9, unit="ms"), item("x.ms", "candidate", n, 1, 1, 1, 4, unit="ms")]
        verdicts = {p.stat: p.verdict for p in ab.analyse(records).paired}
        self.assertEqual(verdicts, {"p50": "unchanged", "p99": "improved"})

    def test_a_record_without_a_side_is_refused(self):
        stray = record.from_samples("x.y", "s", [1.0], **ENV)
        with self.assertRaisesRegex(ValueError, "not an A/B record"):
            ab.analyse([stray])

    def test_balanced_order(self):
        self.assertEqual([ab.order(n) for n in (1, 2, 3, 4)],
                         [("base", "candidate"), ("candidate", "base")] * 2)

    def test_the_summary_names_the_verdict_and_the_interval(self):
        records = rounds("hosting.cpu", [10.0 * s for s in SPEED], [8.0 * s for s in SPEED])
        sides = {"base": {"ref": "main", "commit": "b" * 40}, "candidate": {"ref": "HEAD", "commit": "c" * 40}}
        text = ab.summary(ab.analyse(records), sides, ["hosting"], "d" * 40, ["hosting round 3 base: boom"])
        self.assertIn("**improved** (p50): `hosting.cpu`", text)
        self.assertIn("| -20.0 % | [-20.0 %, -20.0 %] | 8 / 0 of 8 |", text)
        self.assertIn("hosting round 3 base: boom", text)


# A fake group for the end-to-end run: "building" copies the side's `cost`
# file to target/bench, and a run reports it scaled by the round's runner speed.
def fake_run(ctx: ab.RunContext) -> list[record.Record]:
    cost = float(ctx.bins["bench"].read_text())
    staged = float((ctx.repo / "target/staged").read_text())
    assert staged == cost, "the side's artifact is staged before its run"
    speed = SPEED[(int(ctx.env["meta"]["ab_round"]) - 1) % len(SPEED)]
    env = {name: value for name, value in ctx.env.items() if name != "meta"}
    return [record.from_samples("fake.cost", "s", [cost * speed], meta=dict(ctx.env["meta"]), **env)]


FAKE = ab.Group("fake", "release", "none", (("sh", "-c", "mkdir -p target && cp cost target/bench"),),
                {"bench": "target/bench"}, {}, {"bench": "target/staged"}, fake_run)


class EndToEnd(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name) / "repo"
        self.repo.mkdir()
        self.env = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.com",
                    "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.com"}
        self.git("init", "--quiet", "--initial-branch=work")
        (self.repo / ".gitignore").write_text("target/\nout/\n")
        (self.repo / "cost").write_text("10\n")
        self.git("add", ".")
        self.git("commit", "--quiet", "-m", "base")
        self.base = self.git("rev-parse", "HEAD")
        (self.repo / "cost").write_text("8\n")
        self.git("commit", "--quiet", "-am", "candidate")
        self.candidate = self.git("rev-parse", "HEAD")
        (self.repo / "harness").write_text("the harness\n")
        self.git("add", "harness")
        self.git("commit", "--quiet", "-m", "harness")

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *args: str) -> str:
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, capture_output=True, text=True,
                              check=True).stdout.strip()

    def options(self, rounds: int = 6) -> argparse.Namespace:
        return argparse.Namespace(out=self.repo / "out", rounds=rounds, base=self.base, candidate=self.candidate,
                                  summary=None, virtualization="none", runner="local")

    def run_ab(self, rounds: int = 6) -> int:
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return ab.run(self.options(rounds), self.repo, [FAKE])

    def test_both_sides_are_built_and_run_interleaved_and_judged(self):
        self.assertEqual(self.run_ab(), 0)
        # The checkout is back on the harness branch.
        self.assertEqual(self.git("symbolic-ref", "--short", "HEAD"), "work")
        self.assertTrue((self.repo / "harness").exists())
        records = record.read(self.repo / "out/records.jsonl")
        self.assertEqual([(r.meta["ab_round"], r.meta["ab_side"]) for r in records][:4],
                         [("1", "base"), ("1", "candidate"), ("2", "candidate"), ("2", "base")])
        self.assertEqual({r.commit for r in records if r.meta["ab_side"] == "base"}, {self.base})
        result = json.loads((self.repo / "out/ab-result.json").read_text())
        [p50] = [k for k in result["kpis"] if k["stat"] == "p50"]
        self.assertEqual(p50["verdict"], "improved")
        self.assertAlmostEqual(p50["estimate"], 0.8)
        self.assertEqual(result["candidate"]["commit"], self.candidate)
        self.assertIn("improved", (self.repo / "out/summary.md").read_text())

    def test_an_optional_benchmark_runs_only_on_the_side_that_has_it(self):
        # `harness` exists only after the candidate: on a base that predates it
        # the optional artifact is skipped, and its KPI is the candidate's alone.
        def run_with_extra(ctx: ab.RunContext) -> list[record.Record]:
            records = fake_run(ctx)
            if "extra" in ctx.bins:
                env = {name: value for name, value in ctx.env.items() if name != "meta"}
                records.append(record.from_samples("fake.extra", "s", [1], meta=dict(ctx.env["meta"]), **env))
            return records

        group = dataclasses.replace(FAKE, run=run_with_extra, optional={
            "extra": ("harness", ("sh", "-c", "mkdir -p target && cp harness target/extra"), "target/extra")})
        options = self.options()
        options.candidate = self.git("rev-parse", "HEAD")
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(ab.run(options, self.repo, [group]), 0)
        self.assertFalse((self.repo / "out/bin/base/fake/extra").exists())
        self.assertTrue((self.repo / "out/bin/candidate/fake/extra").exists())
        result = json.loads((self.repo / "out/ab-result.json").read_text())
        self.assertEqual((result["only_base"], result["only_candidate"]), ([], ["fake.extra"]))

    def test_a_dirty_checkout_is_refused(self):
        (self.repo / "cost").write_text("1\n")
        with self.assertRaisesRegex(RuntimeError, "tracked changes"):
            self.run_ab()

    def test_too_few_rounds_are_refused_before_building(self):
        with self.assertRaisesRegex(RuntimeError, "at least 6"):
            self.run_ab(rounds=4)
        self.assertFalse((self.repo / "out").exists())

    def test_a_failed_build_returns_the_checkout(self):
        broken = ab.Group("broken", "release", "none", (("sh", "-c", "exit 3"),), {}, {}, {}, fake_run)
        with self.assertRaisesRegex(RuntimeError, "exited 3"), contextlib.redirect_stdout(io.StringIO()):
            ab.run(self.options(), self.repo, [broken])
        self.assertEqual(self.git("symbolic-ref", "--short", "HEAD"), "work")


class Groups(unittest.TestCase):
    def test_hosting_runs_the_keystroke_gate_when_present_and_propagates_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            repo = Path(temporary)
            (repo / "hosting-load.json").write_text("{}")
            ctx = ab.RunContext(repo, {"relay_load": repo / "relay_load"}, {}, repo,
                                ENV, repo / "env.json",
                                argparse.Namespace(hosting_relay_cpus=0, hosting_minutes=3))
            with mock.patch.object(ab, "call") as call, mock.patch.object(
                    ab.adapters, "hosting_load", return_value=[]) as adapt:
                self.assertEqual(ab.run_hosting(ctx), [])
                self.assertEqual(call.call_count, 1)
                self.assertEqual(call.call_args.args[0][0], ctx.bins["relay_load"])
                ctx.bins["keystroke_writes"] = repo / "keystroke_writes"
                call.reset_mock()
                self.assertEqual(ab.run_hosting(ctx), [])
                self.assertEqual(call.call_count, 2)
                self.assertEqual(call.call_args_list[0].args,
                                 ([ctx.bins["keystroke_writes"]], repo / "keystroke-writes.log", repo))
                self.assertEqual(call.call_args_list[1].args[0][0], ctx.bins["relay_load"])
                call.reset_mock()
                adapt.reset_mock()
                call.side_effect = RuntimeError("keystroke write gate failed")
                with self.assertRaisesRegex(RuntimeError, "keystroke write gate failed"):
                    ab.run_hosting(ctx)
                self.assertEqual(call.call_count, 1)
                adapt.assert_not_called()

    def test_idle_timing_is_an_explicit_executable_not_a_pr_test(self):
        root = HERE.parents[1]
        manifest = tomllib.loads((root / "crates/session/shell/Cargo.toml").read_text())
        target = next(target for target in manifest["example"] if target["name"] == "idle-wakeups")
        self.assertFalse(target["test"])
        self.assertFalse(target["bench"])
        self.assertEqual(target["required-features"], ["scheduled-idle-benchmark"])
        self.assertNotIn("scheduled-idle-benchmark", manifest["features"]["default"])
        fixture = (root / "crates/session/shell/tests/idle_wakeups.rs").read_text()
        self.assertNotIn("fn an_idle_desktop_with_two_apps_stays_within_its_wakeup_budget", fixture)
        benchmark = (root / "crates/session/shell/examples/idle-wakeups.rs").read_text()
        self.assertIn("fn an_idle_desktop_with_two_apps_stays_within_its_wakeup_budget", benchmark)
        self.assertNotIn("#[test]", benchmark)
        group = ab.GROUPS["wakeups"]
        self.assertFalse(group.tests)
        self.assertEqual(set(group.binaries), {"shell", "idle-wakeups"})
        self.assertTrue(any("--bin" in command and "shell" == command[-1] for command in group.builds))

    def test_idle_ab_uses_the_old_test_only_for_historical_refs(self):
        with tempfile.TemporaryDirectory() as temporary:
            repo = Path(temporary)
            selected = ab.GROUPS["wakeups"]
            legacy = ab.side_group(selected, repo)
            self.assertEqual(set(legacy.tests), {"it"})
            self.assertEqual(set(legacy.binaries), {"shell"})
            source = repo / "crates/session/shell/examples/idle-wakeups.rs"
            source.parent.mkdir(parents=True)
            source.write_text("fn main() {}")
            self.assertIs(ab.side_group(selected, repo), selected)

    def test_fs_ab_runs_the_old_gate_only_for_historical_refs(self):
        with tempfile.TemporaryDirectory() as temporary:
            repo = Path(temporary)
            selected = ab.GROUPS["fs"]
            legacy = ab.side_group(selected, repo)
            (command, target), = legacy.tests.values()
            self.assertEqual(set(legacy.tests), {"performance_timing"})
            self.assertEqual((command[-2], target), ("performance_gate", "performance_gate"))
            source = repo / "crates/system/fs/tests/performance_timing.rs"
            source.parent.mkdir(parents=True)
            source.write_text("")
            self.assertIs(ab.side_group(selected, repo), selected)

    def test_every_build_lane_but_footprint_has_an_ab_group(self):
        workflow = (HERE.parents[1] / ".forgejo/workflows/perf-scheduled.yml").read_text()
        for name in ab.GROUPS:
            self.assertIn(f"\n  {name}:\n", workflow, f"{name} is a perf-scheduled group")
        self.assertNotIn("footprint", ab.GROUPS)

    def test_the_workflow_accepts_every_ab_group(self):
        # The plan job refuses a group it does not list before ab.py runs.
        workflow = (HERE.parents[1] / ".forgejo/workflows/perf-scheduled.yml").read_text()
        listed = re.search(r"ab_known=\(([^)]*)\)", workflow)
        self.assertIsNotNone(listed, "perf-scheduled.yml lists its A/B groups")
        self.assertEqual(set(listed.group(1).split()), set(ab.GROUPS))

    def test_every_group_builds_the_benchmarks_its_nightly_job_builds(self):
        # An A/B group that skips one of its nightly job's benchmarks cannot
        # pair that benchmark's KPIs (the render group once left out strip pans).
        workflow = (HERE.parents[1] / ".forgejo/workflows/perf-scheduled.yml").read_text()
        for name, group in ab.GROUPS.items():
            job = re.split(r"\n  [a-z][a-z0-9-]*:\n", workflow.split(f"\n  {name}:\n", 1)[1], maxsplit=1)[0]
            nightly = set(re.findall(r"--example (\S+)", job))
            commands = [*group.builds, *(command for _, command, _ in group.optional.values())]
            built = {command[command.index("--example") + 1] for command in commands if "--example" in command}
            self.assertEqual(nightly - built, set(), f"the A/B {name} group skips nightly benchmarks")

    def test_staged_artifacts_are_built(self):
        for group in ab.GROUPS.values():
            for name in group.stage:
                self.assertIn(name, {*group.binaries, *group.tests})
            self.assertTrue(group.builds or group.tests, group.name)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""tools/perf/room_capacity.py: the sweep's plan and what a sweep derives."""

from __future__ import annotations

import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

import room_capacity  # noqa: E402


def quiet_cgroup(directory: Path) -> Path:
    """A job cgroup with room, so a test measures its run and not the job.

    The suite runs beside every other suite of a CI check in one cgroup; a
    bench started there inherits the job's memory use, and the sweep's guard
    would stop it at once instead of at its own timeout.
    """
    root = directory / "cgroup"
    root.mkdir()
    (root / "memory.current").write_text("0", encoding="utf-8")
    (root / "memory.max").write_text(str(8 * 1024**3), encoding="utf-8")
    return root


def report(guests: int, rss: float, cores: float, spectators: int = 0, egress: float = 0.0,
           failures: dict[str, int] | None = None) -> dict:
    """A room-load bench report with the fields the summary reads."""
    out = {
        "config": {"guests": guests, "spectators": spectators},
        "joined": guests,
        "full": {"count": 0},
        "join_ms": {"p50": 20.0, "p95": 40.0, "p99": 50.0, "max": 60.0},
        "chat_ms": {"p50": 5.0, "p95": 9.0, "p99": 12.0, "max": 30.0},
        "failures": failures or {},
        "host": {"hold_cpu_cores": cores, "rss_mib_peak": rss, "egress_bytes_per_s": egress,
                 "driver_cpu_cores_avg": 2.0, "machine_cores": 12},
    }
    if spectators:
        out["spectators"] = {"joined": spectators, "full": {"count": 0},
                             "join_ms": {"p99": 70.0}, "chat_ms": {"p99": 15.0}, "failures": {}}
    return out


def run(name: str, kind: str, body: dict | None) -> dict:
    return {"name": name, "kind": kind, "args": [], "exit": 0 if body else 1, "report": body}


class PlanTest(unittest.TestCase):
    def test_the_full_plan_covers_the_write_up(self):
        names = [item.name for item in room_capacity.plan("full")]
        for expected in ("idle", "plain-g50", "plain-g800", "rooms4-g300", "cap500-g800", "spect-s4000", "caps"):
            self.assertIn(expected, names)
        caps = next(item for item in room_capacity.plan("full") if item.name == "caps")
        self.assertEqual(caps.args, ("--guests", "500", "--spectators", "2000",
                                     "--spectator-members", "--max-guests", "500", "--max-spectators", "2000"))

    def test_a_regex_selects_runs_and_an_empty_selection_fails(self):
        self.assertEqual([item.name for item in room_capacity.plan("^spect-")],
                         ["spect-s500", "spect-s1000", "spect-s2000", "spect-s4000"])
        with self.assertRaises(ValueError):
            room_capacity.plan("nothing-matches")


class BenchTest(unittest.TestCase):
    def test_a_run_past_its_time_is_stopped_and_named(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(room_capacity, "CGROUP", quiet_cgroup(root)):
                code, _, stopped = room_capacity.bench(
                    [sys.executable, "-c", "import time; time.sleep(30)"],
                    root / "log", timeout_s=1)
        self.assertIsNone(code)
        self.assertEqual(stopped, "stopped after 1 s")

    def test_a_run_has_its_ramps_hold_and_join_timeout(self):
        caps = next(item for item in room_capacity.plan("full") if item.name == "caps")
        # 2000 spectators at 50/s outlast 500 guests at 20/s.
        self.assertEqual(room_capacity.run_seconds(caps), 40 + 60 + 60 + 300)

    def test_the_degraded_plan_asks_the_declared_capacity_and_the_cap(self):
        self.assertEqual([(item.name, item.kind) for item in room_capacity.degraded_plan()],
                         [("probe-g50", "plain"), ("probe-g100", "degraded"), ("probe-g500", "capped")])
        capped = room_capacity.degraded_plan()[-1]
        # More guests than the room may carry, so admission answers instead of
        # the run waiting on a room that grew past its capacity.
        self.assertEqual(dict(zip(capped.args[::2], capped.args[1::2]))["--max-guests"],
                         str(room_capacity.DECLARED_GUESTS))


class DegradedRunTest(unittest.TestCase):
    def test_every_run_hands_the_prefix_the_bench_and_its_own_arguments(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "tilo-room-load"
            # A stand-in bench: it prints a report for the guests it was asked for and records
            # the command line it was given.
            binary.write_text(
                "#!/bin/sh\n"
                'guests=0; while [ $# -gt 0 ]; do [ "$1" = --guests ] && guests=$2; shift; done\n'
                'printf \'{"name": "probe-g%s", "guests": %s, "joined": %s, "failures": 0, "host": {},'
                ' "join_ms": {"p99": 100}, "chat_ms": {"p99": 100}, "spectators": {"joined": 0}}\\n\' "$guests" "$guests" "$guests"\n',
                encoding="utf-8")
            binary.chmod(0o755)
            # The prefix: what stands between the probe and the bench, and what the probe must
            # hand the bench's whole command line to (tools/ci/perf-room-degraded.sh in a run).
            prefix = root / "degrade"
            prefix.write_text(f'#!/bin/sh\nprintf "%s\\n" "$*" >> {root / "calls.log"}\nexec "$@"\n',
                              encoding="utf-8")
            prefix.chmod(0o755)
            out = root / "probe"
            with patch.object(room_capacity, "CGROUP", quiet_cgroup(root)):
                self.assertEqual(room_capacity.degraded([str(prefix)], binary, out), 0)
            self.assertEqual(sorted(path.stem for path in out.glob("*.json")),
                             sorted(item.name for item in room_capacity.degraded_plan()))
            calls = (root / "calls.log").read_text(encoding="utf-8").splitlines()
            self.assertEqual(len(calls), len(room_capacity.degraded_plan()), "one process a run")
            for call, item in zip(calls, room_capacity.degraded_plan()):
                self.assertTrue(call.startswith(str(binary)), call)
                self.assertIn("bench", call)
                self.assertIn(" ".join(item.args), call)


def degraded_report(guests: int, join_p99: float, chat_p99: float, cores: float,
                    egress_bytes_s: float, failures: dict[str, int] | None = None, spectators: int = 0) -> dict:
    """A bench report of a run under the degraded host."""
    body = report(guests, 200.0, cores, spectators=spectators, egress=egress_bytes_s, failures=failures)
    body["join_ms"]["p99"] = join_p99
    body["chat_ms"]["p99"] = chat_p99
    body["name"] = f"probe-g{guests}"
    return body


class DegradedProbeTest(unittest.TestCase):
    def probe(self, at_capacity: dict | None = None, capped: dict | None = None) -> dict:
        runs = [run("probe-g50", "plain", degraded_report(50, 600.0, 1_500.0, 0.5, 200_000.0)),
                run("probe-g100", "degraded", at_capacity or degraded_report(100, 9_000.0, 5_500.0, 1.6, 300_000.0)),
                run("probe-g500", "capped", capped or degraded_report(500, 9_500.0, 5_900.0, 2.6, 350_000.0, spectators=1000))]
        return room_capacity.judge_probe(runs)

    def test_a_capacity_run_inside_the_service_level_passes(self):
        budget = self.probe()
        self.assertTrue(budget["met"], budget["notes"])
        self.assertEqual(budget["capped_watching"], 1000)

    def test_a_slow_join_at_the_declared_capacity_fails_with_its_own_reason(self):
        budget = self.probe(at_capacity=degraded_report(100, 12_000.0, 5_000.0, 1.6, 300_000.0))
        self.assertFalse(budget["met"])
        self.assertIn("joins at the declared", budget["notes"][0])

    def test_a_slow_conversation_at_the_declared_capacity_fails(self):
        budget = self.probe(at_capacity=degraded_report(100, 9_000.0, 8_000.0, 1.6, 300_000.0))
        self.assertFalse(budget["met"])
        self.assertIn("chat at the declared", budget["notes"][0])

    def test_a_degradation_the_host_never_felt_fails_rather_than_passes(self):
        # An idle host offers 70 % of nothing: the run measured the machine as
        # it is, so it says nothing about a host at capacity.
        budget = self.probe(at_capacity=degraded_report(100, 100.0, 100.0, 0.2, 1_000.0))
        self.assertFalse(budget["met"])
        self.assertIn("under the", budget["notes"][0])

    def test_a_room_past_its_cap_must_keep_answering(self):
        budget = self.probe(capped=degraded_report(500, 60_000.0, 1_000.0, 2.6, 350_000.0))
        self.assertFalse(budget["met"])
        self.assertIn("past its cap answered joins", budget["notes"][0])

    def test_a_missing_run_is_named_not_assumed(self):
        runs = [run("probe-g50", "plain", degraded_report(50, 600.0, 1_500.0, 0.5, 200_000.0))]
        budget = room_capacity.judge_probe(runs)
        self.assertFalse(budget["met"])
        self.assertEqual(budget["missing"], ["probe-g100", "probe-g500"])

    def test_a_failed_session_fails_the_probe(self):
        budget = self.probe(at_capacity=degraded_report(100, 9_000.0, 5_000.0, 1.6, 300_000.0,
                                                        failures={"timeout": 3}))
        self.assertFalse(budget["met"])
        self.assertIn("3 sessions failed", budget["notes"][0])


class SummaryTest(unittest.TestCase):
    prices = tomllib.loads(room_capacity.PRICES.read_text(encoding="utf-8"))

    def sweep(self) -> list[dict]:
        # 100 MiB idle, 2 MiB and 5 millicores per guest, 0.1 MiB per spectator.
        runs = [run("idle", "idle", report(0, 100, 0.0))]
        runs += [run(f"plain-g{n}", "plain", report(n, 100 + 2 * n, 0.005 * n, egress=1000.0 * n))
                 for n in (100, 300, 500)]
        runs += [run(f"spect-s{n}", "spectators", report(50, 200 + 0.1 * n, 0.25, spectators=n,
                                                         egress=50_000 + 100.0 * n))
                 for n in (1000, 2000)]
        runs.append(run("caps", "caps", report(500, 1350, 2.7, spectators=2000)))
        runs.append(run("plain-g800", "plain", None))
        return runs

    def test_the_slopes_are_per_guest_and_per_spectator(self):
        derived = room_capacity.summarize(self.sweep(), self.prices)["derived"]
        self.assertAlmostEqual(derived["mib_per_guest"], 2.0)
        self.assertAlmostEqual(derived["guests_per_vcpu"], 200.0)
        self.assertAlmostEqual(derived["mib_per_spectator"], 0.1)
        self.assertAlmostEqual(derived["egress_kbit_s_per_guest"], 8.0)
        self.assertAlmostEqual(derived["predicted_rss_mib_at_caps"], 100 + 1000 + 200)
        self.assertEqual(derived["at_caps"]["rss_mib_peak"], 1350)

    def test_a_failed_run_is_named_not_averaged(self):
        summary = room_capacity.summarize(self.sweep(), self.prices)
        self.assertEqual(summary["failed_runs"], ["plain-g800"])
        self.assertIn("Runs that did not finish: plain-g800", room_capacity.markdown(summary))

    def test_nodes_are_priced_by_their_binding_resource(self):
        cost = room_capacity.summarize(self.sweep(), self.prices)["cost"]
        ax102 = cost["hetzner-ax102"]
        # 32 vCPU at 70 % / 5 millicores = 4480 guests; memory allows far more.
        self.assertEqual(ax102["binding"], "cpu")
        self.assertEqual(ax102["guests_per_node"], 4480)
        self.assertAlmostEqual(ax102["usd_per_month_per_100_guests"], 302.33 / 4480 * 100)
        aws = cost["aws-c7a-2xlarge"]
        # 8 vCPU: 1120 guests, each sending 1000 B/s that AWS bills.
        egress = 1000.0 * 3600 * 730 / 1e9 * 1120 * 0.09
        self.assertAlmostEqual(aws["usd_per_month_per_100_guests"], (299.71 + egress) / 1120 * 100)

    def test_a_node_whose_port_fills_first_is_bound_by_its_network(self):
        node = self.prices["room_node"]["hetzner-ax102"]
        # 1 Mbit/s each: the 1 Gbit/s port at 70 % carries 700 guests, the
        # CPU 4480.
        cost = room_capacity.node_cost(node, self.prices, {"cores": 0.005, "mib": 2.0, "egress_bytes_s": 125_000.0},
                                       idle_mib=100)
        self.assertEqual((cost["binding"], cost["guests_per_node"]), ("network", 700))

    def test_a_partial_sweep_is_reported_without_derivations(self):
        summary = room_capacity.summarize([run("plain-g300", "plain", report(300, 700, 1.5))], self.prices)
        self.assertEqual(summary["derived"], {})
        self.assertIn("| plain-g300 | 300/300 |", room_capacity.markdown(summary))

    def test_a_runs_phase_timings_and_commits_are_reported(self):
        body = report(300, 700, 1.5)
        body["committed"], body["chat_tx"] = 18, 316
        body["timings"] = {"join.ticket_wait": "n=300 total_ms=900 p99_ms=12.0 last_size=0 ms_by_tenth=1,2",
                           "owner.replay.persist": "n=900 total_ms=4000 max_ms=80.0 busy_ms_by_s=0,950"}
        text = room_capacity.markdown(room_capacity.summarize([run("plain-g300", "plain", body)], self.prices))
        self.assertIn("| 18/316 |", text)
        self.assertIn("plain-g300 phase timings:", text)
        self.assertIn("- `join.ticket_wait`: n=300 total_ms=900", text)
        self.assertIn("- `owner.replay.persist`: n=900 total_ms=4000 max_ms=80.0 busy_ms_by_s=0,950", text)


if __name__ == "__main__":
    unittest.main()

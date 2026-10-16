#!/usr/bin/env python3
"""tools/perf/room_capacity.py: the sweep's plan and what a sweep derives."""

from __future__ import annotations

import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import room_capacity  # noqa: E402


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
            code, _, stopped = room_capacity.bench([sys.executable, "-c", "import time; time.sleep(30)"],
                                                   Path(directory) / "log", timeout_s=1)
        self.assertIsNone(code)
        self.assertEqual(stopped, "stopped after 1 s")

    def test_a_run_has_its_ramps_hold_and_join_timeout(self):
        caps = next(item for item in room_capacity.plan("full") if item.name == "caps")
        # 2000 spectators at 50/s outlast 500 guests at 20/s.
        self.assertEqual(room_capacity.run_seconds(caps), 40 + 60 + 60 + 300)


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

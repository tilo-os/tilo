"""The idle checks (R02, R09) tell the session's processes apart by what they run: the
login wrapper, the compositor and the compositor's render services all carry the name
tilo-session."""

from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/vm-acceptance"))

from acceptance.checks import resources  # noqa: E402

WRAPPER, COMPOSITOR, PRIVATE, SHARED, NOTES = 2987, 3053, 3200, 3300, 4000


def snapshot(ticks: dict[int, int], switches: dict[int, int]) -> str:
    """One `SAMPLE` snapshot: the wrapper (bash) starts the compositor, which starts a
    render service for the private workspaces and one for a shared workspace."""
    procs = [
        (WRAPPER, "tilo-session", 1, "/usr/bin/bash"),
        (COMPOSITOR, "tilo-session", WRAPPER, resources.COMPOSITOR_BIN),
        (PRIVATE, "tilo-session", COMPOSITOR, resources.COMPOSITOR_BIN),
        (SHARED, "tilo-session", COMPOSITOR, resources.COMPOSITOR_BIN),
        (NOTES, "notes", COMPOSITOR, "/usr/libexec/tilo/notes"),
    ]
    lines = ["U 600.0"]
    for pid, comm, ppid, exe in procs:
        lines.append(f"P {pid} {comm} {ticks[pid]} {switches[pid]} {ppid} 100 {exe}")
        lines.append(f"H {pid} {pid} {comm} {ticks[pid]} {switches[pid]}")
    lines.append("T 100000 90000")
    return "\n".join(lines)


def sample(before: dict[int, int], after: dict[int, int]) -> dict:
    still = {pid: 0 for pid in before}
    text = "\n".join(["@@0", snapshot(before, still), "@@1", snapshot(after, after), "HZ 100"])
    return resources._parse_sample(text)


class SessionProcesses(unittest.TestCase):
    def setUp(self):
        # Over 10 s: the compositor 50 % (500 ticks), the shared workspace's render
        # service 30 %, the private one 10 %, the wrapper nothing.
        before = {WRAPPER: 0, COMPOSITOR: 0, PRIVATE: 0, SHARED: 0, NOTES: 0}
        after = {WRAPPER: 0, COMPOSITOR: 500, PRIVATE: 100, SHARED: 300, NOTES: 20}
        self.summary = resources.summarize(sample(before, after), 10)

    def test_the_compositor_is_the_binary_the_wrapper_starts(self):
        self.assertEqual(self.summary["compositor_pid"], COMPOSITOR)
        self.assertEqual(self.summary["shell_cpu_pct"], 50.0)

    def test_every_render_service_is_measured(self):
        render = self.summary["render_services"]
        self.assertEqual(render["processes"], 2)
        self.assertEqual(render["cpu_pct"], 40.0)
        self.assertEqual(set(render["threads"]), {str(PRIVATE), str(SHARED)})

    def test_a_failure_names_what_each_tilo_session_is(self):
        named = [resources.describe_process(p) for p in self.summary["top"]]
        self.assertEqual(named[:4], ["tilo-session (compositor) 50.0%",
                                     "tilo-session (render service) 30.0%",
                                     "tilo-session (render service) 10.0%",
                                     "notes 2.0%"])
        self.assertIn("tilo-session (login wrapper) 0.0%", named)

    def test_the_shared_workspace_check_reports_the_render_services(self):
        m = {**self.summary, "named_threads": [
            {"pid": COMPOSITOR, "process": "tilo-session", "thread": resources.BROKER_THREAD,
             "threads": 1, "cpu_pct": 0.1, "ctxt_switches_per_s": 0.2},
            {"pid": NOTES, "process": "notes", "thread": resources.SHOVEL_THREAD,
             "threads": 1, "cpu_pct": 0.0, "ctxt_switches_per_s": 0.1}]}
        budgets = {"idle_shared_broker_wakeups_per_s": 1.0, "idle_shared_shovel_wakeups_per_s": 1.0}
        judged, problems = resources.shared_idle(m, budgets)
        self.assertEqual(problems, [])
        self.assertEqual(judged["render_service_processes"], 2)
        self.assertEqual(judged["render_service_cpu_pct"], 40.0)


if __name__ == "__main__":
    unittest.main()

"""How fast every app the session launched drew its first frame, judged against a budget.

Each Tilo app logs one line when its first frame is presented, which the session log
keeps as `[app <pkg> stderr] APP-STARTUP app=<pkg> total_ms=<ms> phases=<mark>:<ms>,...`.
`total_ms` runs from the app's process start to that first frame. Opening History logs
`[app <pkg> stderr] HISTORY-OPEN listed=<n> earlier=<bool> ms=<ms>`: the time from the
request to the listed history. The `tilo-gpu: window adapter` line an app logs before its
first frame says what it rendered on: a launch on a CPU rasterizer (llvmpipe in a VM
without a GPU) compiles and runs vello's shaders on the CPU and is held to the software
budget the VM checks use, a launch on a GPU to the hardware one.

Plain functions over text, so the VM checks and the installed-image gate in CI
(app_startup_check.py) judge alike.
"""

from __future__ import annotations

import re
import statistics

LOGS = ("$HOME/.local/state/tilo/session-logs/previous.log",
        "$HOME/.local/state/tilo/session-logs/latest.log")

# Run in the guest as the session's user: every startup and History line of the
# session's logs, oldest first.
PROBE = ("for f in " + " ".join(f'"{log}"' for log in LOGS) + "; do "
         "[ -f \"$f\" ] && grep -a -E 'stderr\\] (APP-STARTUP|HISTORY-OPEN|tilo-gpu: window adapter) ' "
         "\"$f\"; done; true")

ANSI = re.compile(r"\x1b\[[0-9;]*m")
STARTUP = re.compile(r"\[app (?P<pkg>\S+) stderr\] APP-STARTUP app=(?P<app>\S+) "
                     r"total_ms=(?P<total>\d+) phases=(?P<phases>\S*)")
ADAPTER = re.compile(r"\[app (?P<pkg>\S+) stderr\] tilo-gpu: window adapter \"[^\"]*\" type=(?P<type>\w+)")
HISTORY = re.compile(r"\[app (?P<pkg>\S+) stderr\] HISTORY-OPEN listed=(?P<listed>\d+) "
                     r"earlier=(?P<earlier>\w+) ms=(?P<ms>\d+)")

# Process start to first frame, in milliseconds. One second for any app; the small,
# single-view apps hold a tighter one, since nothing they show at launch needs more.
DEFAULT_BUDGET_MS = 1000
SMALL_APP_BUDGET_MS = 600
SMALL_APPS = frozenset({
    "so.tilo.calculator", "so.tilo.weather", "so.tilo.snippets",
})
# On a CPU rasterizer: the VM checks' first-frame budget for a software-rendered VM
# (`files_first_frame_s` in run.py).
SOFTWARE_BUDGET_MS = 3000
HISTORY_BUDGET_MS = 1000


# A warm open on a CPU rasterizer, judged by the median of A15's ten opens: well under
# the per-launch software budget, which also covers a session's cold first launches.
# Since apps stopped starting OpenGL beside Vulkan and a GPU-less session pins
# lavapipe, the median is 0.6-0.8 s on a quiet runner and was at most ~1.2 s on a
# contended one before either change.
WARM_OPEN_SOFTWARE_BUDGET_MS = 1500


def warm_open_budget_ms(app: str, renderer: str = "gpu") -> int:
    """The median warm open's budget (A15): tighter than a launch's on a CPU rasterizer."""
    if renderer == "cpu":
        return WARM_OPEN_SOFTWARE_BUDGET_MS
    return budget_ms(app, renderer)


def budget_ms(app: str, renderer: str = "gpu") -> int:
    if renderer == "cpu":
        return SOFTWARE_BUDGET_MS
    return SMALL_APP_BUDGET_MS if app in SMALL_APPS else DEFAULT_BUDGET_MS


def parse_startups(log: str) -> list[dict]:
    """Every `APP-STARTUP` line in `log`, in order: app, total_ms, its phases, and the
    renderer ("gpu", "cpu", or "unknown") its latest adapter line before it named.

    Phase durations are intervals ending at the named marker, not spans named by
    that marker. Keep their order and repetitions in phase_intervals; phases
    retains the longest interval for each marker for the slowest-phase report.
    The summary omits sub-millisecond intervals, so it cannot reconstruct exact
    timestamps or span durations; those require the original event receipt.
    """
    out = []
    renderers: dict[str, str] = {}
    for line in ANSI.sub("", log).splitlines():
        adapter = ADAPTER.search(line)
        if adapter:
            renderers[adapter["pkg"]] = "cpu" if adapter["type"] == "Cpu" else "gpu"
            continue
        found = STARTUP.search(line)
        if not found:
            continue
        phases = {}
        phase_intervals = []
        for phase in filter(None, found["phases"].split(",")):
            name, _, ms = phase.rpartition(":")
            if name and ms.isdigit():
                elapsed = int(ms)
                phase_intervals.append({"marker": name, "ms": elapsed})
                phases[name] = max(phases.get(name, 0), elapsed)
        out.append({"app": found["app"], "total_ms": int(found["total"]), "phases": phases,
                    "phase_intervals": phase_intervals,
                    "renderer": renderers.get(found["pkg"], "unknown"), "line": found.group(0)})
    return out


def parse_history_opens(log: str) -> list[dict]:
    """Every `HISTORY-OPEN` line in `log`, in order."""
    return [{"app": m["pkg"], "listed": int(m["listed"]), "earlier": m["earlier"] == "true",
             "ms": int(m["ms"])}
            for m in (HISTORY.search(line) for line in ANSI.sub("", log).splitlines()) if m]


def slowest_phase(phases: dict[str, int]) -> str:
    if not phases:
        return "-"
    name, ms = max(phases.items(), key=lambda item: item[1])
    return f"until {name} {ms} ms"


def table(startups: list[dict]) -> list[str]:
    """One row per app, slowest first: launches, median and slowest first frame, budget,
    and the phase that took longest in its slowest launch."""
    by_app: dict[str, list[dict]] = {}
    for launch in startups:
        by_app.setdefault(launch["app"], []).append(launch)
    rows = []
    for app, launches in by_app.items():
        worst = max(launches, key=lambda launch: launch["total_ms"])
        rows.append((worst["total_ms"], app, len(launches),
                     int(statistics.median(launch["total_ms"] for launch in launches)),
                     worst["renderer"], slowest_phase(worst["phases"])))
    rows.sort(reverse=True)
    lines = ["| app | launches | median ms | slowest ms | renderer | budget ms | slowest phase |",
             "|---|---|---|---|---|---|---|"]
    lines += [f"| {app} | {count} | {median} | {worst} | {renderer} | {budget_ms(app, renderer)} | {phase} |"
              for worst, app, count, median, renderer, phase in rows]
    return lines


def over_budget(startups: list[dict], scale: float = 1.0) -> list[str]:
    """Each launch whose first frame came later than its app's budget times `scale`."""
    def budget(launch: dict) -> int:
        return int(budget_ms(launch["app"], launch["renderer"]) * scale)
    return [f"{launch['app']} took {launch['total_ms']} ms to its first frame on "
            f"{launch['renderer']} rendering (budget {budget(launch)} ms; slowest phase "
            f"{slowest_phase(launch['phases'])})"
            for launch in startups if launch["total_ms"] > budget(launch)]


def history_over_budget(opens: list[dict], scale: float = 1.0) -> list[str]:
    return [f"{opened['app']} took {opened['ms']} ms to open History on {opened['listed']} "
            f"changes (budget {int(HISTORY_BUDGET_MS * scale)} ms)"
            for opened in opens if opened["ms"] > HISTORY_BUDGET_MS * scale]

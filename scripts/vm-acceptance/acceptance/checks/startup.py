"""How fast Tilo apps start, and which GPU their windows render on.

A14 opens Files as the session's first Files launch and times it from the app's own
startup marks (process start to `first-frame-presented`, written by every Tilo app to
`<data home>/tilo-rooms/<room>/<package>/.local/state/startup-<N>.json`). A frame alone
is not a working window, so it also reads the folder listing off the app's AT-SPI tree
with the image's external AT-SPI client, the one the installed-image gate drives.

R05 judges the GPU: `tilo system health --gpu --json` and the adapter line every app
window logs (see acceptance/gpu.py). A VM without a render node cannot show a CPU
fallback, so there it is unobservable; `run.py --gl` boots the VM with virgl, which can.
"""

from __future__ import annotations

import itertools
import json
import re
import shlex
import time

from .. import app_startup, core, gpu, records
from ..core import CheckFailed, Unobservable, check
from ..vm import wait_until
from .apps import _close, _lifecycle

FILES = "so.tilo.file-explorer"
FILES_PROGRAM = "file-explorer"
# The Files manifest's AT-SPI root and the name of its folder listing (files-a11y-list).
FILES_ROOT = "Files activity"
FILES_LIST = "Files"
SESSION_LOG = "$HOME/.local/state/tilo/session-logs/latest.log"
MARKS_WAIT_S = 10
LISTING_WAIT_S = 30

ANSI = re.compile(r"\x1b\[[0-9;]*m")

# Walks the app's tree in the guest, where each AT-SPI call is one local gdbus: from the
# host every call would cross ssh. Prints one JSON object: the bus it read, whether the
# root was there, and the names in the folder listing (None: no listing found).
ATSPI_PROBE = r'''
import importlib.util, json, re, sys, time
CLIENT = "/usr/libexec/tilo/atspi-external-client.py"
program, root_name, list_name, timeout = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4])
try:
    spec = importlib.util.spec_from_file_location("atspi_client", CLIENT)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
except Exception as e:
    print(json.dumps({"error": f"no AT-SPI client at {CLIENT}: {e}"}))
    sys.exit(0)

# AtspiRole's list. AccessKit has no role-name method; roles are numbers.
ROLE_LIST = 31

def text(reply):
    # gdbus quotes a string in single quotes, or double quotes when it holds one.
    found = re.search(r"'((?:[^'\\]|\\.)*)'|\"((?:[^\"\\]|\\.)*)\"", reply)
    return (found.group(1) if found.group(1) is not None else found.group(2)) if found else ""

def walk(client, limit=800):
    pending, seen, names = [m.ROOT_PATH], set(), []
    while pending and len(seen) < limit:
        path = pending.pop(0)
        if path in seen:
            continue
        seen.add(path)
        name = text(client.property(path, "Name"))
        names.append(name)
        children = m.PATH_RE.findall(client.call(path, "org.a11y.atspi.Accessible.GetChildren"))
        if name == list_name and client.role(path) == ROLE_LIST:
            return names, [text(client.property(c, "Name")) for c in children]
        pending += [c for c in children if c not in seen]
    return names, None

deadline = time.monotonic() + timeout
last = {"error": f"no AT-SPI connection belongs to a {program} process"}
try:
    address = m.atspi_address()
except Exception as e:
    print(json.dumps({"error": f"no AT-SPI bus: {e}"}))
    sys.exit(0)
while True:
    for bus, pid in m.bus_pids(address).items():
        cmd = m.process_command(m.app_process(pid)).split()
        if not cmd or cmd[0].rsplit("/", 1)[-1] != program:
            continue
        try:
            names, entries = walk(m.AtspiClient(address, bus))
        except Exception as e:
            last = {"bus": bus, "error": f"walk failed: {e}"}
            continue
        last = {"bus": bus, "root": root_name in names, "entries": entries, "names": names[:80]}
        if last["root"] and entries:
            print(json.dumps(last))
            sys.exit(0)
    if time.monotonic() >= deadline:
        print(json.dumps(last))
        sys.exit(0)
    time.sleep(0.5)
'''


def parse_startup_marks(text: str, since_ms: int) -> dict | None:
    """The startup marks of the first launch that began at or after `since_ms` (the guest's
    clock before the open): {"path", "origin_epoch_ms", "events": {name: seconds}}. Input is
    one `<path>\\t<json>` line per startup file; unreadable or partial files are skipped."""
    best = None
    for line in text.splitlines():
        path, _, body = line.partition("\t")
        try:
            doc = json.loads(body)
        except ValueError:
            continue
        origin = doc.get("origin_epoch_ms") if isinstance(doc, dict) else None
        if not isinstance(origin, int) or origin < since_ms:
            continue
        if best is None or origin < best["origin_epoch_ms"]:
            events = {}
            for e in doc.get("events") or []:
                if isinstance(e, dict) and isinstance(e.get("ns"), int) and e.get("name") not in events:
                    events[e["name"]] = round(e["ns"] / 1e9, 3)
            best = {"path": path, "origin_epoch_ms": origin, "events": events}
    return best


def parse_launch_latency(log: str, package: str) -> float | None:
    """Seconds from the open request to the first presented frame, from the compositor's
    `tilo::launch_latency` line for `package` (logged at info, so only when the session's
    log level includes it). The first such line in `log`, else None."""
    for line in ANSI.sub("", log).splitlines():
        if f"package_id={package} " not in line + " " and f'package_id="{package}"' not in line:
            continue
        found = re.search(r"request_to_first_present_us=(\d+)", line)
        if found:
            return round(int(found.group(1)) / 1e6, 3)
    return None


def first_frame_budget(budgets: dict, hardware: bool) -> tuple[float, str]:
    """The first-frame budget in seconds and what it applies to: real display hardware
    gets the hardware budget, a VM the one for its CPU rasterizer."""
    if hardware:
        return budgets["files_first_frame_hw_s"], "real display hardware"
    return budgets["files_first_frame_s"], "a VM (software or virgl rendering)"


def judge_listing(probe: dict, marker: str) -> tuple[str, str]:
    """("pass" | "fail" | "unobservable", why) for the AT-SPI probe's result."""
    if "root" not in probe:
        return "unobservable", f"Files' AT-SPI tree could not be read: {probe.get('error', probe)}"
    if not probe["root"]:
        return "fail", (f"Files' AT-SPI tree has no {FILES_ROOT!r} root "
                        f"(names: {probe.get('names', [])[:20]})")
    entries = probe.get("entries")
    if entries is None:
        return "fail", (f"Files shows no folder listing (no {FILES_LIST!r} list in its AT-SPI tree; "
                        f"names: {probe.get('names', [])[:20]})")
    if not entries:
        return "fail", "Files' folder listing is empty although the home folder has entries"
    where = "the home folder" if marker in entries else "a folder other than home"
    return "pass", f"Files lists {len(entries)} entries of {where}"


def _first_int(text: str) -> int | None:
    return next((int(w) for w in text.split() if w.isdigit()), None)


def _marks(ctx, since_ms: int, app: str = FILES, wait_s: float = MARKS_WAIT_S):
    """The startup marks of `app`'s first launch since `since_ms`, once they hold its
    first presented frame (or whatever they hold after `wait_s`)."""
    script = (f'for f in "${{XDG_DATA_HOME:-$HOME/.local/share}}"/tilo-rooms/*/{app}/.local/state/startup-*.json; '
              'do [ -f "$f" ] || continue; printf "%s\\t" "$f"; tr -d "\\n" < "$f"; echo; done')

    def probe():
        m = parse_startup_marks(ctx.vm.exec(script, check=False), since_ms)
        return m if m and "first-frame-presented" in m["events"] else None
    return wait_until(probe, wait_s, 1.0) or parse_startup_marks(ctx.vm.exec(script, check=False), since_ms)


@check("A14", "apps", "Files draws its first frame within budget and lists the folder it opens on")
def files_start(ctx, res):
    label = "A14-files-start"
    ctx.reset()
    marker = f"acc-files-{int(time.time())}"
    ctx.vm.exec(f'mkdir -p "$HOME/{marker}"')
    since_ms = _first_int(ctx.vm.exec("date +%s%3N"))
    log_lines = _first_int(ctx.vm.exec(f'wc -l < "{SESSION_LOG}" 2>/dev/null || echo 0', check=False)) or 0
    problems, unobserved = [], []
    try:
        gid, procs, toasts0 = _lifecycle(ctx, res, FILES, "applications", label, True)
        marks = _marks(ctx, since_ms) if since_ms is not None else None
        log = ctx.vm.exec(f'tail -n +{log_lines + 1} "{SESSION_LOG}" | grep -a "tilo::launch_latency"',
                          check=False)
        request_s = parse_launch_latency(log, FILES)
        probe_out = ctx.vm.exec(
            f"python3 - {FILES_PROGRAM} {shlex.quote(FILES_ROOT)} {shlex.quote(FILES_LIST)} "
            f"{LISTING_WAIT_S} <<'EOF'\n{ATSPI_PROBE}\nEOF", timeout=LISTING_WAIT_S + 60, check=False)
        ctx.shot(f"{label}-listing")
        try:
            _close(ctx, res, label, gid, procs, toasts0)
        except CheckFailed as exc:
            problems.append(str(exc))
    finally:
        ctx.vm.exec(f'rmdir "$HOME/{marker}" 2>/dev/null; true', check=False)

    budget, applies = first_frame_budget(ctx.config["budgets"], (ctx.facts.get("display") or {}).get("hardware", False))
    res.metrics["first_frame_budget_s"] = budget
    res.metrics["renderer"] = ctx.facts.get("renderer")
    if request_s is not None:
        res.metrics["request_to_first_present_s"] = request_s
    if marks is None or "first-frame-presented" not in marks["events"]:
        unobserved.append("Files wrote no `first-frame-presented` startup mark within "
                          f"{MARKS_WAIT_S} s of its first frame ({marks['path'] if marks else 'no startup file'}; "
                          f"polled first frame {res.metrics.get('seconds_to_first_frame')} s)")
    else:
        first = marks["events"]["first-frame-presented"]
        res.metrics.update(process_start_to_first_frame_s=first, startup_marks=marks["events"])
        ctx.record(f"app.startup_ms.{_short(FILES)}.first_in_session", "ms", [first * 1000],
                   clock="process start to first presented frame (the app's startup marks)")
        if first > budget:
            problems.append(f"Files took {first} s from process start to its first frame, "
                            f"over the {budget} s budget for {applies}")

    try:
        probe = json.loads(next(line for line in probe_out.splitlines() if line.startswith("{")))
    except (StopIteration, ValueError):
        probe = {"error": probe_out.strip()[-400:] or "the AT-SPI probe printed nothing"}
    ctx.write(f"{label}-atspi.json", json.dumps(probe, indent=1))
    res.metrics["listing"] = (probe.get("entries") or [])[:20]
    verdict, why = judge_listing(probe, marker)
    if verdict == "fail":
        problems.append(why)
    elif verdict == "unobservable":
        unobserved.append(why)
    if problems:
        raise CheckFailed("; ".join(problems + unobserved))
    if unobserved:
        raise Unobservable("; ".join(unobserved))
    res.detail = why


@check("R05", "resources", "App windows render on the machine's GPU, not on a CPU fallback")
def gpu_adapters(ctx, res):
    def read():
        out = ctx.vm.exec(gpu.PROBE, timeout=90, check=False)
        health_text, log = gpu.split_probe(out)
        return out, health_text, gpu.parse_health(health_text), gpu.parse_adapters(log), gpu.parse_fallbacks(log)

    out, health_text, health, adapters, fallbacks = read()
    if not adapters and (health or {}).get("status") in ("ok", "degraded"):
        # Run on its own, no app has opened a window yet: open Files so one reports.
        res.metrics["opened_for_check"] = FILES
        ctx.reset()
        ctx.open_app(FILES)
        wait_until(lambda: gpu.parse_adapters(gpu.split_probe(ctx.vm.exec(gpu.PROBE, timeout=90, check=False))[1]),
                   30, 2.0)
        out, health_text, health, adapters, fallbacks = read()
        ctx.reset()
    ctx.write("R05-gpu.txt", out)
    res.metrics.update(health=health, adapters=gpu.summary(adapters), cpu_fallbacks=fallbacks,
                       renderer=ctx.facts.get("renderer"))
    verdict, why = gpu.judge(health, adapters, fallbacks, health_text)
    if verdict == "fail":
        raise CheckFailed(why)
    if verdict == "unobservable":
        raise Unobservable(why)
    res.detail = why


# -- warm opens and every launch of the run (the installed-desktop startup gate) ----

WARM_APPS = ("so.tilo.file-explorer", "so.tilo.notes", "so.tilo.settings")
WARM_LAUNCHES = 10
OPEN_WAIT_S = 30
EXIT_WAIT_S = 15
_NUDGE = itertools.cycle((8, 12))


def _short(app: str) -> str:
    return app.rsplit(".", 1)[-1]


def open_times(t0_ms: int, marks: dict | None) -> tuple[float, float] | None:
    """(request to first presented frame, process start to first presented frame), in
    ms, for a launch requested at guest time `t0_ms` whose startup marks are `marks`.
    The request is stamped in the session just before `tilo-control` asks the
    compositor to open the app, so the first number holds the compositor's dispatch,
    the launcher and the sandbox setup as well as the app's own start."""
    if not marks or "first-frame-presented" not in marks["events"]:
        return None
    first_ms = marks["events"]["first-frame-presented"] * 1000
    return round(marks["origin_epoch_ms"] + first_ms - t0_ms, 1), round(first_ms, 1)


def median(values: list[float]) -> float:
    """The lower median, a value that was measured."""
    return sorted(values)[(len(values) - 1) // 2]


def _app_gone(ctx, app: str) -> bool:
    return wait_until(lambda: not [p for p in core.processes(ctx)["procs"].values() if p["app"] == app],
                      EXIT_WAIT_S, 1.0) is not None


def _timed_open(ctx, app: str) -> tuple[float, float] | None:
    """Open `app` from the slot's session as a person's request does, wait for its first
    frame, close every window and wait until its processes are gone."""
    out = ctx.vm.exec(f"date +%s%3N; tilo-control actions invoke applications {shlex.quote(app)} >/dev/null",
                      timeout=60)
    t0 = _first_int(out)
    times = open_times(t0, _marks(ctx, t0, app, OPEN_WAIT_S)) if t0 is not None else None
    ctx.reset()
    # Opens through the control socket are no user activity: without a real input the
    # session locks itself on idle a few minutes into the series, as a person's would.
    # The pointer moves between launches, never inside a measured one.
    ctx.vm.input("move", str(next(_NUDGE)), "300")
    if not _app_gone(ctx, app):
        raise CheckFailed(f"{app}'s processes were still running {EXIT_WAIT_S} s after its window closed, "
                          "so the next open would not start it afresh")
    return times


@check("A15", "apps", "Warm opens of Files, Notes and Settings reach their first frame within budget")
def warm_opens(ctx, res):
    """Each app is opened and closed once unmeasured, so its files are in the page cache,
    then ten times measured. The budget is the app's (acceptance/app_startup.py) on the
    session's renderer, held by the median; every launch is recorded."""
    software = records.renderer(ctx.facts.get("renderer"), ctx.facts.get("display")) == "lavapipe"
    kind = "cpu" if software else "gpu"
    launches = WARM_LAUNCHES
    ctx.reset()
    ctx.vm.input("move", str(next(_NUDGE)), "300")
    problems, unobserved, summary = [], [], {}
    for app in WARM_APPS:
        _timed_open(ctx, app)
        opens, starts = [], []
        for _ in range(launches):
            times = _timed_open(ctx, app)
            if times is not None:
                opens.append(times[0])
                starts.append(times[1])
        short = _short(app)
        ctx.record(f"app.open.warm_ms.{short}", "ms", opens, app=app,
                   what="open request to the app's first presented frame, launcher and sandbox included",
                   clock="guest wall clock; the request is stamped in the session before tilo-control")
        ctx.record(f"app.startup_ms.{short}", "ms", starts, app=app,
                   what="process start to the app's first presented frame")
        budget = app_startup.budget_ms(app, kind)
        summary[app] = {"open_ms": opens, "startup_ms": starts, "budget_ms": budget}
        if len(opens) < launches:
            unobserved.append(f"{app}: {launches - len(opens)} of {launches} opens wrote no first-frame "
                              f"startup mark within {OPEN_WAIT_S} s")
        if opens and median(opens) > budget:
            problems.append(f"{app} took {median(opens)} ms (median of {len(opens)}) from the open request "
                            f"to its first frame, over its {budget} ms budget on {kind} rendering "
                            f"(process start to first frame: median {median(starts)} ms)")
    res.metrics.update(warm_opens=summary, renderer=ctx.facts.get("renderer"))
    ctx.write("A15-warm-opens.json", json.dumps(summary, indent=1))
    if problems:
        raise CheckFailed("; ".join(problems + unobserved))
    if unobserved:
        raise Unobservable("; ".join(unobserved))


@check("A16", "apps", "Every Tilo app the run launched drew its first frame within its budget")
def every_launch(ctx, res):
    """The installed-desktop gate's judgement (app_startup_check.py) over this session:
    each `APP-STARTUP` line against its app's budget on the renderer its window used,
    History opens within 1 s, and Notes among the launches."""
    log = ctx.vm.exec(app_startup.PROBE, timeout=90, check=False)
    startups = app_startup.parse_startups(log)
    opens = app_startup.parse_history_opens(log)
    ctx.write("A16-app-startup.txt", "\n".join(app_startup.table(startups) + [s["line"] for s in startups]))
    apps = sorted({s["app"] for s in startups})
    res.metrics.update(launches=len(startups), apps=apps, history_opens=opens)
    if not startups:
        raise Unobservable("no app logged an APP-STARTUP line in the session's logs")
    problems = app_startup.over_budget(startups) + app_startup.history_over_budget(opens)
    if "so.tilo.notes" not in apps:
        problems.append("so.tilo.notes logged no APP-STARTUP line")
    if problems:
        raise CheckFailed("; ".join(problems))
    res.detail = f"{len(startups)} launches of {len(apps)} apps within budget"

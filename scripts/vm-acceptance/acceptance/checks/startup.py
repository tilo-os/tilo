"""How fast Tilo apps start, and which GPU their windows render on.

A14 opens Files as the session's first Files launch and times it from the app's own
startup marks (process start to `first-frame-presented`, written by every Tilo app to
`<data home>/tilo-rooms/<room>/<package>/.local/state/startup-<N>.json`). A frame alone
is not a working window, so it also reads the folder listing off the app's AT-SPI tree
with the checkout's external AT-SPI client (tools/atspi_external_client.py), copied into
the guest for the check: it is test tooling and the image does not ship it.

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
import tomllib
from pathlib import Path

from .. import app_startup, core, gpu, records, session_unit
from ..core import CheckFailed, Skipped, Unobservable, check
from ..vm import VmError, wait_until
from .apps import _close, _lifecycle
from .resources import shell_anon_mib, slope

FILES = "so.tilo.file-explorer"
FILES_PROGRAM = "file-explorer"
# The Files manifest's AT-SPI root and the name of its folder listing (files-a11y-list).
FILES_ROOT = "Files activity"
FILES_LIST = "Files"
SESSION_LOG = "$HOME/.local/state/tilo/session-logs/latest.log"
MARKS_WAIT_S = 10
LISTING_WAIT_S = 30
KPI_BUDGETS = Path(__file__).resolve().parents[4] / "tools/perf/kpi-budgets.toml"
ATSPI_CLIENT = Path(__file__).resolve().parents[4] / "tools/atspi_external_client.py"
GUEST_ATSPI_CLIENT = ".cache/acceptance/atspi_external_client.py"

ANSI = re.compile(r"\x1b\[[0-9;]*m")

# Walks the app's tree in the guest, where each AT-SPI call is one local gdbus: from the
# host every call would cross ssh. Prints one JSON object: the bus it read, whether the
# root was there, and the names in the folder listing (None: no listing found). When it
# finds no connection of the program it prints every connection instead, attributed to
# its process, so the report separates "nothing published" from "published under a
# process this reader does not recognise".
ATSPI_PROBE = r'''
import importlib.util, json, os, re, sys, time
CLIENT = os.environ["TILO_ATSPI_CLIENT"]
program, root_name, list_name, timeout = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4])
try:
    spec = importlib.util.spec_from_file_location("atspi_client", CLIENT)
    m = importlib.util.module_from_spec(spec)
    # dataclasses resolves the client's string annotations through sys.modules.
    sys.modules[spec.name] = m
    spec.loader.exec_module(m)
except Exception as e:
    if isinstance(e, FileNotFoundError) and e.filename == CLIENT:
        error = f"no AT-SPI client at {CLIENT}: {e}"
    else:
        error = f"the AT-SPI client at {CLIENT} failed to load: {type(e).__name__}: {e}"
    print(json.dumps({"error": error}))
    sys.exit(0)

# AtspiRole's list and table. AccessKit has no role-name method; roles are numbers. Files'
# listing is the toolkit's file table (a row per entry, named for its file); a plain list
# names its items the same way.
ROLE_LIST = 31
ROLE_TABLE = 55
LISTING_ROLES = (ROLE_LIST, ROLE_TABLE)

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
        if name == list_name and client.role(path) in LISTING_ROLES:
            return names, [text(client.property(c, "Name")) for c in children]
        pending += [c for c in children if c not in seen]
    return names, None

def a11y_enabled():
    reply = m.command("gdbus", "call", "--session", "--dest", "org.a11y.Bus", "--object-path",
                      "/org/a11y/bus", "--method", "org.freedesktop.DBus.Properties.Get",
                      "org.a11y.Status", "IsEnabled", check=False)
    return "true" in reply

def set_a11y(on):
    m.command("busctl", "--user", "set-property", "org.a11y.Bus", "/org/a11y/bus",
              "org.a11y.Status", "IsEnabled", "b", "true" if on else "false", check=False)

deadline = time.monotonic() + timeout
found = None
# Every connection the walk found on the last pass, whether it belonged to PROGRAM.
clients = []
try:
    address = m.atspi_address()
except Exception as e:
    print(json.dumps({"error": f"no AT-SPI bus: {e}"}))
    sys.exit(0)
# An app publishes its tree on the AT-SPI bus only while assistive technology
# is on (org.a11y.Status IsEnabled), which nothing in a fresh session turns
# on. A screen reader turns it on as it starts; so does this reader, and it
# leaves the session as it found it, so later checks measure the session a
# person without assistive technology has.
was_enabled = a11y_enabled()
if not was_enabled:
    set_a11y(True)
def report(result):
    print(json.dumps({**result, "a11y_was_enabled": was_enabled}))
    if not was_enabled:
        set_a11y(False)
    sys.exit(0)
while True:
    clients = []
    for bus, pid in m.bus_pids(address).items():
        app = m.app_process(pid)
        cmd = m.process_command(app).split()
        own = bool(cmd) and cmd[0].rsplit("/", 1)[-1] == program
        try:
            names, entries = walk(m.AtspiClient(address, bus))
            client = {"bus": bus, "pid": pid, "app_pid": app, "command": " ".join(cmd)[:160],
                      "root": root_name in names, "entries": entries, "names": names[:20]}
        except Exception as e:
            client = {"bus": bus, "pid": pid, "app_pid": app, "command": " ".join(cmd)[:160],
                      "error": f"walk failed: {e}"}
        clients.append(client)
        if not own:
            continue
        found = client
        if found["root"] and entries:
            report(found)
    if time.monotonic() >= deadline:
        report(found or {"error": f"no AT-SPI connection belongs to a {program} process",
                         "clients": clients[:20]})
    time.sleep(0.5)
'''


def parse_startup_marks(text: str, since_ms: int) -> dict | None:
    """The startup marks of the first launch that began at or after `since_ms` (the guest's
    clock before the open): {"path", "origin_epoch_ms", "origin_epoch_us", "events": {name:
    seconds}, "launcher": [(name, epoch_us)], "exec_to_main_us"}. `launcher` holds the
    phase marks the launcher handed the app (empty from a launcher that hands none) and
    `exec_to_main_us` the time from the process's creation to its `main` (None where the
    app did not measure it). Input is one `<path>\\t<json>` line per startup file;
    unreadable or partial files are skipped."""
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
            origin_us = doc.get("origin_epoch_us")
            launcher = [(e["name"], e["epoch_us"]) for e in doc.get("launcher") or []
                        if isinstance(e, dict) and isinstance(e.get("name"), str)
                        and isinstance(e.get("epoch_us"), int)]
            exec_to_main = doc.get("exec_to_main_us")
            best = {"path": path, "origin_epoch_ms": origin,
                    "origin_epoch_us": origin_us if isinstance(origin_us, int) else origin * 1000,
                    "events": events, "launcher": launcher,
                    "exec_to_main_us": exec_to_main if isinstance(exec_to_main, int) else None}
    return best


# The compositor's per-launch timing fields (crates/system/tilo-startup-metrics), each
# on one of its three `tilo::launch_latency` lines.
LATENCY_FIELDS = ("request_to_spawn", "spawn_to_map", "map_to_first_present", "request_to_first_present")


def parse_launch_latencies(log: str, package: str) -> dict[str, float]:
    """{field: ms} from the compositor's `tilo::launch_latency` lines for `package`, the
    first value of each field in `log`; a field no line carries is absent. The spawn
    line's `dispatch_phases` (where `request_to_spawn` went: each phase's interval, ending
    at it) become `dispatch_phase.<phase>`."""
    out: dict[str, float] = {}
    for line in ANSI.sub("", log).splitlines():
        if f"package_id={package} " not in line + " " and f'package_id="{package}"' not in line:
            continue
        for field in LATENCY_FIELDS:
            found = re.search(rf"\b{field}_us=(\d+)", line)
            if found and field not in out:
                out[field] = round(int(found.group(1)) / 1e3, 1)
        phases = re.search(r"\bdispatch_phases=(\S+)", line)
        if phases and not any(name.startswith("dispatch_phase.") for name in out):
            for entry in phases.group(1).split(","):
                phase, _, ms = entry.partition(":")
                if re.fullmatch(r"[a-z-]+", phase) and re.fullmatch(r"\d+(\.\d+)?", ms):
                    out[f"dispatch_phase.{phase}"] = float(ms)
    return out


def parse_launch_latency(log: str, package: str) -> float | None:
    """Seconds from the open request to the first presented frame, from the compositor's
    `tilo::launch_latency` line for `package`. The first such line in `log`, else None."""
    ms = parse_launch_latencies(log, package).get("request_to_first_present")
    return None if ms is None else round(ms / 1e3, 3)


def open_segments(t0_ms: int, marks: dict | None) -> dict | None:
    """Where one open requested at guest time `t0_ms` spent its time, in ms, as segments
    that add up to the open: `request_to_launcher` (tilo-control, the compositor's
    dispatch and the launcher's exec), `launcher` (its checks and sandbox preparation, to
    its `sandbox-argv-ready` mark), `sandbox_to_main` (bubblewrap, the Landlock shim, the
    dynamic loader) and `main_to_first_frame` (the app's own start). `launcher_phases`
    holds each mark's interval, ending at that mark, the sandbox shim's
    `sandbox-entered` (bubblewrap) and `sandbox-exec` (Landlock) among them;
    `exec_to_main`, from `sandbox-exec` to `main`, is the loader. None when the marks hold
    no first frame or no launcher phases."""
    if not marks or "first-frame-presented" not in marks["events"]:
        return None
    launcher = marks.get("launcher") or []
    if len(launcher) < 2 or launcher[0][0] != "launcher-start":
        return None
    at = dict(launcher)
    start_us = launcher[0][1]
    handoff_us = at.get("sandbox-argv-ready", launcher[-1][1])
    phases, previous = {}, start_us
    for name, epoch_us in launcher[1:]:
        phases[name] = round((epoch_us - previous) / 1e3, 1)
        previous = epoch_us
    origin_us = marks["origin_epoch_us"]
    out = {"request_to_launcher": round(start_us / 1e3 - t0_ms, 1),
           "launcher": round((handoff_us - start_us) / 1e3, 1),
           "sandbox_to_main": round((origin_us - handoff_us) / 1e3, 1),
           "main_to_first_frame": round(marks["events"]["first-frame-presented"] * 1e3, 1),
           "launcher_phases": phases}
    if "sandbox-exec" in at:
        out["exec_to_main"] = round((origin_us - at["sandbox-exec"]) / 1e3, 1)
    elif marks.get("exec_to_main_us") is not None:
        out["exec_to_main"] = round(marks["exec_to_main_us"] / 1e3, 1)
    return out


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
    ctx.vm.write_home_file(GUEST_ATSPI_CLIENT, ATSPI_CLIENT.read_bytes())
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
            f'TILO_ATSPI_CLIENT="$HOME/{GUEST_ATSPI_CLIENT}" python3 - {FILES_PROGRAM} '
            f"{shlex.quote(FILES_ROOT)} {shlex.quote(FILES_LIST)} "
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
    if ctx.facts.get("software_rendering"):
        raise Skipped("needs a GPU render node; this run renders in software")

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
# After the measured series: Files, the app whose loss of the session was worst (every
# later open of it waited 30 s and spawned nothing), opened and closed this many times more.
REPEATED_OPENS = ((FILES, 20),)
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


def _app_gone(ctx, app: str) -> dict | None:
    """The first process table without `app`'s processes, or None if they outlived the wait."""
    def without_app():
        table = core.processes(ctx)
        return None if any(p["app"] == app for p in table["procs"].values()) else table

    return wait_until(without_app, EXIT_WAIT_S, 1.0)


def _timed_open(ctx, app: str, shell_anon: list[float] | None = None,
                restarts: list[str] | None = None, label: str = "") -> dict | None:
    """Open `app` from the slot's session as a person's request does, wait for its first
    frame, close every window and wait until its processes are gone. Returns the open's
    `open_times`, its `open_segments` and the compositor's `launch_latency` fields; appends
    the compositor's anonymous RSS once the app is gone to `shell_anon`, and to `restarts`
    why the open fails if the session restarted on its own around it (`label` names the
    open there)."""
    out = ctx.vm.exec(f'wc -l < "{SESSION_LOG}" 2>/dev/null || echo 0; date +%s%3N; '
                      f"tilo-control actions invoke applications {shlex.quote(app)} >/dev/null",
                      timeout=60)
    numbers = [int(word) for word in out.split() if word.isdigit()]
    log_lines, t0 = (numbers + [None, None])[:2]
    result = None
    if t0 is not None:
        marks = _marks(ctx, t0, app, OPEN_WAIT_S)
        times = open_times(t0, marks)
        if times is not None:
            log = ctx.vm.exec(f'tail -n +{(log_lines or 0) + 1} "{SESSION_LOG}" | grep -a "tilo::launch_latency"',
                              check=False)
            # `marks` is every startup mark of the launch (ms since its `main`, first of each
            # name), so a per-step table reads the launch itself rather than A16's summary
            # line, which drops sub-millisecond intervals.
            result = {"open": times[0], "startup": times[1], "segments": open_segments(t0, marks),
                      "compositor": parse_launch_latencies(log, app),
                      "marks": {name: round(seconds * 1e3, 1)
                                for name, seconds in sorted(marks["events"].items(), key=lambda item: item[1])}}
        else:
            # No first frame: keep what the session said about this app since the request
            # (a launch refused or held, say waiting for its previous instance to retire).
            said = ctx.vm.exec(f'tail -n +{(log_lines or 0) + 1} "{SESSION_LOG}" | sed "s/\\x1b\\[[0-9;]*m//g" '
                               f'| grep -a -e {shlex.quote(app)} -e app-writer -e shell-launch '
                               '| tail -n 80 | cut -c1-500; echo "--- the session\'s authority and state writes";'
                               f' sed "s/\\x1b\\[[0-9;]*m//g" "{SESSION_LOG}" | grep -aiE '
                               '"authority_rejection|launch refused|native state|state writer|state-file|fenced|persistence failed|publication did not|stale native" '
                               '| grep -av "tilo-sandbox-exec" | tail -n 40 | cut -c1-500', check=False)
            said = said or "the session log holds no line about this app since the request"
            check_id = ctx.current.id if ctx.current is not None else "A15"
            ctx.write(f"{check_id}-no-first-frame-{re.sub(r'[^A-Za-z0-9]+', '-', label or app).strip('-')}.log",
                      said)
            ctx.facts["last_no_first_frame"] = said
    if restarts is not None:
        # Read before the reset: the dump then holds the seconds around the open.
        restarted = core.session_restart(ctx, label or app)
        if restarted:
            restarts.append(restarted)
    ctx.reset()
    # Opens through the control socket are no user activity: without a real input the
    # session locks itself on idle a few minutes into the series, as a person's would.
    # The pointer moves between launches, never inside a measured one.
    ctx.vm.input("move", str(next(_NUDGE)), "300")
    table = _app_gone(ctx, app)
    if not table:
        raise CheckFailed(f"{app}'s processes were still running {EXIT_WAIT_S} s after its window closed, "
                          "so the next open would not start it afresh")
    if shell_anon is not None:
        anon = shell_anon_mib(table, ctx.vm.slot_uid())
        if anon is not None:
            shell_anon.append(anon)
    return result


# The segments of an open before the app's `main`, recorded as `app.open.<name>_ms.<app>`;
# with `app.startup_ms.<app>` (main to first frame) they add up to the open.
SEGMENTS = ("request_to_launcher", "launcher", "sandbox_to_main", "exec_to_main")


def segment_series(results: list[dict]) -> dict[str, list[float]]:
    """{KPI name stem: one value per open} over the opens' segments before `main`, the
    launcher's phases (`launcher_phase.<mark>`) and the compositor's fields; an open that
    lacks one leaves it out."""
    series: dict[str, list[float]] = {}
    for result in results:
        segments = result.get("segments") or {}
        values = {name: segments[name] for name in SEGMENTS if name in segments}
        values.update({f"launcher_phase.{mark}": ms for mark, ms in segments.get("launcher_phases", {}).items()})
        values.update(result.get("compositor") or {})
        for name, value in values.items():
            series.setdefault(name, []).append(value)
    return series


def kpi_p95_target(kpi: str) -> float | None:
    """The KPI's p95 ceiling for `kpi` (tools/perf/kpi-budgets.toml), if any."""
    budgets = tomllib.loads(KPI_BUDGETS.read_text(encoding="utf-8"))
    target = budgets.get("kpi", {}).get(kpi, {}).get("p95")
    return None if target is None else float(target)


def p95(values: list[float]) -> float:
    """The nearest-rank 95th percentile, the perf records' rule."""
    ordered = sorted(values)
    return ordered[max(0, -(-len(ordered) * 95 // 100) - 1)]


@check("A15", "apps", "Warm opens of Files, Notes and Settings reach their first frame within budget")
def warm_opens(ctx, res):
    """Each app is opened and closed once unmeasured, so its files are in the page cache,
    then ten times measured. The budget is the app's (acceptance/app_startup.py) on the
    session's renderer, held by the median; on a GPU the KPI's p95 target holds as
    well. Every launch is recorded, split into segments that add up to the open (the
    launcher's phases among them) and the compositor's own request/spawn/map times. Then
    Files opens `REPEATED_OPENS` times more, each to its first frame; a restart of the
    session around any open fails the check by that open (`core.session_restart`)."""
    software = records.renderer(ctx.facts.get("renderer"), ctx.facts.get("display")) == "lavapipe"
    kind = "cpu" if software else "gpu"
    launches = WARM_LAUNCHES
    ctx.reset()
    ctx.vm.input("move", str(next(_NUDGE)), "300")
    problems, unobserved, summary = [], [], {}
    # The compositor's anonymous RSS after each open has closed, in order: what an open
    # and close leaves behind in it.
    shell_anon: list[float] = []
    restarts: list[str] = []
    for app in WARM_APPS:
        short = _short(app)
        _timed_open(ctx, app, shell_anon, restarts, f"the {short} warm-up open")
        results = [result for result in (_timed_open(ctx, app, shell_anon, restarts, f"{short} open #{k + 1}")
                                         for k in range(launches)) if result]
        opens = [result["open"] for result in results]
        starts = [result["startup"] for result in results]
        ctx.record(f"app.open.warm_ms.{short}", "ms", opens, app=app,
                   what="open request to the app's first presented frame, launcher and sandbox included",
                   clock="guest wall clock; the request is stamped in the session before tilo-control")
        ctx.record(f"app.startup_ms.{short}", "ms", starts, app=app,
                   what="process start to the app's first presented frame")
        series = segment_series(results)
        for name, values in series.items():
            stem, _, mark = name.partition(".")
            ctx.record(f"app.open.{stem}_ms.{short}" + (f".{mark}" if mark else ""), "ms", values, app=app,
                       what="one segment of the warm open (acceptance/checks/startup.py open_segments)")
        budget = app_startup.warm_open_budget_ms(app, kind)
        summary[app] = {"open_ms": opens, "startup_ms": starts, "budget_ms": budget,
                        "median_ms": {name: median(values) for name, values in series.items()},
                        "opens": results}
        if len(opens) < launches:
            unobserved.append(f"{app}: {launches - len(opens)} of {launches} opens wrote no first-frame "
                              f"startup mark within {OPEN_WAIT_S} s")
        unsplit = sum(1 for result in results if result["segments"] is None)
        if unsplit:
            unobserved.append(f"{app}: {unsplit} of {len(results)} opens carry no launcher phase marks, "
                              "so the time before the app's main is not split")
        untimed = sum(1 for result in results if "request_to_spawn" not in result["compositor"])
        if untimed:
            unobserved.append(f"{app}: the session log holds no compositor launch timing for {untimed} "
                              f"of {len(results)} opens")
        if opens and median(opens) > budget:
            problems.append(f"{app} took {median(opens)} ms (median of {len(opens)}) from the open request "
                            f"to its first frame, over its {budget} ms budget on {kind} rendering "
                            f"(process start to first frame: median {median(starts)} ms)")
        target = kpi_p95_target(f"app.open.warm_ms.{short}")
        if kind == "gpu" and opens and target is not None and p95(opens) > target:
            problems.append(f"{app} took {p95(opens)} ms (p95 of {len(opens)}) from the open request to its "
                            f"first frame on a GPU, over the KPI's {target:.0f} ms")
    # Then one app over and over, unmeasured: the session must outlive it and the app open
    # every time. Its RSS stays out of `shell_anon`, whose slope is a KPI over the series above.
    repeated = {}
    for app, count in REPEATED_OPENS:
        short = _short(app)
        missed = [k + 1 for k in range(count)
                  if _timed_open(ctx, app, None, restarts, f"{short} repeated open #{k + 1}") is None]
        repeated[app] = {"opens": count, "no_first_frame": missed}
        if missed:
            problems.append(f"{app}: {len(missed)} of {count} repeated opens (#{', #'.join(map(str, missed[:8]))}) "
                            f"drew no first frame within {OPEN_WAIT_S} s")
    problems[:0] = restarts
    res.metrics.update(warm_opens={app: {k: v for k, v in entry.items() if k != "opens"}
                                   for app, entry in summary.items()},
                       repeated_opens=repeated, session_restarts=len(restarts),
                       renderer=ctx.facts.get("renderer"))
    ctx.write("A15-warm-opens.json", json.dumps(summary, indent=1))
    growth = slope(shell_anon)
    res.metrics.update(shell_anon_after_close_mib=shell_anon, shell_anon_mib_per_open_close=growth)
    ctx.record("memory.shell_anon_mib.per_open_close", "MiB", [growth], opens=len(shell_anon),
               what="least-squares slope of the compositor's anonymous RSS over warm open/close cycles")
    if growth is None:
        unobserved.append(f"the compositor's anonymous RSS was read after {len(shell_anon)} closes, "
                          "too few for its growth per open")
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
    # A failed check keeps no files, so the slowest late launch's own phases go into the
    # failure text: which marks the first frame waited on, and whether the window named
    # its adapter (a launch with no adapter line is judged on the hardware budget).
    late = [s for s in startups if s["total_ms"] > app_startup.budget_ms(s["app"], s["renderer"])]
    if late:
        slowest = max(late, key=lambda s: s["total_ms"])
        unnamed = (f" ({slowest['app']} logged no `tilo-gpu: window adapter` line before it)"
                   if slowest["renderer"] == "unknown" else "")
        problems.append(f"slowest late launch{unnamed}: {slowest['line'][:1500]}")
        # The broker's own account of that app's joins (`COLLAB-JOIN`): a join slower on the
        # app's side than here waited for the broker's thread.
        joins = [line[line.index("COLLAB-JOIN"):] for line in ctx.vm.exec(
                     f'grep -a "COLLAB-JOIN channel=" "{core.SESSION_LOG}"; true', timeout=60,
                     check=False).splitlines()
                 if "COLLAB-JOIN" in line and slowest["app"] in line]
        if joins:
            problems.append(f"the broker's joins for {slowest['app']}: {'; '.join(joins[-3:])}")
    if "so.tilo.notes" not in apps:
        problems.append("so.tilo.notes logged no APP-STARTUP line")
    if problems:
        raise CheckFailed("; ".join(problems))
    res.detail = f"{len(startups)} launches of {len(apps)} apps within budget"


# The compositor under the slot's session wrapper: its child named `shell`.
COMPOSITOR_PID = ("w=$(systemctl show -p MainPID --value {unit}); for d in /proc/[0-9]*; do "
                  "[ \"$(awk '/^PPid:/ {{print $2}}' \"$d/status\" 2>/dev/null)\" = \"$w\" ] && "
                  "[ \"$(cat \"$d/comm\" 2>/dev/null)\" = shell ] && echo \"${{d#/proc/}}\"; done; true")
RECOVERY_WAIT_S = 60


def first_line_mentioning(text: str, words: tuple[str, ...]) -> str:
    """The first line of `text` holding any of `words`, else its first line."""
    lines = [line for line in text.splitlines() if line.strip()]
    return next((line for line in lines if any(word in line for word in words)), lines[0] if lines else "")


@check("A17", "apps", "After the desktop session ends abruptly, Files opens on the first request")
def files_after_session_crash(ctx, res):
    """The slot's desktop runs as `tilo-vm-session@<slot>`; when its compositor dies,
    `Restart=` brings up a fresh session. After such a restart in A15 every later Files open
    was refused ("filesystem authority changed while the application launch was pending")
    until the session ended again. That crash came while an app was opening: open and close
    Files once, ask for it again and end the compositor the way a crash does (SIGSEGV, from
    root) while that launch is under way, wait for the next session to answer and close what
    it restored, then open Files twice: both must draw their first frame."""
    ctx.reset()
    if _timed_open(ctx, FILES, label="Files before the session ended") is None:
        raise Unobservable("Files drew no first frame before the session was ended, so whether it "
                           "reopens after the restart says nothing")
    unit = session_unit.unit(ctx.vm.slot)
    pids = ctx.vm.root_exec(COMPOSITOR_PID.format(unit=shlex.quote(unit))).split()
    if len(pids) != 1 or not pids[0].isdigit():
        raise Unobservable(f"no single compositor process under {unit} to end: {pids}")
    ctx.vm.exec(f"tilo-control actions invoke applications {shlex.quote(FILES)} >/dev/null")
    wrapper = (session_unit.show(ctx) or {}).get("MainPID", "")
    # Rust's own SIGSEGV handler returns from a signal no fault raised, resetting the default
    # action: the second one ends the compositor as a fault does.
    ctx.vm.root_exec(f"kill -SEGV {pids[0]}; sleep 0.5; kill -SEGV {pids[0]} 2>/dev/null; true")

    def answering():
        try:
            fields = session_unit.show(ctx)
            moved = fields and fields.get("MainPID", "0") not in ("0", "", wrapper)
            return ctx.vm.state() if moved else None
        except VmError:
            return None

    if wait_until(answering, RECOVERY_WAIT_S, 2.0) is None:
        raise CheckFailed(f"no new session answered within {RECOVERY_WAIT_S} s of the compositor's SIGSEGV "
                          f"(session main process before: {wrapper or '?'})")
    core.adopt_session(ctx)
    res.metrics["ended_compositor_pid"] = int(pids[0])
    ctx.reset()
    missed = [k for k in (1, 2) if _timed_open(ctx, FILES, label=f"Files open #{k} after the restart") is None]
    res.metrics["opens_without_first_frame"] = missed
    if missed:
        said = first_line_mentioning(ctx.facts.get("last_no_first_frame", ""), ("ERROR", "refused", "WARN"))
        raise CheckFailed(f"after the session restarted, Files open #{', #'.join(map(str, missed))} of 2 drew no "
                          f"first frame within {OPEN_WAIT_S} s; the session said: {said[:400]}")
    res.detail = "Files opened twice after the session restarted"

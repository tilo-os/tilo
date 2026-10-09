"""Essential 5: idle CPU, wakeups, frames and RAM; memory per app cgroup; frame work
during interactions; boot to a usable greeter; the render-path gate. Software
rendering in a VM skews GPU-bound numbers, so every result carries the renderer it
was measured on, and so does every performance record (acceptance/records.py)."""

from __future__ import annotations

import json
import re
import time

from .. import core
from ..core import check, expect

AREA = "resources"

# One sh script, so ssh latency never lands inside the measured window.
SAMPLE = r'''
uid=__UID__; hz=$(getconf CLK_TCK)
snap() {
  for d in /proc/[0-9]*; do
    [ "$(stat -c %u $d 2>/dev/null)" = "$uid" ] || continue
    s=$(cat $d/stat 2>/dev/null) || continue
    comm=$(tr " " _ < $d/comm 2>/dev/null)
    set -- $(echo "$s" | sed 's/^.*) //')
    cs=$(awk '/^voluntary_ctxt_switches|^nonvoluntary_ctxt_switches/ {n+=$2} END {print n+0}' $d/status 2>/dev/null)
    echo "P ${d#/proc/} $comm $(( ${12} + ${13} )) $cs"
  done
  awk '/^cpu / {t=0; for (i=2;i<=NF;i++) t+=$i; print "T", t, $5}' /proc/stat
}
echo "@@0"; snap; sleep __SECS__; echo "@@1"; snap; echo "HZ $hz"
'''


def _parse_sample(text):
    snaps, cur, hz = [{}, {}], None, 100
    totals = [None, None]
    for line in text.splitlines():
        if line == "@@0":
            cur = 0
        elif line == "@@1":
            cur = 1
        elif line.startswith("P ") and cur is not None:
            _, pid, comm, ticks, cs = line.split()[:5]
            snaps[cur][int(pid)] = (comm, int(ticks), int(cs))
        elif line.startswith("T ") and cur is not None:
            _, total, idle = line.split()
            totals[cur] = (int(total), int(idle))
        elif line.startswith("HZ "):
            hz = int(line.split()[1])
    return snaps, totals, hz


def measure(ctx, seconds: int) -> dict:
    script = SAMPLE.replace("__SECS__", str(seconds)).replace("__UID__", str(ctx.vm.slot_uid()))
    mark = core.trace_mark(ctx)
    text = ctx.vm.root_exec(script, timeout=seconds + 90)
    rows = core.trace_after(ctx, mark)
    (a, b), totals, hz = _parse_sample(text)
    procs = []
    for pid, (comm, ticks, cs) in b.items():
        if pid in a:
            procs.append({"pid": pid, "comm": comm,
                          "cpu_pct": round((ticks - a[pid][1]) / hz / seconds * 100, 2),
                          "ctxt_switches_per_s": round((cs - a[pid][2]) / seconds, 1)})
    procs.sort(key=lambda p: -p["cpu_pct"])
    shell = next((p for p in procs if p["comm"] == "shell"), None)
    guest = None
    if all(totals):
        dt, di = totals[1][0] - totals[0][0], totals[1][1] - totals[0][1]
        guest = round((1 - di / dt) * 100, 2) if dt else None
    painted = [r for r in rows if r.get("painted") == 1]
    return {
        "seconds": seconds,
        "shell_cpu_pct": shell and shell["cpu_pct"],
        "shell_wakeups_per_s": shell and shell["ctxt_switches_per_s"],
        "shell_loop_ticks_per_s": round(len(rows) / seconds, 2),
        "frames_painted": len(painted),
        "user_cpu_pct_total": round(sum(p["cpu_pct"] for p in procs), 2),
        "guest_cpu_pct": guest,
        "top": procs[:10],
    }


def memory(ctx, apps: list[str]) -> dict:
    """Per app: the memory its own cgroup is charged (memory.current, split into anon and
    file by memory.stat), the number Tilo budgets an app by. Per process: anonymous RSS
    of the slot user's processes, as evidence of which process holds it. Session services
    (portals, PipeWire, indexers) are in no app's cgroup and count for no app."""
    table = core.processes(ctx)
    uid = ctx.vm.slot_uid()
    procs = sorted((p for p in table["procs"].values() if p["uid"] == uid),
                   key=lambda p: -(p["anon_rss_mb"] or 0))
    shell = next((p for p in procs if p["comm"] == "shell"), None)
    per_app = {}
    for app in apps:
        a = core.app_cgroup(table, app)
        own = [table["cgroups"].get(c, {}) for c in a["cgroups"]] if not a["shared_with"] else []
        if own and all(c.get("memory_mb") is not None for c in own):
            a.update({k: round(sum(c[k] for c in own), 1) for k in ("memory_mb", "anon_mb", "file_mb")})
        per_app[app] = a
    session_cgroups = {shell["cgroup"]} if shell else set()
    session_cgroups |= {c for a in per_app.values() if not a["shared_with"] for c in a["cgroups"]}
    charged = [table["cgroups"].get(c, {}).get("memory_mb") for c in session_cgroups]
    return {
        "apps": per_app,
        "procs": [{k: p[k] for k in ("pid", "comm", "app", "cgroup", "rss_mb", "anon_rss_mb")}
                  for p in procs],
        "shell": shell and {"pid": shell["pid"], "cgroup": shell["cgroup"],
                            "anon_rss_mb": shell["anon_rss_mb"],
                            **table["cgroups"].get(shell["cgroup"], {})},
        "session_memory_mb": round(sum(charged), 1) if charged and None not in charged else None,
        "session_cgroups": sorted(session_cgroups),
        "guest_mem_mb": table["meminfo_mb"],
    }


def guest_ram_used_mib(ctx) -> int:
    """The guest's memory in use, MemTotal less MemAvailable: what the running system
    holds and could not hand back without swapping. The dev VM's whole system: its
    seat0 greeter, tilo-vmd and the slot's session."""
    out = ctx.vm.exec("awk '/^MemTotal:|^MemAvailable:/ {print $1, $2}' /proc/meminfo", check=False)
    kib = {line.split()[0].rstrip(":"): int(line.split()[1])
           for line in out.splitlines() if len(line.split()) == 2 and line.split()[1].isdigit()}
    if "MemTotal" not in kib or "MemAvailable" not in kib:
        raise core.Unobservable(f"/proc/meminfo did not name MemTotal and MemAvailable: {out[-200:]!r}")
    return (kib["MemTotal"] - kib["MemAvailable"]) // 1024


def _record_idle(ctx, m: dict, ram_mib: int, variant: str = "") -> None:
    suffix = f".{variant}" if variant else ""
    window = {"window_s": m["seconds"]}
    ctx.record(f"idle.shell_cpu_pct{suffix}", "percent", [m["shell_cpu_pct"]], **window)
    ctx.record(f"idle.session_cpu_pct{suffix}", "percent", [m["user_cpu_pct_total"]], **window)
    ctx.record(f"idle.ram_mib{suffix}", "MiB", [ram_mib], what="guest MemTotal - MemAvailable")
    if not variant:
        # Context switches of the compositor's main thread, its loop: each is a wakeup or
        # a preemption.
        ctx.record("idle.wakeups_per_s.shell", "per_s", [m["shell_wakeups_per_s"]], **window)
        ctx.record("idle.loop_ticks_per_s", "per_s", [m["shell_loop_ticks_per_s"]], **window)
        ctx.record("idle.frames", "count", [m["frames_painted"]], **window)


def _open_several(ctx):
    """Open the four apps; return the ones still open 8 s later.

    An app whose window closed on its own in that time is recorded in
    facts["apps_closed_after_open"], with what the session log and the user
    journal said about it, rather than measured as if it were running."""
    ctx.reset()
    groups = {}
    for app in ("so.tilo.terminal", "so.tilo.file-explorer", "so.tilo.settings", "so.tilo.notes"):
        try:
            group, _ = ctx.open_app(app)
            groups[app] = group["id"]
        except core.CheckFailed:
            pass
    time.sleep(8)
    st = ctx.state()
    closed = [app for app, gid in groups.items() if core.group_by_id(st, gid) is None]
    if closed:
        ctx.shot("R-apps-closed-after-open")
        pattern = "|".join(a.replace(".", "\\.") for a in closed)
        # The compositor's own lines and the apps' ends, not their start-up chatter.
        ctx.write("R-apps-closed-after-open.log", ctx.vm.exec(
            "sed 's/\\x1b\\[[0-9;]*m//g' \"$HOME/.local/state/tilo/session-logs/latest.log\" 2>/dev/null "
            f"| grep -avE 'tilo-sandbox-exec|MESA|service socket connected|action contract validation' "
            f"| grep -aE '^[^\\[]|{pattern}' | grep -aiE '{pattern}|exit|signal|panic|close|kill|fail|launch|surface' "
            "| tail -n 150 | cut -c1-400; echo ---; "
            f"journalctl --user -b --no-pager -o short-monotonic 2>/dev/null "
            f"| grep -aE 'scope|{pattern}' | grep -aiE 'stop|deactivat|fail|exit|kill|signal|consumed|dump' "
            "| tail -n 60 | cut -c1-400; echo ---; coredumpctl --no-pager list 2>/dev/null | tail -n 10; true",
            check=False))
    ctx.facts["apps_closed_after_open"] = closed
    return [app for app in groups if app not in closed]


def _budget_problems(m, budgets, prefix):
    problems = []
    if m["shell_cpu_pct"] is None:
        raise core.Unobservable("no `shell` process of the slot user found")
    if m["shell_cpu_pct"] > budgets[f"{prefix}_shell_cpu_pct"]:
        problems.append(f"compositor CPU {m['shell_cpu_pct']}% > {budgets[f'{prefix}_shell_cpu_pct']}%")
    if m["frames_painted"] > budgets[f"{prefix}_frames"]:
        problems.append(f"{m['frames_painted']} frames painted while idle > {budgets[f'{prefix}_frames']}")
    if m["shell_loop_ticks_per_s"] > budgets["idle_loop_ticks_per_s"]:
        problems.append(f"compositor loop woke {m['shell_loop_ticks_per_s']}/s > {budgets['idle_loop_ticks_per_s']}/s")
    if m["user_cpu_pct_total"] > budgets[f"{prefix}_user_cpu_pct"]:
        top = ", ".join(f"{p['comm']} {p['cpu_pct']}%" for p in m["top"][:4])
        problems.append(f"session CPU {m['user_cpu_pct_total']}% > {budgets[f'{prefix}_user_cpu_pct']}% ({top})")
    return problems


@check("R01", AREA, "Idle with no apps: compositor asleep, no frames, little CPU")
def idle_empty(ctx, res):
    ctx.reset()
    time.sleep(5)
    m = measure(ctx, ctx.config["idle_seconds"])
    m["guest_ram_used_mib"] = guest_ram_used_mib(ctx)
    res.metrics.update(m, renderer=ctx.facts.get("renderer"))
    ctx.write("R01-idle-empty.json", json.dumps(m, indent=1))
    if m["shell_cpu_pct"] is not None:
        _record_idle(ctx, m, m["guest_ram_used_mib"])
    problems = _budget_problems(m, ctx.config["budgets"], "idle")
    expect(not problems, "; ".join(problems))


@check("R02", AREA, "Idle with Terminal, Files, Settings and Notes open")
def idle_apps(ctx, res):
    opened = _open_several(ctx)
    res.metrics["opened"] = opened
    m = measure(ctx, ctx.config["idle_seconds"])
    m["guest_ram_used_mib"] = guest_ram_used_mib(ctx)
    res.metrics.update(m, renderer=ctx.facts.get("renderer"))
    ctx.write("R02-idle-apps.json", json.dumps(m, indent=1))
    if m["shell_cpu_pct"] is not None and len(opened) == 4:
        _record_idle(ctx, m, m["guest_ram_used_mib"], "four_apps")
    ctx.facts["apps_open_for_memory"] = opened
    problems = _budget_problems(m, ctx.config["budgets"], "idle_apps")
    closed = ctx.facts.get("apps_closed_after_open") or []
    if closed:
        problems.append(f"{', '.join(closed)} closed on their own within 8 s of opening "
                        "(see R-apps-closed-after-open.log)")
    if len(opened) + len(closed) < 4:
        problems.append(f"only {len(opened) + len(closed)} of 4 apps opened ({opened + closed})")
    expect(not problems, "; ".join(problems))


def _record_memory(ctx, m: dict) -> None:
    if m["shell"]:
        ctx.record("memory.shell_anon_mib", "MiB", [m["shell"]["anon_rss_mb"]])
    ctx.record("memory.session_mib", "MiB", [m["session_memory_mb"]],
               what="the compositor's and the open apps' cgroups")
    for app, a in m["apps"].items():
        if a["shared_with"] or a.get("memory_mb") is None:
            continue
        short = app.rsplit(".", 1)[-1]
        ctx.record(f"memory.app_mib.{short}", "MiB", [a["memory_mb"]], app=app)
        ctx.record(f"memory.app_anon_mib.{short}", "MiB", [a["anon_mb"]], app=app)


@check("R03", AREA, "Memory: each open app's cgroup and the compositor stay within budget")
def mem(ctx, res):
    opened = ctx.facts.get("apps_open_for_memory") or _open_several(ctx)
    m = memory(ctx, opened)
    res.metrics.update(apps=m["apps"], shell=m["shell"], session_memory_mb=m["session_memory_mb"],
                       top=m["procs"][:15], guest_mem_mb=m["guest_mem_mb"])
    ctx.write("R03-memory.json", json.dumps(m, indent=1))
    _record_memory(ctx, m)
    b = ctx.config["budgets"]
    expect(m["shell"], "no compositor process found")
    problems, unobserved = [], []
    if m["shell"]["anon_rss_mb"] > b["shell_anon_mb"]:
        problems.append(f"compositor anonymous RSS {m['shell']['anon_rss_mb']} MB > {b['shell_anon_mb']} MB")
    big = [p for p in m["procs"] if p["comm"] != "shell" and (p["anon_rss_mb"] or 0) > b["process_anon_mb"]]
    if big:
        problems.append("over the per-process anonymous RSS budget: " +
                        ", ".join(f"{p['comm']} ({p['app'] or 'session'}) {p['anon_rss_mb']} MB"
                                  for p in big[:5]))
    for app, a in m["apps"].items():
        if not a["pids"]:
            unobserved.append(f"{app}: no `tilo-launcher --app-id {app}` process tree")
        elif a["shared_with"]:
            where = "; ".join(f"{c} with {', '.join(v[:4])}" for c, v in a["shared_with"].items())
            problems.append(f"{app} has no cgroup of its own, so its memory cannot be told apart "
                            f"(runs in {where}; its processes hold {a['anon_rss_mb']} MB anonymous RSS)")
        elif a.get("memory_mb") is None:
            unobserved.append(f"{app}: no memory controller on {', '.join(a['cgroups'])}")
        else:
            if a["memory_mb"] > b["app_memory_mb"]:
                problems.append(f"{app} cgroup memory {a['memory_mb']} MB > {b['app_memory_mb']} MB "
                                f"(anon {a['anon_mb']}, file {a['file_mb']})")
            if a["anon_mb"] > b["app_anon_mb"]:
                problems.append(f"{app} cgroup anon {a['anon_mb']} MB > {b['app_anon_mb']} MB")
    if m["session_memory_mb"] is not None and m["session_memory_mb"] > b["session_memory_mb"]:
        problems.append(f"compositor and app cgroups {m['session_memory_mb']} MB > {b['session_memory_mb']} MB")
    closed = ctx.facts.get("apps_closed_after_open") or []
    if closed:
        problems.append(f"{', '.join(closed)} closed on their own within 8 s of opening, so their "
                        "memory was not measured (see R-apps-closed-after-open.log)")
    expect(not problems, "; ".join(problems + unobserved))
    if unobserved:
        raise core.Unobservable("; ".join(unobserved))


@check("R04", AREA, "Frame CPU p50/p95/p99 during overview, focus and workspace interactions")
def frame_work(ctx, res):
    ctx.reset()
    for _ in range(2):
        ctx.new_terminal_window()
    time.sleep(2)
    mark = core.trace_mark(ctx)
    for _ in range(3):
        ctx.vm.key("super+Tab")
        time.sleep(0.8)
        ctx.vm.key("Escape")
        time.sleep(0.8)
    for _ in range(3):
        ctx.vm.key("super+Left")
        time.sleep(0.5)
        ctx.vm.key("super+Right")
        time.sleep(0.5)
    ctx.vm.key("super+equal")
    time.sleep(0.6)
    ctx.vm.key("super+minus")
    time.sleep(1.0)
    rows = core.trace_after(ctx, mark)
    st = core.frame_stats(rows)
    display = ctx.facts.get("display") or {}
    res.metrics.update(st, renderer=ctx.facts.get("renderer"), display=display)
    ctx.write("R04-frames.csv", core.trace_csv(rows))
    ctx.record("render.frame_cpu_ms", "ms",
               [r["cpu_us"] / 1000 for r in rows if r.get("painted") == 1 and isinstance(r.get("cpu_us"), int)],
               interactions="overview, focus, zoom")
    b = ctx.config["budgets"]
    expect(st["painted"] > 0, "no frame painted during the interactions")
    if not core.has_cpu(rows):
        raise core.Unobservable("the frame trace carries no per-tick CPU time (cpu_us), so the "
                                "compositor's own work cannot be told from time the thread was "
                                "descheduled or blocked; the installed compositor predates it")
    # CPU per painted frame is the compositor's alone. Wall time per frame also holds
    # time the thread was descheduled or blocked: judged only on real hardware, where
    # that is the compositor's to answer for, and kept as evidence elsewhere.
    problems = []
    for pct in ("p95", "p99"):
        if st[f"cpu_ms_{pct}"] > b[f"work_ms_{pct}"]:
            problems.append(f"frame CPU {pct} {st[f'cpu_ms_{pct}']} ms > {b[f'work_ms_{pct}']} ms")
        if display.get("hardware") and st[f"work_ms_{pct}"] > b[f"work_ms_{pct}"]:
            problems.append(f"frame wall time {pct} {st[f'work_ms_{pct}']} ms > {b[f'work_ms_{pct}']} ms")
    expect(not problems, "; ".join(problems) + f" [renderer: {ctx.facts.get('renderer')}]")


# -- the installed-desktop gates of build-image.yml, on this run's VM -------------

BOOT_GATE = "/usr/libexec/tilo/boot-performance-gate.sh"
BOOT_TOTAL = re.compile(r"kernel\+userspace reached usable greeter in (\d+)\.(\d{3})s")
BOOT_PHASES = re.compile(r"phases: kernel ([0-9.]+)s, initrd ([0-9.]+)s, userspace to greeter ([0-9.]+)s")
GATE_EXIT = re.compile(r"^GATE-EXIT=(\d+)$", re.M)


def parse_boot_gate(text: str) -> dict:
    """The boot gate's output: its exit status, kernel+userspace seconds to a usable
    greeter and, when it printed them, the kernel, initrd and userspace phases."""
    exit_ = GATE_EXIT.search(text)
    total = BOOT_TOTAL.search(text)
    phases = BOOT_PHASES.search(text)
    out = {"exit": int(exit_.group(1)) if exit_ else None,
           "to_greeter_s": int(total.group(1)) + int(total.group(2)) / 1000 if total else None}
    if phases:
        out.update(kernel_s=float(phases.group(1)), initrd_s=float(phases.group(2)),
                   userspace_s=float(phases.group(3)))
    return out


@check("R06", AREA, "The VM booted kernel and userspace to a usable greeter within budget")
def boot(ctx, res):
    """The release gate's boot check (os/tests/boot_performance_gate.sh, shipped in the
    image): graphical.target and greetd's login endpoint up, no device timeout, the
    image's boot contract, all within the budget. It reads this boot's monotonic
    timestamps, so it may run at any point of the run."""
    budget = ctx.config["budgets"]["boot_to_greeter_s"]
    text = ctx.vm.root_exec(f"TILO_BOOT_USERSPACE_BUDGET_SEC={budget} {BOOT_GATE} 2>&1; echo GATE-EXIT=$?",
                            timeout=180)
    ctx.write("R06-boot-gate.txt", text)
    gate = parse_boot_gate(text)
    res.metrics.update({k: v for k, v in gate.items() if k != "exit"}, budget_s=budget)
    said = " | ".join(l.strip() for l in text.splitlines()[-8:] if l.strip() and "GATE-EXIT" not in l)
    if gate["to_greeter_s"] is None:
        raise core.Unobservable(f"the boot gate named no time to the greeter (exit {gate['exit']}): {said}")
    ctx.record("boot.to_greeter_s", "s", [gate["to_greeter_s"]], what="kernel+userspace, firmware excluded")
    for phase in ("kernel", "initrd", "userspace"):
        ctx.record(f"boot.{phase}_s", "s", [gate.get(f"{phase}_s")])
    expect(gate["exit"] == 0, f"the boot gate failed: {said}")


RENDER_REPORT = "/usr/libexec/tilo-session-report"
RENDER_UNOBSERVABLE = ("no session flight record", "the compositor logged no DRM render phase")


@check("R07", AREA, "The session presented within the render-path budgets (tilo-session-report --render-gate)")
def render_gate(ctx, res):
    """The release gate's render check on this run's session: direct GPU scanout where
    the renderer shares the scanout GPU, and every DRM render phase and the painted
    frames' main-thread work within the budgets build-image.yml holds a KVM guest to."""
    b = ctx.config["budgets"]
    text = ctx.vm.exec(f"{RENDER_REPORT} --render-gate --render-budget-ms {b['render_frame_ms']} "
                       f"--frame-work-budget-ms {b['render_frame_work_ms']} 2>&1; echo GATE-EXIT=$?",
                       timeout=120, check=False)
    ctx.write("R07-render-gate.txt", text)
    exit_ = GATE_EXIT.search(text)
    failures = [l.split("render gate: FAIL ", 1)[1] for l in text.splitlines() if "render gate: FAIL " in l]
    res.metrics.update(failures=failures, renderer=ctx.facts.get("renderer"))
    if exit_ is None or (exit_.group(1) != "0" and not failures):
        raise core.Unobservable(f"{RENDER_REPORT} did not judge the session: {text.strip()[-300:]}")
    unobserved = [f for f in failures if f.startswith(RENDER_UNOBSERVABLE)]
    if unobserved:
        raise core.Unobservable("; ".join(unobserved))
    expect(not failures, "; ".join(failures) + f" [renderer: {ctx.facts.get('renderer')}]")

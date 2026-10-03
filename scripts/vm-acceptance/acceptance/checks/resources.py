"""Essential 5: idle CPU, wakeups and frames; memory per app cgroup; frame work
during interactions. Software rendering in a VM skews GPU-bound numbers, so
every result carries the renderer it was measured on."""

from __future__ import annotations

import json
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
    t0 = core.trace_now_us(ctx)
    script = SAMPLE.replace("__SECS__", str(seconds)).replace("__UID__", str(ctx.vm.slot_uid()))
    text = ctx.vm.root_exec(script, timeout=seconds + 90)
    rows = core.trace_since(core.read_trace(ctx), t0 + 1)
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


def _open_several(ctx):
    ctx.reset()
    opened = []
    for app in ("so.tilo.terminal", "so.tilo.file-explorer", "so.tilo.settings", "so.tilo.notes"):
        try:
            ctx.open_app(app)
            opened.append(app)
        except core.CheckFailed:
            pass
    time.sleep(8)
    return opened


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
    res.metrics.update(m, renderer=ctx.facts.get("renderer"))
    ctx.write("R01-idle-empty.json", json.dumps(m, indent=1))
    problems = _budget_problems(m, ctx.config["budgets"], "idle")
    expect(not problems, "; ".join(problems))


@check("R02", AREA, "Idle with Terminal, Files, Settings and Notes open")
def idle_apps(ctx, res):
    opened = _open_several(ctx)
    res.metrics["opened"] = opened
    m = measure(ctx, ctx.config["idle_seconds"])
    res.metrics.update(m, renderer=ctx.facts.get("renderer"))
    ctx.write("R02-idle-apps.json", json.dumps(m, indent=1))
    ctx.facts["apps_open_for_memory"] = opened
    problems = _budget_problems(m, ctx.config["budgets"], "idle_apps")
    if len(opened) < 4:
        problems.append(f"only {len(opened)} of 4 apps opened ({opened})")
    expect(not problems, "; ".join(problems))


@check("R03", AREA, "Memory: each open app's cgroup and the compositor stay within budget")
def mem(ctx, res):
    opened = ctx.facts.get("apps_open_for_memory") or _open_several(ctx)
    m = memory(ctx, opened)
    res.metrics.update(apps=m["apps"], shell=m["shell"], session_memory_mb=m["session_memory_mb"],
                       top=m["procs"][:15], guest_mem_mb=m["guest_mem_mb"])
    ctx.write("R03-memory.json", json.dumps(m, indent=1))
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
    expect(not problems, "; ".join(problems + unobserved))
    if unobserved:
        raise core.Unobservable("; ".join(unobserved))


@check("R04", AREA, "Frame work p50/p95/p99 during overview, focus and workspace interactions")
def frame_work(ctx, res):
    ctx.reset()
    for _ in range(2):
        ctx.new_terminal_window()
    time.sleep(2)
    t0 = core.trace_now_us(ctx)
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
    rows = core.trace_since(core.read_trace(ctx), t0 + 1)
    st = core.frame_stats(rows)
    res.metrics.update(st, renderer=ctx.facts.get("renderer"))
    ctx.write("R04-frames.csv", "\n".join(",".join(str(r[k]) for k in r) for r in rows))
    b = ctx.config["budgets"]
    expect(st["painted"] > 0, "no frame painted during the interactions")
    problems = []
    if st["work_ms_p95"] > b["work_ms_p95"]:
        problems.append(f"frame work p95 {st['work_ms_p95']} ms > {b['work_ms_p95']} ms")
    if st["work_ms_p99"] > b["work_ms_p99"]:
        problems.append(f"frame work p99 {st['work_ms_p99']} ms > {b['work_ms_p99']} ms")
    expect(not problems, "; ".join(problems) + f" [renderer: {ctx.facts.get('renderer')}]")

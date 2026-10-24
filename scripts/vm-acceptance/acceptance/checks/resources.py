"""Essential 5: idle CPU, wakeups, frames and RAM; memory per app cgroup; frame work
during interactions; boot to a usable greeter; greeter sign-in to a usable desktop; the
render-path gate. Software rendering in a VM skews GPU-bound numbers, so every result
carries the renderer it was measured on, and so does every performance record
(acceptance/records.py)."""

from __future__ import annotations

import base64
import json
import re
import shlex
import sys
import time
from pathlib import Path

from .. import core
from ..core import check, expect
from ..vm import VmError, first_json, wait_until

AREA = "resources"

# One sh script, so ssh latency never lands inside the measured window. Per process:
# its CPU ticks, its main thread's context switches, its parent and its start time.
# Per thread of every process, one awk per process (a few dozen forks per snapshot,
# not one per thread): the thread's name, CPU ticks and context switches.
SAMPLE = r"""
uid=__UID__; hz=$(getconf CLK_TCK)
threads() {
  awk -v pid="$1" 'BEGIN {
    dir = "/proc/" pid "/task"; ls = "ls " dir " 2>/dev/null"
    while ((ls | getline tid) > 0) {
      base = dir "/" tid
      ok = (getline st < (base "/stat")) > 0; close(base "/stat")
      if (!ok) continue
      comm = ""; getline comm < (base "/comm"); close(base "/comm")
      gsub(/[ \t]/, "_", comm); if (comm == "") comm = "?"
      sub(/^.*\) /, "", st); split(st, f, " ")
      cs = 0
      while ((getline line < (base "/status")) > 0)
        if (line ~ /^(non)?voluntary_ctxt_switches:/) { split(line, g, /[ \t]+/); cs += g[2] }
      close(base "/status")
      print "H", pid, tid, comm, f[12] + f[13], cs
    }
    close(ls)
  }'
}
snap() {
  echo "U $(cut -d' ' -f1 /proc/uptime)"
  for d in /proc/[0-9]*; do
    [ "$(stat -c %u $d 2>/dev/null)" = "$uid" ] || continue
    s=$(cat $d/stat 2>/dev/null) || continue
    comm=$(tr " " _ < $d/comm 2>/dev/null)
    set -- $(echo "$s" | sed 's/^.*) //')
    cs=$(awk '/^voluntary_ctxt_switches|^nonvoluntary_ctxt_switches/ {n+=$2} END {print n+0}' $d/status 2>/dev/null)
    echo "P ${d#/proc/} $comm $(( ${12} + ${13} )) $cs $2 ${20}"
    threads "${d#/proc/}"
  done
  awk '/^cpu / {t=0; for (i=2;i<=NF;i++) t+=$i; print "T", t, $5}' /proc/stat
}
echo "@@0"; snap; sleep __SECS__; echo "@@1"; snap; echo "HZ $hz"
"""


def _parse_sample(text: str) -> dict:
    """`SAMPLE` output -> both snapshots: per process {pid: {comm, ticks, cs, ppid, start}},
    per thread {(pid, tid): (comm, ticks, cs)}, the guest's CPU totals and uptime, and HZ."""
    out = {"procs": [{}, {}], "threads": [{}, {}], "totals": [None, None],
           "uptime": [None, None], "hz": 100}
    cur = None
    for line in text.splitlines():
        f = line.split()
        if line in ("@@0", "@@1"):
            cur = int(line[2])
        elif line.startswith("HZ "):
            out["hz"] = int(f[1])
        elif cur is None or not f:
            continue
        elif f[0] == "P" and len(f) >= 5:
            out["procs"][cur][int(f[1])] = {
                "comm": f[2], "ticks": int(f[3]), "cs": int(f[4]),
                "ppid": int(f[5]) if len(f) > 5 else None,
                "start": int(f[6]) if len(f) > 6 else None}
        elif f[0] == "H" and len(f) == 6:
            out["threads"][cur][(int(f[1]), int(f[2]))] = (f[3], int(f[4]), int(f[5]))
        elif f[0] == "T" and len(f) == 3:
            out["totals"][cur] = (int(f[1]), int(f[2]))
        elif f[0] == "U" and len(f) == 2:
            out["uptime"][cur] = float(f[1])
    return out


def compositor_pid(procs: dict) -> int | None:
    """The compositor among the slot user's processes: the `shell` with no `shell` above it.

    Every other process named `shell` was started by the compositor from its own binary
    (the public shader-seed renderer, `--shader-seed-render`), so it has the compositor
    as an ancestor; the busiest `shell` could be such a child. Two roots would be two
    compositors, and which of them is the idle desktop cannot be said."""
    def has_shell_ancestor(pid):
        seen = {pid}
        pid = procs[pid]["ppid"]
        while pid in procs and pid not in seen:
            if procs[pid]["comm"] == "shell":
                return True
            seen.add(pid)
            pid = procs[pid]["ppid"]
        return False

    roots = sorted(pid for pid, p in procs.items() if p["comm"] == "shell" and not has_shell_ancestor(pid))
    if len(roots) > 1:
        raise core.Unobservable(f"{len(roots)} compositor processes of the slot user (pids {roots})")
    return roots[0] if roots else None


def thread_rows(sample: dict, pid: int, seconds: float) -> dict:
    """CPU and context switches per thread of `pid` over the window, busiest first.

    A thread started inside the window counts from zero. The CPU of threads that ended
    inside it is in the process's total but in no thread: `exited_cpu_pct`."""
    (a, b), hz = sample["threads"], sample["hz"]
    rows = []
    for (owner, tid), (comm, ticks, cs) in b.items():
        if owner != pid:
            continue
        _, t0, c0 = a.get((owner, tid), (comm, 0, 0))
        rows.append({"tid": tid, "comm": comm,
                     "cpu_pct": round((ticks - t0) / hz / seconds * 100, 2),
                     "ctxt_switches_per_s": round((cs - c0) / seconds, 2),
                     "new": (owner, tid) not in a})
    rows.sort(key=lambda r: (-r["cpu_pct"], -r["ctxt_switches_per_s"]))
    procs = sample["procs"]
    proc_cpu = ((procs[1][pid]["ticks"] - procs[0][pid]["ticks"]) / hz / seconds * 100
                if pid in procs[0] and pid in procs[1] else None)
    exited = None if proc_cpu is None or not rows else max(proc_cpu - sum(r["cpu_pct"] for r in rows), 0.0)
    return {"threads": rows,
            "wakeups_per_s": round(sum(r["ctxt_switches_per_s"] for r in rows), 1),
            "exited_cpu_pct": None if exited is None else round(exited, 2)}


def thread_groups(rows: list[dict]) -> list[dict]:
    """Threads summed by name less a trailing index, so a pool (`llvmpipe-0`…`llvmpipe-5`,
    `tilo-session-rt-1`) reads as one line; busiest first."""
    groups: dict[str, dict] = {}
    for r in rows:
        name = re.sub(r"[-_:#.]?\d+$", "", r["comm"]) or r["comm"]
        g = groups.setdefault(name, {"name": name, "threads": 0, "cpu_pct": 0.0,
                                     "ctxt_switches_per_s": 0.0})
        g["threads"] += 1
        g["cpu_pct"] = round(g["cpu_pct"] + r["cpu_pct"], 2)
        g["ctxt_switches_per_s"] = round(g["ctxt_switches_per_s"] + r["ctxt_switches_per_s"], 2)
    return sorted(groups.values(), key=lambda g: (-g["cpu_pct"], -g["ctxt_switches_per_s"]))


# The compositor's threads that its GPU driver started. The compositor names the thread that
# opens the device `tilo-gpu-driver` while it does, so threads the driver starts then take that
# name (shell present.rs DRIVER_THREAD_NAME, #4085); Mesa's CPU rasteriser names its rasteriser
# threads `llvmpipe-N` itself. On a GPU these threads hardly run. On lavapipe they are the
# rasteriser: its unnamed compute-shader pool runs every Vello stage of every frame.
GPU_DRIVER_GROUPS = ("llvmpipe", "tilo-gpu-driver")


def gpu_driver_cpu_pct(groups: list[dict]) -> float:
    """CPU of the compositor's GPU-driver thread groups (GPU_DRIVER_GROUPS)."""
    return round(sum(g["cpu_pct"] for g in groups if g["name"] in GPU_DRIVER_GROUPS), 2)


def describe_threads(groups: list[dict], n: int = 4) -> str:
    """The busiest thread groups for a failure message: `tilo-net 1.9%, llvmpipe×6 0.8%`."""
    return ", ".join(f"{g['name']}{'×' + str(g['threads']) if g['threads'] > 1 else ''} {g['cpu_pct']}%"
                     for g in groups[:n] if g["cpu_pct"] > 0)


def summarize(sample: dict, seconds: int) -> dict:
    """Per-process and per-thread CPU over the window, the compositor named by
    `compositor_pid` rather than by being the busiest `shell`."""
    (a, b), hz = sample["procs"], sample["hz"]
    procs = []
    for pid, p in b.items():
        if pid in a:
            procs.append({"pid": pid, "comm": p["comm"],
                          "cpu_pct": round((p["ticks"] - a[pid]["ticks"]) / hz / seconds * 100, 2),
                          "ctxt_switches_per_s": round((p["cs"] - a[pid]["cs"]) / seconds, 1)})
    procs.sort(key=lambda p: -p["cpu_pct"])
    pid = compositor_pid(a)
    shell = next((p for p in procs if p["pid"] == pid), None)
    threads = thread_rows(sample, pid, seconds) if shell else None
    groups = threads and thread_groups(threads["threads"])
    # The busiest other processes' threads too: a session service or an app's runtime
    # (Terminal's tilo-ush) is as much an idle cost as the compositor. The raw rows keep
    # each thread's index and whether it started inside the window, which tell a pool
    # created per operation (`iroh-blob-store-37`, new) from one that idles.
    for p in procs[:5]:
        if p is not shell and p["cpu_pct"] >= 0.5:
            rows = thread_rows(sample, p["pid"], seconds)
            p["threads"] = thread_groups(rows["threads"])[:8]
            p["thread_rows"] = rows["threads"][:16]
            p["exited_cpu_pct"] = rows["exited_cpu_pct"]
    start, uptime = (a.get(pid) or {}).get("start"), sample["uptime"][0]
    guest, totals = None, sample["totals"]
    if all(totals):
        dt, di = totals[1][0] - totals[0][0], totals[1][1] - totals[0][1]
        guest = round((1 - di / dt) * 100, 2) if dt else None
    return {
        "compositor_pid": pid,
        # How long the compositor had run when the window opened: startup work still
        # running by then is paid inside the idle window, and this says how likely that is.
        "session_age_s": round(uptime - start / hz, 1) if start is not None and uptime else None,
        "shell_cpu_pct": shell and shell["cpu_pct"],
        "shell_wakeups_per_s": shell and shell["ctxt_switches_per_s"],
        "shell_threads_wakeups_per_s": threads and threads["wakeups_per_s"],
        "shell_exited_threads_cpu_pct": threads and threads["exited_cpu_pct"],
        "shell_thread_groups": groups,
        "shell_gpu_driver_cpu_pct": None if groups is None else gpu_driver_cpu_pct(groups),
        "shell_threads": threads and threads["threads"],
        "user_cpu_pct_total": round(sum(p["cpu_pct"] for p in procs), 2),
        "guest_cpu_pct": guest,
        "top": procs[:10],
        "ush": runtime_idle(sample, procs, seconds, "tilo-ush"),
    }


def runtime_idle(sample: dict, procs: list[dict], seconds: float, comm: str) -> dict | None:
    """Every process named `comm` over the window, summed: CPU, context switches of
    all their threads, and each thread with its index and `new` flag. Terminal's shell
    runtime is two such processes (`tilo-ush serve`, and `tilo-ush run` per open
    session, which holds the guest's broker and egress stub). None when none ran."""
    mine = [p for p in procs if p["comm"] == comm]
    if not mine:
        return None
    rows = {p["pid"]: thread_rows(sample, p["pid"], seconds) for p in mine}
    return {
        "processes": len(mine),
        "cpu_pct": round(sum(p["cpu_pct"] for p in mine), 2),
        "wakeups_per_s": round(sum(r["wakeups_per_s"] for r in rows.values()), 1),
        "exited_cpu_pct": round(sum(r["exited_cpu_pct"] or 0.0 for r in rows.values()), 2),
        "thread_groups": thread_groups([t for r in rows.values() for t in r["threads"]]),
        "threads": {str(pid): r["threads"] for pid, r in rows.items()},
    }


def painters(painted: list[dict]) -> dict[str, int]:
    """Painted ticks by what made them paint: the compositor's paint reasons and, when
    client surfaces committed, the apps whose commits the frame carried, and when it
    animated, whose motion it was. A trace from a
    compositor that predates the columns counts them as unattributed."""
    out: dict[str, int] = {}
    for r in painted:
        if "paint_reasons" not in r:
            label = "unattributed"
        else:
            label = str(r.get("paint_reasons") or "") or "no reason"
            if r.get("paint_clients"):
                label += f" by {r['paint_clients']}"
            if r.get("paint_animations"):
                label += f" of {r['paint_animations']}"
        out[label] = out.get(label, 0) + 1
    return dict(sorted(out.items(), key=lambda kv: (-kv[1], kv[0])))


def describe_painters(by: dict[str, int]) -> str:
    return ", ".join(f"{label} {n}" for label, n in by.items())


def named_threads(sample: dict, names: tuple[str, ...], seconds: float) -> list[dict]:
    """Every thread called one of `names` in any of the slot user's processes over the
    window, summed per process and name: its CPU and context switches. A thread started
    inside the window counts from zero."""
    (a, b), hz = sample["threads"], sample["hz"]
    procs = sample["procs"][1]
    rows: dict[tuple[int, str], dict] = {}
    for (pid, tid), (comm, ticks, cs) in b.items():
        if comm not in names:
            continue
        _, t0, c0 = a.get((pid, tid), (comm, 0, 0))
        r = rows.setdefault((pid, comm), {"pid": pid, "process": (procs.get(pid) or {}).get("comm"),
                                          "thread": comm, "threads": 0, "cpu_pct": 0.0,
                                          "ctxt_switches_per_s": 0.0})
        r["threads"] += 1
        r["cpu_pct"] = round(r["cpu_pct"] + (ticks - t0) / hz / seconds * 100, 2)
        r["ctxt_switches_per_s"] = round(r["ctxt_switches_per_s"] + (cs - c0) / seconds, 2)
    return sorted(rows.values(), key=lambda r: (r["thread"], -r["ctxt_switches_per_s"], r["pid"]))


def measure(ctx, seconds: int, name: str | None = None, threads: tuple[str, ...] = ()) -> dict:
    """The idle window. With `name`, its frame trace is kept as `<name>-frames.csv`, so a
    window that painted says which tick painted, why, and for which app. With `threads`,
    `named_threads` of those names in every process of the slot user."""
    script = SAMPLE.replace("__SECS__", str(seconds)).replace("__UID__", str(ctx.vm.slot_uid()))
    mark = core.trace_mark(ctx)
    text = ctx.vm.root_exec(script, timeout=seconds + 90)
    rows = core.trace_after(ctx, mark)
    if name:
        ctx.write(f"{name}-frames.csv", core.trace_csv(rows))
    painted = [r for r in rows if r.get("painted") == 1]
    sample = _parse_sample(text)
    return {
        "seconds": seconds,
        **summarize(sample, seconds),
        "shell_loop_ticks_per_s": round(len(rows) / seconds, 2),
        "frames_painted": len(painted),
        "painted_by": painters(painted),
        **({"named_threads": named_threads(sample, threads, seconds)} if threads else {}),
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
    pid = compositor_pid({p["pid"]: p for p in procs})
    shell = next((p for p in procs if p["pid"] == pid), None)
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


# -- where the guest's memory is: per bucket of cgroups, per process ---------------

# The guest's kernel memory that no process's RSS holds: unreclaimable slab, stacks,
# page tables, vmalloc and per-CPU areas. SecPageTables (KVM's secondary page tables)
# is absent before Linux 6.0, so it alone may be missing.
GUEST_KERNEL = ("SUnreclaim", "KernelStack", "PageTables", "VmallocUsed", "Percpu")
GUEST_FIELDS = ("MemTotal", "MemAvailable", "AnonPages", "Shmem") + GUEST_KERNEL

# The buckets that exist only because this is a dev VM driven by a harness: seat0's
# greeter, which an installed machine ends at login but which runs beside the slot here,
# and the dev image's services and the harness's own ssh sessions.
VM_ONLY = ("greeter", "harness")
BUCKETS = ("compositor", "apps", "session_services", "system") + VM_ONLY
# Dev image only (os/Containerfile.dev): sshd, tilo-device-admin, tilo-vmd's units; and
# root's sessions, which are the harness's ssh commands (this dump among them).
HARNESS = re.compile(r"^/system\.slice/(sshd\.service|tilo-device-admin\.service|tilo-vm-[^/]*)(/|$)"
                     r"|^/user\.slice/user-0\.slice(/|$)")


def bucket_of(path: str, procs: list[dict], who: dict) -> str:
    """The bucket of the cgroup `path`, whose processes are `procs`. `who` names the
    compositor's cgroup, greetd's uid and the slot's uid; first match wins."""
    if path == who.get("compositor_cgroup"):
        return "compositor"
    if "/app-tilo-so." in path or any(p["app"] for p in procs):
        return "apps"
    within = path + "/"
    if who.get("greeter_uid") is not None and f"/user-{who['greeter_uid']}.slice/" in within:
        return "greeter"
    if HARNESS.search(path):
        return "harness"
    if who.get("slot_uid") is not None and f"/user-{who['slot_uid']}.slice/" in within:
        return "session_services"
    return "system"


def _resident(c: dict) -> float:
    """What a cgroup holds that the kernel cannot reclaim as cache: anon, shmem, kernel."""
    return round(c["anon_mb"] + c["shmem_mb"] + c["kernel_mb"], 1)


def reserve_mib(table: dict, mib: dict) -> dict:
    """What MemAvailable withholds from free memory and cache, as si_mem_available() does:
    the zones' reserve (each zone's high watermark plus its largest lowmem protection,
    capped at its managed pages: totalreserve_pages) and, of the file LRU and reclaimable
    kernel memory, min(half, the zones' low watermarks summed) each."""
    zones, page = table.get("zones") or {}, table.get("page_size")
    if not zones or not page:
        raise core.Unobservable("/proc/zoneinfo or the page size could not be read")
    to_mib = page / 2**20
    reserve = sum(min(z["protection"] + z["high"], z["managed"]) for z in zones.values()) * to_mib
    low = sum(z["low"] for z in zones.values()) * to_mib
    missing = [k for k in ("Active(file)", "Inactive(file)", "KReclaimable") if k not in mib]
    if missing:
        raise core.Unobservable(f"/proc/meminfo did not name {', '.join(missing)}")
    cache = mib["Active(file)"] + mib["Inactive(file)"]
    withheld = min(cache / 2, low) + min(mib["KReclaimable"] / 2, low)
    return {"zones_mib": round(reserve, 1), "withheld_cache_mib": round(withheld, 1),
            "low_watermarks_mib": round(low, 1)}


def guest_breakdown(table: dict) -> dict:
    """/proc/meminfo and /proc/zoneinfo -> the guest's used memory (MemTotal less
    MemAvailable) split into anon, shmem, kernel, the reserve MemAvailable withholds, and
    what is left: pages a driver or the kernel took that no meminfo counter names (the
    VM's DRM buffers among them)."""
    kib = table["meminfo_kib"]
    missing = [k for k in GUEST_FIELDS if k not in kib]
    if missing:
        raise core.Unobservable(f"/proc/meminfo did not name {', '.join(missing)}")
    mib = {k: round(v / 1024, 1) for k, v in kib.items()}
    used = round(mib["MemTotal"] - mib["MemAvailable"], 1)
    kernel = round(sum(mib[k] for k in GUEST_KERNEL) + mib.get("SecPageTables", 0.0), 1)
    reserve = reserve_mib(table, mib)
    held = reserve["zones_mib"] + reserve["withheld_cache_mib"]
    return {"used_mib": used, "anon_mib": mib["AnonPages"], "shmem_mib": mib["Shmem"],
            "kernel_mib": kernel, "reserve_mib": round(held, 1),
            "untracked_mib": round(used - mib["AnonPages"] - mib["Shmem"] - kernel - held, 1),
            "reserve_parts_mib": reserve,
            "kernel_parts_mib": {k: mib.get(k) for k in GUEST_KERNEL + ("SecPageTables",)},
            "meminfo_mib": mib}


def footprint_of(table: dict, slot_uid: int) -> dict:
    """A `core.parse_processes` table -> where the guest's memory is.

    Every cgroup a process runs in is charged where its memory controller is (itself, or
    the nearest ancestor with one); each charge point lands in one bucket. A bucket's
    `resident_mib` is its anon + shmem + kernel: what it holds that is not reclaimable
    cache. Every process is listed, all users, by its anonymous RSS, largest first: with
    no PSS on Tilo's kernel, that is the memory a process holds alone."""
    procs, cgroups = table["procs"], table["cgroups"]
    slot = {p["pid"]: p for p in procs.values() if p["uid"] == slot_uid}
    shell = slot.get(compositor_pid(slot))
    by_point: dict[str, list[dict]] = {}
    uncharged = []
    for p in procs.values():
        point = (cgroups.get(p["cgroup"]) or {}).get("charged_at")
        if point is None:
            uncharged.append(p)
        else:
            by_point.setdefault(point, []).append(p)
    who = {"compositor_cgroup": (cgroups.get(shell["cgroup"]) or {}).get("charged_at") if shell else None,
           "greeter_uid": table.get("greeter_uid"), "slot_uid": slot_uid}
    buckets = {b: {"resident_mib": 0.0, "cgroups": []} for b in BUCKETS}
    point_bucket = {}
    for point, members in sorted(by_point.items()):
        c = cgroups[point]
        if None in (c["memory_mb"], c["anon_mb"], c["shmem_mb"], c["kernel_mb"]):
            raise core.Unobservable(f"cgroup {point} has no memory.current or memory.stat anon/shmem/kernel")
        b = point_bucket[point] = bucket_of(point, members, who)
        buckets[b]["cgroups"].append({"cgroup": point, "resident_mib": _resident(c),
                                      **{k: c[k] for k in ("memory_mb", "anon_mb", "file_mb", "shmem_mb",
                                                           "file_mapped_mb", "kernel_mb")},
                                      "comms": sorted({p["comm"] for p in members})})
    for b in buckets.values():
        b["cgroups"].sort(key=lambda c: -c["resident_mib"])
        b["resident_mib"] = round(sum(c["resident_mib"] for c in b["cgroups"]), 1)
        for k in ("anon_mb", "shmem_mb", "kernel_mb", "file_mb"):
            b[k.replace("_mb", "_mib")] = round(sum(c[k] for c in b["cgroups"]), 1)
    rows = []
    for p in procs.values():
        point = (cgroups.get(p["cgroup"]) or {}).get("charged_at")
        rows.append({"pid": p["pid"], "comm": p["comm"], "uid": p["uid"],
                     "bucket": point_bucket.get(point), "app": p["app"], "cgroup": p["cgroup"],
                     **{k: p[k] for k in ("anon_rss_mb", "shmem_rss_mb", "file_rss_mb", "rss_mb",
                                          "swap_mb")}})
    rows.sort(key=lambda r: -(r["anon_rss_mb"] or 0))
    guest = guest_breakdown(table)
    vm_only = round(sum(buckets[b]["resident_mib"] for b in VM_ONLY), 1)
    attributed = sum(b["resident_mib"] for b in buckets.values())
    return {
        "guest": guest,
        "buckets": buckets,
        # Anon, shmem and kernel memory of the guest that no listed cgroup is charged:
        # tmpfs written by a unit that has since exited (reparented to its slice), the
        # kernel's own, DRM buffers of the VM's display.
        "unattributed_mib": round(guest["anon_mib"] + guest["shmem_mib"] + guest["kernel_mib"] - attributed, 1),
        "vm_only_mib": vm_only,
        "compositor_pid": shell and shell["pid"],
        "processes": rows,
        "uncharged_processes": sorted(p["comm"] for p in uncharged),
        "tmpfs": table.get("tmpfs", []),
    }


def footprint(ctx) -> dict:
    """Every process's and cgroup's memory in the guest, read as root in one script."""
    return footprint_of(core.processes(ctx), ctx.vm.slot_uid())


def top_by_comm(rows: list[dict], n: int = 10) -> list[tuple[str, float]]:
    """Anonymous RSS summed per process name, the `n` largest: `fs` runs three times."""
    sums: dict[str, float] = {}
    for r in rows:
        if r["anon_rss_mb"] is not None:
            sums[r["comm"]] = sums.get(r["comm"], 0.0) + r["anon_rss_mb"]
    return sorted(((c, round(v, 1)) for c, v in sums.items()), key=lambda t: -t[1])[:n]


def kpi_word(name: str) -> str:
    """A process name as one KPI path element: `(sd-pam)` -> `sd-pam`."""
    return re.sub(r"[^A-Za-z0-9_-]+", "_", name).strip("_") or "unnamed"


def record_footprint(ctx, fp: dict, ram_mib: float, variant: str = "") -> None:
    """`idle.ram_mib.{anon,shmem,kernel,reserve,untracked,harness,installed}`,
    `memory.idle_mib.<bucket>` and the largest processes' anonymous RSS as
    `memory.idle_anon_mib.<process>`; `variant` is a suffix.

    `installed` is `idle.ram_mib` less what only a dev VM runs (seat0's greeter, the dev
    image's services, the harness's ssh), counted as anon + shmem + kernel so that
    reclaimable cache, which `idle.ram_mib` leaves out, is never subtracted."""
    suffix = f".{variant}" if variant else ""
    g = fp["guest"]
    for part in ("anon", "shmem", "kernel", "reserve", "untracked"):
        ctx.record(f"idle.ram_mib.{part}{suffix}", "MiB", [g[f"{part}_mib"]], what=f"guest {part} (resources.py guest_breakdown)")
    ctx.record(f"idle.ram_mib.harness{suffix}", "MiB", [fp["vm_only_mib"]],
               what="anon+shmem+kernel of the dev VM's greeter and harness cgroups")
    ctx.record(f"idle.ram_mib.installed{suffix}", "MiB", [round(ram_mib - fp["vm_only_mib"], 1)],
               what="idle.ram_mib less the dev VM's greeter and harness")
    for name, b in fp["buckets"].items():
        ctx.record(f"memory.idle_mib.{name}{suffix}", "MiB", [b["resident_mib"]],
                   what="anon+shmem+kernel of the bucket's cgroups")
    ctx.record(f"memory.idle_mib.unattributed{suffix}", "MiB", [fp["unattributed_mib"]],
               what="guest anon+shmem+kernel that no process's cgroup is charged")
    for comm, anon in top_by_comm(fp["processes"]):
        ctx.record(f"memory.idle_anon_mib.{kpi_word(comm)}{suffix}", "MiB", [anon],
                   what="anonymous RSS summed per process name")


def write_footprint(ctx, name: str, fp: dict, ram_mib: float, variant: str = "") -> None:
    ctx.write(name, json.dumps({"idle_ram_mib": ram_mib, **fp}, indent=1))
    record_footprint(ctx, fp, ram_mib, variant)


# Mapping classes that hold no memory of their own: address space reserved by
# guards and glibc's arena tails, and the kernel's vdso/vvar pages.
SHELL_UNCOUNTED = ("reserved", "kernel_pages")


def parse_shell_memory_map(answer) -> dict:
    """The compositor's `process_memory` answer -> the report, checked: Unobservable
    unless it names its RSS and at least one class of mapping with resident memory."""
    if not isinstance(answer, dict):
        raise core.Unobservable(f"process_memory gave no answer: {str(answer)[:200]!r}")
    classes = (answer.get("mappings") or {}).get("classes") or {}
    if not answer.get("status_kib", {}).get("RssAnon") or not any(
            c.get("resident_kib") for name, c in classes.items() if name not in SHELL_UNCOUNTED):
        raise core.Unobservable(f"process_memory named no resident mappings: {str(answer)[:200]!r}")
    return answer


def shell_memory_map(ctx) -> dict:
    """The compositor's own breakdown of its memory by kind of mapping (mincore over
    /proc/self/maps, in-process, as Tilo's kernel has no smaps), glibc's heap split and
    its threads by name: `tilo-control raw '{"op":"process_memory"}'`."""
    try:
        reply = ctx.vm.raw({"op": "process_memory"})
    except Exception as e:  # noqa: BLE001 - any failure to ask is an unobserved subject
        raise core.Unobservable(f"process_memory: {e}") from e
    return parse_shell_memory_map((reply or {}).get("answer"))


def shell_parts_mib(report: dict) -> dict:
    """Resident MiB per mapping class, largest first, plus glibc's heap in use and free."""
    classes = report["mappings"]["classes"]
    parts = {name: round(c["resident_kib"] / 1024, 1)
             for name, c in sorted(classes.items(), key=lambda kv: -kv[1]["resident_kib"])
             if name not in SHELL_UNCOUNTED}
    malloc = report.get("malloc") or {}
    if "in_use_kib" in malloc:
        parts["malloc_in_use"] = round(malloc["in_use_kib"] / 1024, 1)
        parts["malloc_free"] = round(malloc["free_kib"] / 1024, 1)
    return parts


def record_shell_parts(ctx, report: dict, variant: str = "") -> None:
    """`memory.shell_parts_mib.<class>`: the compositor's resident memory per mapping
    class; `memory.shell_parts_mib.malloc_{in_use,free}`: glibc's heap split;
    `memory.shell_threads`: its thread count. `variant` is a suffix."""
    suffix = f".{variant}" if variant else ""
    for name, value in shell_parts_mib(report).items():
        ctx.record(f"memory.shell_parts_mib.{name}{suffix}", "MiB", [value],
                   what="compositor resident memory by mapping class (resources.py shell_memory_map)")
    threads = report["status_kib"].get("Threads")
    if threads is not None:
        ctx.record(f"memory.shell_threads{suffix}", "count", [threads], what="compositor threads")


def shell_idle_anon_problems(anon_mib: float, budgets: dict) -> list[str]:
    """The idle compositor's anonymous RSS against `shell_idle_anon_mb`."""
    budget = budgets["shell_idle_anon_mb"]
    if anon_mib > budget:
        return [f"idle compositor anonymous RSS {anon_mib} MB > {budget} MB "
                "(R01-shell-memory-map.json names its mappings)"]
    return []


def shell_anon_mib(table: dict, slot_uid: int) -> float | None:
    """The compositor's anonymous RSS in a `core.parse_processes` table."""
    slot = {p["pid"]: p for p in table["procs"].values() if p["uid"] == slot_uid}
    pid = compositor_pid(slot)
    return slot[pid]["anon_rss_mb"] if pid is not None else None


def slope(values: list[float]) -> float | None:
    """The least-squares slope of `values` over their index: growth per step."""
    n = len(values)
    if n < 2:
        return None
    mean_x, mean_y = (n - 1) / 2, sum(values) / n
    num = sum((i - mean_x) * (v - mean_y) for i, v in enumerate(values))
    den = sum((i - mean_x) ** 2 for i in range(n))
    return round(num / den, 2)


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
        # The part of idle.shell_cpu_pct on the GPU driver's threads: on lavapipe, the
        # window's frames rasterised.
        ctx.record("idle.gpu_driver_cpu_pct", "percent", [m["shell_gpu_driver_cpu_pct"]],
                   frames=m["frames_painted"], software=software_renderer(ctx), **window)
        # Context switches of the compositor's main thread, its loop: each is a wakeup or
        # a preemption.
        ctx.record("idle.wakeups_per_s.shell", "per_s", [m["shell_wakeups_per_s"]], **window)
        # The same summed over every thread of the compositor process.
        ctx.record("idle.wakeups_per_s.shell_threads", "per_s", [m["shell_threads_wakeups_per_s"]],
                   session_age_s=m["session_age_s"], **window)
        ctx.record("idle.loop_ticks_per_s", "per_s", [m["shell_loop_ticks_per_s"]], **window)
        ctx.record("idle.frames", "count", [m["frames_painted"]], **window)


FOUR_APPS = ("so.tilo.terminal", "so.tilo.file-explorer", "so.tilo.settings", "so.tilo.notes")


def _open_several(ctx, apps: tuple[str, ...] = FOUR_APPS, name: str = "R"):
    """Open `apps` (R02's four) in the active workspace; return the ones still open 8 s later.

    An app whose window closed on its own in that time is recorded in
    facts["apps_closed_after_open"], and one whose window never appeared in
    facts["apps_failed_to_open"] with why, each with what the session log and the user
    journal said about it (`<name>-apps-closed-after-open.log`), rather than measured as if
    it were running."""
    ctx.reset()
    shells = core.terminal_shells(ctx) if "so.tilo.terminal" in apps else set()
    groups, failed = {}, {}
    for app in apps:
        try:
            group, _ = ctx.open_app(app)
            groups[app] = group["id"]
        except core.CheckFailed as e:
            failed[app] = str(e)[:300]
    time.sleep(8)
    # Terminal's shell runs once its guest is built, which on an account's first session
    # imports the environment's runtime first. Waiting for it keeps that
    # one-time import out of the idle window that follows.
    if "so.tilo.terminal" in groups:
        ctx.facts["terminal_shell_s"] = core.wait_new_terminal_shell(ctx, shells, core.TERMINAL_SHELL_S)
    st = ctx.state()
    closed = [app for app, gid in groups.items() if core.group_by_id(st, gid) is None]
    if closed or failed:
        ctx.shot(f"{name}-apps-closed-after-open")
        pattern = "|".join(a.replace(".", "\\.") for a in [*closed, *failed])
        # The compositor's own lines and the apps' ends, not their start-up chatter.
        ctx.write(f"{name}-apps-closed-after-open.log", ctx.vm.exec(
            "sed 's/\\x1b\\[[0-9;]*m//g' \"$HOME/.local/state/tilo/session-logs/latest.log\" 2>/dev/null "
            f"| grep -avE 'tilo-sandbox-exec|MESA|service socket connected|action contract validation' "
            f"| grep -aE '^[^\\[]|{pattern}' | grep -aiE '{pattern}|exit|signal|panic|close|kill|fail|launch|surface' "
            "| tail -n 150 | cut -c1-400; echo ---; "
            f"journalctl --user -b --no-pager -o short-monotonic 2>/dev/null "
            f"| grep -aE 'scope|{pattern}' | grep -aiE 'stop|deactivat|fail|exit|kill|signal|consumed|dump' "
            "| tail -n 60 | cut -c1-400; echo ---; coredumpctl --no-pager list 2>/dev/null | tail -n 10; true",
            check=False))
    ctx.facts["apps_closed_after_open"] = closed
    ctx.facts["apps_failed_to_open"] = failed
    opened = [app for app in groups if app not in closed]
    ctx.facts["apps_open_for_memory"] = opened
    if failed or closed:
        problems = [f"{app}: {why}" for app, why in failed.items()]
        problems.extend(f"{app}: closed on its own after opening" for app in closed)
        raise core.Unobservable("could not establish the app workload: " + "; ".join(problems))
    return opened


def software_renderer(ctx) -> bool:
    """Whether the VM renders on a CPU rasteriser (lavapipe), from run.py's renderer line, as
    records.renderer reads it (which raises on a line it cannot name; this only asks one thing)."""
    return (ctx.facts.get("renderer") or "").lower().startswith("software")


def _budget_problems(m, budgets, prefix, software=False):
    """The idle window against its budgets.

    On a software renderer (`software`) the compositor's CPU is judged without its CPU
    rasteriser's threads. On a real desktop a GPU does the work of the frames the
    budget allows, a few milliseconds of it, while lavapipe spends ~0.4 s of CPU on each. The
    frames stay gated, and a rasteriser that runs with no frame painted fails."""
    problems = []
    if m["shell_cpu_pct"] is None:
        raise core.Unobservable("no `shell` process of the slot user found")
    raster = (m["shell_gpu_driver_cpu_pct"] or 0.0) if software else 0.0
    own = round(m["shell_cpu_pct"] - raster, 2)
    if own > budgets[f"{prefix}_shell_cpu_pct"]:
        groups = [g for g in m["shell_thread_groups"] or []
                  if not software or g["name"] not in GPU_DRIVER_GROUPS]
        busiest = [describe_threads(groups)]
        if m["shell_exited_threads_cpu_pct"]:
            busiest.append(f"exited threads {m['shell_exited_threads_cpu_pct']}%")
        what = (f"compositor CPU less its software rasteriser's {raster}% is {own}%" if software
                else f"compositor CPU {own}%")
        problems.append(f"{what} > {budgets[f'{prefix}_shell_cpu_pct']}% "
                        f"({', '.join(filter(None, busiest)) or 'no thread named'}; "
                        f"session age {m['session_age_s']} s)")
    if software and m["frames_painted"] == 0 and raster > budgets["idle_software_raster_cpu_pct"]:
        problems.append(f"software rasteriser CPU {raster}% with no frame painted > "
                        f"{budgets['idle_software_raster_cpu_pct']}%")
    if m["frames_painted"] > budgets[f"{prefix}_frames"]:
        why = describe_painters(m.get("painted_by") or {})
        problems.append(f"{m['frames_painted']} frames painted while idle > {budgets[f'{prefix}_frames']}"
                        + (f" ({why})" if why else ""))
    if m["shell_loop_ticks_per_s"] > budgets["idle_loop_ticks_per_s"]:
        problems.append(f"compositor loop woke {m['shell_loop_ticks_per_s']}/s > {budgets['idle_loop_ticks_per_s']}/s")
    if m["user_cpu_pct_total"] > budgets[f"{prefix}_user_cpu_pct"]:
        top = ", ".join(f"{p['comm']} {p['cpu_pct']}%" for p in m["top"][:4])
        problems.append(f"session CPU {m['user_cpu_pct_total']}% > {budgets[f'{prefix}_user_cpu_pct']}% ({top})")
    return problems


def ush_problems(ush: dict | None, shell_s: float | None, budgets: dict) -> list[str]:
    """Terminal's shell runtime against its idle budget: an idle Terminal,
    its shell running and nothing typed, costs `tilo-ush` almost nothing."""
    if shell_s is None:
        return [f"Terminal's shell did not run within {core.TERMINAL_SHELL_S} s, so tilo-ush "
                "was not measured idle (its first-session import may still have been running)"]
    if ush is None:
        return ["no tilo-ush process of the slot user ran while Terminal was open"]
    problems = []
    busiest = describe_threads(ush["thread_groups"])
    if ush["cpu_pct"] > budgets["idle_apps_ush_cpu_pct"]:
        problems.append(f"tilo-ush CPU {ush['cpu_pct']}% > {budgets['idle_apps_ush_cpu_pct']}% "
                        f"({busiest or 'no thread named'}; exited threads {ush['exited_cpu_pct']}%)")
    if ush["wakeups_per_s"] > budgets["idle_apps_ush_wakeups_per_s"]:
        woke = ", ".join(f"{g['name']} {g['ctxt_switches_per_s']}/s"
                         for g in sorted(ush["thread_groups"], key=lambda g: -g["ctxt_switches_per_s"])[:4]
                         if g["ctxt_switches_per_s"] > 0)
        problems.append(f"tilo-ush woke {ush['wakeups_per_s']}/s > "
                        f"{budgets['idle_apps_ush_wakeups_per_s']}/s ({woke})")
    return problems


def _ush_idle(ctx, res, m: dict, budgets: dict) -> list[str]:
    """Record and judge tilo-ush in R02's window, once Terminal's shell has run."""
    ush, shell_s = m["ush"], m["terminal_shell_s"]
    if shell_s is not None and ush is not None:
        window = {"window_s": m["seconds"], "processes": ush["processes"]}
        ctx.record("idle.cpu_pct.tilo-ush.four_apps", "percent", [ush["cpu_pct"]], **window)
        ctx.record("idle.wakeups_per_s.tilo-ush.four_apps", "per_s", [ush["wakeups_per_s"]], **window)
    return ush_problems(ush, shell_s, budgets)


@check("R01", AREA, "Idle with no apps: compositor asleep, no frames, little CPU")
def idle_empty(ctx, res):
    ctx.reset()
    time.sleep(5)
    m = measure(ctx, ctx.config["idle_seconds"], "R01")
    m["guest_ram_used_mib"] = guest_ram_used_mib(ctx)
    res.metrics.update(m, renderer=ctx.facts.get("renderer"))
    ctx.write("R01-idle-empty.json", json.dumps(m, indent=1))
    if m["shell_cpu_pct"] is not None:
        _record_idle(ctx, m, m["guest_ram_used_mib"])
    problems = _budget_problems(m, ctx.config["budgets"], "idle", software_renderer(ctx))
    # The compositor's own memory with no window open: R03's figure less this one is
    # what four windows cost.
    try:
        smaps = core.shell_memory(ctx, m["compositor_pid"])
    except core.Unobservable as e:
        problems.append(str(e))
    else:
        ctx.write("R01-shell-smaps.json", json.dumps(smaps, indent=1))
        res.metrics["shell_anon_rss_mib"] = smaps["anon_rss_mib"]
        ctx.record("memory.shell_anon_mib.empty", "MiB", [smaps["anon_rss_mib"]])
        _record_shell_shm(ctx, res, smaps)
        problems += shell_idle_anon_problems(smaps["anon_rss_mib"], ctx.config["budgets"])
    # What the idle RAM is made of, every user's processes and cgroups.
    try:
        fp = footprint(ctx)
    except core.Unobservable as e:
        problems.append(f"idle memory breakdown: {e}")
    else:
        write_footprint(ctx, "R01-footprint.json", fp, m["guest_ram_used_mib"])
        res.metrics["footprint_mib"] = _footprint_summary(fp, m["guest_ram_used_mib"])
    # What the compositor's own memory is: heap, stacks, GPU driver memory, fonts.
    try:
        report = shell_memory_map(ctx)
    except core.Unobservable as e:
        problems.append(f"compositor memory map: {e}")
    else:
        ctx.write("R01-shell-memory-map.json", json.dumps(report, indent=1))
        record_shell_parts(ctx, report)
        res.metrics["shell_parts_mib"] = shell_parts_mib(report)
    expect(not problems, "; ".join(problems))


def _record_shell_shm(ctx, res, smaps: dict, variant: str = "") -> None:
    """`memory.compositor_memfd_mib{.variant}`: shmem allocated to the memfds the
    compositor holds open, mapped or not, with the per-name split in the metrics."""
    suffix = f".{variant}" if variant else ""
    by_name = smaps["shm_by_name"]
    res.metrics[f"shell_shm_by_name{suffix}"] = by_name
    ctx.record(f"memory.compositor_memfd_mib{suffix}", "MiB",
               [round(sum(v["allocated_mib"] for v in by_name.values()), 1)],
               what="shmem allocated to the compositor's open memfds and /dev/shm files",
               largest=next(iter(by_name), None))


def _footprint_summary(fp: dict, ram_mib: float) -> dict:
    return {"used": ram_mib, "installed": round(ram_mib - fp["vm_only_mib"], 1),
            **{k: fp["guest"][f"{k}_mib"] for k in ("anon", "shmem", "kernel", "reserve", "untracked")},
            "buckets": {name: b["resident_mib"] for name, b in fp["buckets"].items()},
            "unattributed": fp["unattributed_mib"],
            "top_anon": top_by_comm(fp["processes"])}


@check("R02", AREA, "Idle with Terminal, Files, Settings and Notes open")
def idle_apps(ctx, res):
    opened = _open_several(ctx)
    res.metrics["opened"] = opened
    m = measure(ctx, ctx.config["idle_seconds"], "R02")
    # What still asks for frames as the idle window ended, with geometry the
    # trace's labels leave out, and each window's frame state (presented,
    # last frame, size against its tile).
    try:
        motion = {"animating": (ctx.vm.state().get("chrome") or {}).get("animating"),
                  "windows": ctx.vm.foreign_frames()}
    except VmError as e:
        motion = {"error": str(e)}
    ctx.write("R02-motion.json", json.dumps(motion, indent=1))
    m["guest_ram_used_mib"] = guest_ram_used_mib(ctx)
    # Seconds from opening Terminal to its shell running: the window opened after it.
    m["terminal_shell_s"] = ctx.facts.get("terminal_shell_s")
    res.metrics.update(m, renderer=ctx.facts.get("renderer"))
    ctx.write("R02-idle-apps.json", json.dumps(m, indent=1))
    if m["shell_cpu_pct"] is not None and len(opened) == 4:
        _record_idle(ctx, m, m["guest_ram_used_mib"], "four_apps")
    ctx.facts["apps_open_for_memory"] = opened
    problems = _budget_problems(m, ctx.config["budgets"], "idle_apps", software_renderer(ctx))
    if "so.tilo.terminal" in opened:
        problems += _ush_idle(ctx, res, m, ctx.config["budgets"])
    try:
        fp = footprint(ctx)
    except core.Unobservable as e:
        problems.append(f"memory breakdown with four apps: {e}")
    else:
        ctx.write("R02-footprint.json", json.dumps({"idle_ram_mib": m["guest_ram_used_mib"], **fp}, indent=1))
        if len(opened) == 4:
            record_footprint(ctx, fp, m["guest_ram_used_mib"], "four_apps")
        res.metrics["footprint_mib"] = _footprint_summary(fp, m["guest_ram_used_mib"])
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
    opened = ctx.facts.get("apps_open_for_memory", [])
    if set(opened) != set(FOUR_APPS):
        opened = _open_several(ctx)
    m = memory(ctx, opened)
    res.metrics.update(apps=m["apps"], shell=m["shell"], session_memory_mb=m["session_memory_mb"],
                       top=m["procs"][:15], guest_mem_mb=m["guest_mem_mb"])
    ctx.write("R03-memory.json", json.dumps(m, indent=1))
    _record_memory(ctx, m)
    b = ctx.config["budgets"]
    expect(m["shell"], "no compositor process found")
    problems, unobserved = [], []
    try:
        smaps = core.shell_memory(ctx, m["shell"]["pid"])
    except core.Unobservable as e:
        unobserved.append(str(e))
    else:
        ctx.write("R03-shell-smaps.json", json.dumps(smaps, indent=1))
        _record_shell_shm(ctx, res, smaps, "four_apps")
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
    # The same idle desktop once the apps have closed: what they leave behind.
    try:
        after = _footprint_after_close(ctx)
    except core.Unobservable as e:
        unobserved.append(f"memory after the apps closed: {e}")
    else:
        ram = after["guest"]["used_mib"]
        write_footprint(ctx, "R03-footprint-after-close.json", after, ram, "after_close")
        ctx.record("idle.ram_mib.after_close", "MiB", [ram], what="guest MemTotal - MemAvailable")
        res.metrics["after_close_mib"] = _footprint_summary(after, ram)
        try:
            smaps = core.shell_memory(ctx, m["shell"]["pid"])
        except core.Unobservable as e:
            unobserved.append(f"the compositor's memory after the apps closed: {e}")
        else:
            ctx.write("R03-shell-smaps-after-close.json", json.dumps(smaps, indent=1))
            _record_shell_shm(ctx, res, smaps, "after_close")
        # The renderer's own account: its atlas and buffers as they grew and shrank, and
        # each trim's release of freed device memory.
        ctx.write("R03-renderer-memory.log", ctx.vm.exec(
            f'grep -a -E "RENDERER-MEMORY|DEVICE-MEMORY" "{core.SESSION_LOG}"; true', check=False))
        try:
            report = shell_memory_map(ctx)
        except core.Unobservable as e:
            unobserved.append(f"compositor memory map after the apps closed: {e}")
        else:
            ctx.write("R03-shell-memory-map-after-close.json", json.dumps(report, indent=1))
            record_shell_parts(ctx, report, "after_close")
    try:
        res.metrics["disk"] = record_disk(ctx)
    except core.Unobservable as e:
        unobserved.append(f"per-user disk use: {e}")
    expect(not problems, "; ".join(problems + unobserved))
    if unobserved:
        raise core.Unobservable("; ".join(unobserved))


AFTER_CLOSE_SETTLE_S = 5

# The slot user's per-user runtime builds and app store, in bytes: what downloaded
# runtimes and apps hold on disk once collection after each change ran (#4114,
# #4151). A build is a directory under tilo-runtime whose name has no leading dot.
DISK = r"""
data=${XDG_DATA_HOME:-$HOME/.local/share}
for d in tilo-runtime tilo-apps; do
  if [ -d "$data/$d" ]; then echo "$d $(du -sb "$data/$d" | cut -f1)"; else echo "$d 0"; fi
done
if [ -d "$data/tilo-runtime" ]; then
  echo "builds $(find "$data/tilo-runtime" -mindepth 1 -maxdepth 1 -type d ! -name '.*' | wc -l)"
else
  echo "builds 0"
fi
"""


def parse_disk(text: str) -> dict:
    """`DISK` output -> {"runtime_bytes", "app_store_bytes", "runtime_builds"}."""
    values = {}
    for line in text.splitlines():
        f = line.split()
        if len(f) == 2 and f[1].isdigit():
            values[f[0]] = int(f[1])
    missing = {"tilo-runtime", "tilo-apps", "builds"} - values.keys()
    if missing:
        raise core.Unobservable(f"per-user disk use not reported for {sorted(missing)}: {text[-200:]!r}")
    return {"runtime_bytes": values["tilo-runtime"], "app_store_bytes": values["tilo-apps"],
            "runtime_builds": values["builds"]}


def record_disk(ctx) -> dict:
    """`disk.runtime_mib.per_user`, `disk.app_store_mib.per_user` and
    `disk.runtime_builds.per_user`: the slot user's downloaded runtimes and apps."""
    disk = parse_disk(ctx.vm.exec(DISK))
    ctx.record("disk.runtime_mib.per_user", "MiB", [round(disk["runtime_bytes"] / 2**20, 1)],
               what="du of $XDG_DATA_HOME/tilo-runtime", builds=disk["runtime_builds"])
    ctx.record("disk.app_store_mib.per_user", "MiB", [round(disk["app_store_bytes"] / 2**20, 1)],
               what="du of $XDG_DATA_HOME/tilo-apps")
    ctx.record("disk.runtime_builds.per_user", "count", [disk["runtime_builds"]],
               what="runtime builds under $XDG_DATA_HOME/tilo-runtime")
    return disk


def _footprint_after_close(ctx) -> dict:
    """Close every window, wait for the apps' processes to exit and the compositor to
    settle (it trims its heap after a close), then read the footprint."""
    ctx.reset()
    gone = wait_until(lambda: not any(p["app"] for p in core.processes(ctx)["procs"].values()), 20, 1.0)
    if not gone:
        raise core.Unobservable("app processes were still running 20 s after their windows closed")
    time.sleep(AFTER_CLOSE_SETTLE_S)
    return footprint(ctx)


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


# -- the installed-desktop gates of os-acceptance-{arch}, on this run's VM ---------

# Test tooling the image does not ship: the run copies it from the checkout into the guest.
BOOT_GATE = Path(__file__).resolve().parents[4] / "os/tests/boot_performance_gate.sh"
BOOT_TOTAL = re.compile(r"kernel\+userspace reached usable greeter in (\d+)\.(\d{3})s")
BOOT_PHASES = re.compile(r"phases: kernel ([0-9.]+)s, initrd ([0-9.]+)s, userspace to greeter ([0-9.]+)s")
GATE_EXIT = re.compile(r"^GATE-EXIT=(\d+)$", re.M)


def guest_script(script: Path, env: str = "") -> str:
    """A shell fragment that writes `script` into a private directory in the guest and runs it
    there with `env` assignments; the directory goes when the calling shell exits."""
    body = script.read_text()
    end = "TILO_GUEST_SCRIPT_EOF"
    assert end not in body
    return (f'd="$(mktemp -d)" && trap \'rm -rf "$d"\' EXIT && cat > "$d/{script.name}" <<\'{end}\'\n'
            f"{body}{'' if body.endswith(chr(10)) else chr(10)}{end}\n"
            f'{env} bash "$d/{script.name}"')


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
    """The release gate's boot check (os/tests/boot_performance_gate.sh, copied into the
    guest for the run): graphical.target and greetd's login endpoint up, no device timeout, the
    image's boot contract, all within the budget. It reads this boot's monotonic
    timestamps, so it may run at any point of the run."""
    budget = ctx.config["budgets"]["boot_to_greeter_s"]
    text = ctx.vm.root_exec(guest_script(BOOT_GATE, f"TILO_BOOT_USERSPACE_BUDGET_SEC={budget}")
                            + " 2>&1; echo GATE-EXIT=$?", timeout=180)
    ctx.write("R06-boot-gate.txt", text)
    gate = parse_boot_gate(text)
    res.metrics.update({k: v for k, v in gate.items() if k != "exit"}, budget_s=budget)
    said = " | ".join(l.strip() for l in text.splitlines()[-8:] if l.strip() and "GATE-EXIT" not in l)
    if gate["to_greeter_s"] is None:
        raise core.Unobservable(f"the boot gate named no time to the greeter (exit {gate['exit']}): {said}")
    ctx.record("boot.to_greeter_s", "s", [gate["to_greeter_s"]], what="kernel+userspace, firmware excluded")
    for phase in ("kernel", "initrd", "userspace"):
        ctx.record(f"boot.{phase}_s", "s", [gate.get(f"{phase}_s")])
    problems = [] if gate["exit"] == 0 else [f"the boot gate failed: {said}"]
    problems += boot_attribution(ctx, res)
    boot_kernel_log(ctx, res)
    expect(not problems, "; ".join(problems))


# The kernel's own log of this boot, written whole, and the boots the persistent journal
# holds. dmesg's ring buffer can have wrapped by the time R06 runs; journald keeps each
# kernel message with the kernel's own timestamp, so it stands in when the early boot is gone.
BOOT_KERNEL_LOG = r"""
exec 2>&1
[ -d /var/log/journal ] && echo "journal=persistent" || echo "journal=volatile"
echo "== boots"; journalctl --list-boots --no-pager 2>/dev/null
log="$(dmesg --color=never 2>/dev/null)"
if ! printf '%s\n' "$log" | grep -q 'SLUB: HWalign'; then
  log="$(journalctl -k -b -o json --no-pager 2>/dev/null | python3 -c '
import json, sys
for line in sys.stdin:
    entry = json.loads(line)
    stamp, message = entry.get("_SOURCE_MONOTONIC_TIMESTAMP"), entry.get("MESSAGE")
    if stamp and isinstance(message, str):
        print(f"[{int(stamp) / 1e6:12.6f}] {message}")
')"
fi
echo "== dmesg"; printf '%s\n' "$log"
echo "== end"
"""
KERNEL_LINE = re.compile(r"^\[\s*(\d+\.\d+)\]\s?(.*)$")
BOOT_ID = re.compile(r"\b[0-9a-f]{32}\b")


def parse_kernel_log(text: str) -> dict:
    """From BOOT_KERNEL_LOG's output: the kernel's memory init (the silence that ends at
    SLUB's first line, where the boot CPU hands all free RAM to the page allocator and,
    with init_on_free, zeroes it first), the mem auto-init policy it reported, and whether
    this was the first boot the persistent journal knows of."""
    out: dict = {}
    section = None
    boots = 0
    previous = None
    for line in text.splitlines():
        if line.startswith("== "):
            section = line[3:].strip()
            continue
        if section == "boots":
            boots += bool(BOOT_ID.search(line))
        elif section == "dmesg" and (m := KERNEL_LINE.match(line)):
            # A continuation line carries no stamp of its own and is no point in time.
            stamp, message = float(m.group(1)), m.group(2)
            if message.startswith("mem auto-init: stack:") and "mem_auto_init" not in out:
                out["mem_auto_init"] = message.removeprefix("mem auto-init: ")
            if message.startswith("SLUB: HWalign") and previous is not None and "kernel_meminit_s" not in out:
                out["kernel_meminit_s"] = round(stamp - previous, 3)
            previous = stamp
    if "journal=persistent" in text.splitlines() and boots:
        out["first_boot"] = boots == 1
    return out


def boot_kernel_log(ctx, res) -> None:
    """Write this boot's kernel log as evidence and record the kernel's memory init and
    whether the run measured a first boot (fresh host pages, first-boot units), which the
    whole boot's numbers depend on."""
    text = ctx.vm.root_exec(BOOT_KERNEL_LOG, timeout=120)
    ctx.write("R06-dmesg.txt", text)
    kernel = parse_kernel_log(text)
    res.metrics.update(kernel)
    # A log that lost its early boot records nothing; the KPI's budget entry then reports
    # it missing rather than comparing a guess.
    if "kernel_meminit_s" in kernel:
        ctx.record("boot.kernel_meminit_s", "s", [kernel["kernel_meminit_s"]],
                   what="kernel log silence ending at SLUB's first line: the boot CPU freeing all RAM")
    if "first_boot" in kernel:
        ctx.record("boot.first_boot", "count", [int(kernel["first_boot"])],
                   what="1 when this boot is the only one in the persistent journal")


# Where a passing boot's time went, written on every run (the gate names it only on a
# failing or slow boot): systemd's own phases, the greeter's critical chain, the units that
# took longest and the whole boot as a plot.
BOOT_ANALYSIS = r"""
exec 2>&1
echo "== time"; systemd-analyze time --no-pager
echo "== critical-chain greetd.service"; systemd-analyze critical-chain greetd.service --no-pager
echo "== critical-chain graphical.target"; systemd-analyze critical-chain graphical.target --no-pager
echo "== blame"; systemd-analyze blame --no-pager | grep -v '\.device$' | head -40
echo "== stamps"
echo "userspace_us=$(systemctl show -p UserspaceTimestampMonotonic --value)"
echo "greetd_us=$(systemctl show greetd.service -p ActiveEnterTimestampMonotonic --value)"
echo "== end"
"""
CHAIN_UNIT = re.compile(r"([A-Za-z0-9_@:.\\-]+\.(?:service|target|mount|socket|device|swap|path|timer|slice))[ @]")
STAMP = re.compile(r"^(userspace|greetd)_us=(\d+)$", re.M)
# Nothing the greeter needs comes from the network: greetd waits for
# systemd-user-sessions.service, which upstream orders After=network.target, so a network
# start job queued before logins are allowed holds the greeter until NetworkManager is up.
GREETER_NEEDS_NO = ("NetworkManager.service", "network.target", "network-online.target",
                    "firewalld.service", "NetworkManager-wait-online.service")


def parse_boot_analysis(text: str) -> dict:
    """The units on greetd's critical chain, in chain order, and greetd's start in seconds
    since kernel start and since userspace started."""
    sections: dict[str, list[str]] = {}
    name = None
    for line in text.splitlines():
        if line.startswith("== "):
            name = line[3:].strip()
            sections[name] = []
        elif name is not None:
            sections[name].append(line)
    chain = [m.group(1) for line in sections.get("critical-chain greetd.service", [])
             if (m := CHAIN_UNIT.search(line + " "))]
    stamps = {k: int(v) for k, v in STAMP.findall(text)}
    out = {"greeter_chain": chain}
    if stamps.get("greetd"):
        out["greeter_s"] = round(stamps["greetd"] / 1e6, 3)
        if stamps.get("userspace"):
            out["userspace_to_greeter_s"] = round((stamps["greetd"] - stamps["userspace"]) / 1e6, 3)
    return out


def boot_attribution(ctx, res) -> list[str]:
    """Write the boot's attribution as evidence, record greetd's start, and name what has no
    business on the greeter's critical chain or holds the greeter past its userspace budget."""
    text = ctx.vm.root_exec(BOOT_ANALYSIS, timeout=120)
    ctx.write("R06-boot-analysis.txt", text)
    ctx.write("R06-boot-plot.svg", ctx.vm.root_exec("systemd-analyze plot", timeout=120))
    analysis = parse_boot_analysis(text)
    res.metrics.update(analysis)
    if "greeter_s" not in analysis or not analysis["greeter_chain"]:
        raise core.Unobservable(f"systemd named no critical chain or start time for greetd: {text.strip()[-300:]}")
    ctx.record("boot.greeter_s", "s", [analysis["greeter_s"]], what="kernel start to greetd.service active")
    ctx.record("boot.userspace_to_greeter_s", "s", [analysis.get("userspace_to_greeter_s")],
               what="userspace start to greetd.service active")
    budget = ctx.config["budgets"]["userspace_to_greeter_s"]
    problems = [f"{unit} is on the greeter's critical chain" for unit in analysis["greeter_chain"]
                if unit in GREETER_NEEDS_NO]
    if analysis.get("userspace_to_greeter_s", 0) > budget:
        problems.append(f"userspace took {analysis['userspace_to_greeter_s']} s to the greeter > {budget} s "
                        f"(chain: {' <- '.join(analysis['greeter_chain'][:6])})")
    return problems


RENDER_REPORT = "/usr/libexec/tilo-session-report"
RENDER_UNOBSERVABLE = ("no session flight record", "the compositor logged no DRM render phase")


@check("R07", AREA, "The session presented within the render-path budgets (tilo-session-report --render-gate)")
def render_gate(ctx, res):
    """The release gate's render check on this run's session: direct GPU scanout where
    the renderer shares the scanout GPU, and every DRM render phase and the painted
    frames' main-thread work within the budgets os-acceptance-{arch} holds a KVM guest to."""
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


# -- greeter sign-in to the desktop ---------------------------------------

# One sh script, so ssh latency never lands inside the measured window: sign in at the
# seat0 greeter through its console action, then poll every 20 ms for each step of the
# session's start. Times are CLOCK_MONOTONIC seconds (/proc/timer_list's `now at`), the
# clock of the compositor's `FRAME-LIFECYCLE ... monotonic_us=` line.
SIGN_IN = r"""
user=__USER__; uid=$(id -u "$user"); home=$(getent passwd "$user" | cut -d: -f6)
gsock=$(ls /run/user/$(id -u greetd)/tilo-console/*.sock 2>/dev/null | head -1)
log="$home/.local/state/tilo/session-logs/latest.log"
ctl=/run/user/$uid/tilo-session/test-control.sock
now() { awk '/^now at/ { printf "%.3f", $3 / 1e9; exit }' /proc/timer_list; }
[ -S "$gsock" ] || { echo "SIGNIN error=no-greeter-socket"; exit 0; }
rm -f "$log"
t0=$(now)
printf '%s' __SECRET__ | tilo-control --socket "$gsock" actions invoke greeter sign-in@greeter --args-stdin >/tmp/acc-signin.out 2>&1
um= sess= shell= frame= usable=
i=0
while [ $i -lt 1500 ]; do
  i=$((i + 1)); t=$(now)
  [ -n "$um" ] || { systemctl is-active -q "user@$uid.service" && um=$t; }
  [ -n "$sess" ] || { pgrep -u "$uid" -f '^/bin/bash /usr/libexec/tilo-session' >/dev/null && sess=$t; }
  [ -n "$shell" ] || { pgrep -u "$uid" -x shell >/dev/null && shell=$t; }
  [ -n "$frame" ] || frame=$(sed -n 's/^FRAME-LIFECYCLE serial=1 event=render-retired .* monotonic_us=\([0-9]*\).*/\1/p' "$log" 2>/dev/null | head -1)
  if [ -z "$usable" ] && [ -S "$ctl" ] && timeout 3 tilo-control --socket "$ctl" state-dump >/dev/null 2>&1; then usable=$(now); fi
  [ -n "$um" ] && [ -n "$sess" ] && [ -n "$shell" ] && [ -n "$usable" ] \
    && grep -q '^FRAME-LIFECYCLE serial=1 ' "$log" 2>/dev/null && break
  sleep 0.02
done
grep -m1 '^FRAME-LIFECYCLE serial=1 ' "$log" 2>/dev/null
echo "SIGNIN t0=$t0 user_manager=$um session=$sess shell=$shell first_frame_us=$frame usable=$usable"
cut -c1-300 /tmp/acc-signin.out
"""
SIGN_OUT = r"""
loginctl terminate-user __USER__ 2>&1 || true
i=0
while [ $i -lt 300 ]; do
  i=$((i + 1))
  ! pgrep -u __USER__ -x shell >/dev/null && ls /run/user/$(id -u greetd)/tilo-console/*.sock >/dev/null 2>&1 && break
  sleep 0.2
done
sleep 3
echo "SIGNED-OUT shell=$(pgrep -u __USER__ -x shell | head -1)"
"""
SIGNIN_LINE = re.compile(r"^SIGNIN (.*)$", re.M)


def dev_account() -> tuple[str, str]:
    """The account a dev disk is built with (os/account.env: a development and test
    credential that never reaches a published image)."""
    env = (Path(__file__).resolve().parents[4] / "os" / "account.env").read_text()
    match = re.search(r'TILO_ACCOUNT="\$\{TILO_ACCOUNT:-([^:"]+):([^:"]+):', env)
    if not match:
        raise core.Unobservable("os/account.env names no dev account")
    return match.group(1), match.group(2)


def parse_sign_in(text: str) -> dict:
    """Seconds from the sign-in request to each step of the session's start; a step the
    probe never saw is absent. Usable is the later of the first presented frame and the
    compositor's control socket answering."""
    line = SIGNIN_LINE.search(text)
    if not line:
        return {}
    fields = dict(f.split("=", 1) for f in line.group(1).split() if "=" in f)
    try:
        t0 = float(fields["t0"])
    except (KeyError, ValueError):
        return {"error": fields.get("error", "no start time")}
    out = {}
    for key, name in (("user_manager", "user_manager_s"), ("session", "session_s"),
                      ("shell", "shell_s"), ("usable", "control_s")):
        if fields.get(key):
            out[name] = round(float(fields[key]) - t0, 3)
    if fields.get("first_frame_us"):
        out["first_frame_s"] = round(int(fields["first_frame_us"]) / 1e6 - t0, 3)
    if "first_frame_s" in out and "control_s" in out:
        out["usable_s"] = max(out["first_frame_s"], out["control_s"])
    return out


def sign_in(ctx, user: str, secret: str, label: str) -> dict:
    script = SIGN_IN.replace("__USER__", shlex.quote(user)).replace(
        "__SECRET__", shlex.quote(json.dumps({"password": secret})))
    text = ctx.vm.root_exec(script, timeout=120)
    ctx.write(f"R08-sign-in-{label}.txt", text)
    return parse_sign_in(text)


@check("R08", AREA, "Signing in at the greeter reaches a drawn, answering desktop within budget")
def sign_in_to_desktop(ctx, res):
    """Sign the dev account in at the seat0 greeter through its console action, as a
    person's passcode does, and time each step to a usable desktop: the user manager, the
    session wrapper, the compositor process, its first presented frame and its control
    socket answering. Twice: the account's first sign-in (cold caches, first-login setup),
    then again after signing out, which is what every later sign-in costs."""
    user, secret = dev_account()
    budgets = ctx.config["budgets"]
    problems = []
    for label in ("first", "again"):
        if label == "again":
            ctx.write("R08-sign-out.txt",
                      ctx.vm.root_exec(SIGN_OUT.replace("__USER__", shlex.quote(user)), timeout=120))
        steps = sign_in(ctx, user, secret, label)
        res.metrics[label] = steps
        if "usable_s" not in steps:
            raise core.Unobservable(f"{label} sign-in: no first presented frame with a time, or no "
                                    f"answering control socket: {steps}")
        for step in ("shell", "first_frame", "usable"):
            ctx.record(f"session.sign_in_{label}.{step}_s", "s", [steps.get(f"{step}_s")],
                       what=f"greeter sign-in request to {step.replace('_', ' ')}")
        budget = budgets[f"sign_in_{label}_usable_s"]
        if steps["usable_s"] > budget:
            problems.append(f"{label} sign-in took {steps['usable_s']} s to a usable desktop > {budget} s "
                            f"({steps})")
    expect(not problems, "; ".join(problems) + f" [renderer: {ctx.facts.get('renderer')}]")


# -- an idle shared workspace (R09) ------------------------------------------

# A workspace is natively committed only when a relay of another device governs it: one
# of the person's hosts, which may not own workspaces itself (a host that may is a node,
# whose rooms are stewarded, not committed). The run makes one in the guest, as a person
# enrolls a home server: the relay's own key, certified out of band by the slot identity's
# root with the host capability and recorded as its home peer, then `invitation.issue`
# on a fresh workspace, which shares it on that relay and enrolls this device into it.
HOME_RELAY_DIR = "tilo-acceptance-home-relay"
HOME_RELAY_UNIT = "tilo-acceptance-home-relay"
HOME_RELAY_LABEL = "Acceptance home relay"
SHARED_WORKSPACE = "Acceptance shared"
# Two apps that read their records through the broker: their committed reads are what
# the broker holds a wait for.
SHARED_APPS = ("so.tilo.notes", "so.tilo.file-explorer")
# The kernel truncates thread names to 15 bytes: the compositor's collaboration broker
# (`tilo-collab-broker-capnp`) and each app's collaboration shovel (`ui-collab-shovel`).
BROKER_THREAD = "tilo-collab-bro"
SHOVEL_THREAD = "ui-collab-shove"

# As the slot user, in a fresh owner-only directory of its runtime dir: the relay's
# device key, printed as one JSON line with its relay's endpoint id.
HOME_RELAY_KEY = r"""
exec 2>&1
set -eu
d="${XDG_RUNTIME_DIR:?}/__DIR__"
load=$(systemctl --user show __UNIT__.service --property=LoadState --value)
if [ "$load" != not-found ]; then
  systemctl --user stop __UNIT__.service
fi
rm -rf "$d"
mkdir -m 700 "$d"
mkdir -m 700 "$d/identity" "$d/state" "$d/records"
tilo-enroll device-key --identity-root "$d/identity"
"""

# The slot identity's root certifies the relay's key as a host and records it as a home
# peer listening on loopback; the relay installs its certificate and starts as a transient
# unit of the slot user's manager, ready once it has bound its endpoint. A slot without a
# throwaway identity root is refused: this never enrolls a host into a real identity.
HOME_RELAY_START = r"""
exec 2>&1
set -eu
d="${XDG_RUNTIME_DIR:?}/__DIR__"
root="${TILO_IDENTITY_ROOT:-}"
[ -n "$root" ] || { echo "HOME-RELAY no-identity-root"; exit 0; }
export DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-unix:path=$XDG_RUNTIME_DIR/bus}"
tilo-enroll certify --identity-root "$root" --identity-dir "${XDG_CONFIG_HOME:-$HOME/.config}/tilo/identity" \
  --device __DEVICE__ --label __LABEL__ --capability host --capability read-all \
  --home-peer-service __SERVICE__ --home-peer-addr 127.0.0.1:__PORT__ --out "$d/host.cert"
tilo-enroll install-cert --identity-root "$d/identity" --identity-dir "$d/records" --cert "$d/host.cert"
cat > "$d/relay.toml" <<TOML
identity_root = "$d/identity"
state_root = "$d/state"
full_replica_status = "$d/full-replica-status.json"
home_peer_bundle = "$d/home-peer.toml"
iroh_listen = "127.0.0.1:__PORT__"
TOML
systemd-run --user --quiet --collect --unit=__UNIT__ --service-type=notify -p TimeoutStartSec=60 \
  /usr/libexec/tilo/tilo-relay --config "$d/relay.toml" run
echo "HOME-RELAY started"
"""

HOME_RELAY_STOP = r"""
d="${XDG_RUNTIME_DIR:?}/__DIR__"
journalctl --user -u __UNIT__ --no-pager -o short-monotonic 2>/dev/null | tail -n 200 | cut -c1-400
load=$(systemctl --user show __UNIT__.service --property=LoadState --value) || exit $?
if [ "$load" != not-found ]; then
  systemctl --user stop __UNIT__.service || exit $?
fi
rm -rf "$d"
"""

HOME_PEERS = '"${TILO_IDENTITY_ROOT:?}/peers/home-peers.json"'

# What the compositor said about launches, authorship and the native owner while R09 ran:
# an app that never opens in a committed workspace is refused on that path.
SHARED_SESSION_LOG = (
    "sed 's/\\x1b\\[[0-9;]*m//g' \"$HOME/.local/state/tilo/session-logs/latest.log\" 2>/dev/null "
    "| grep -aiE 'launch|authorship|native|membership|owner|custody|refus|identity|enroll|governor' "
    "| tail -n 200 | cut -c1-500; true")


def _guest(script: str, **values) -> str:
    for key, value in {"DIR": HOME_RELAY_DIR, "UNIT": HOME_RELAY_UNIT, **values}.items():
        script = script.replace(f"__{key}__", str(value))
    return script


def home_relay_port(uid: int) -> int:
    """The loopback port the slot's relay listens on: one per slot user, so two slots of a
    shared VM never contend, and below Linux's ephemeral range."""
    return 24000 + uid % 8000


def start_home_relay(ctx) -> dict:
    """Make the slot identity's home relay and start it; return its device key, relay
    endpoint id and port. Raises Unobservable when the guest cannot."""
    out = ctx.vm.exec(_guest(HOME_RELAY_KEY), check=False)
    try:
        key = first_json(out)
        device, service = key["device_public_key"], key["relay_service_node_id"]
    except (VmError, KeyError, TypeError, ValueError) as e:
        raise core.Unobservable(f"tilo-enroll device-key gave no relay key: {out.strip()[-300:]!r}") from e
    try:
        port = home_relay_port(ctx.vm.slot_uid())
        out = ctx.vm.exec(_guest(HOME_RELAY_START, DEVICE=device, SERVICE=service, PORT=port,
                                 LABEL=shlex.quote(HOME_RELAY_LABEL)), timeout=180, check=False)
        ctx.write("R09-home-relay-setup.txt", out)
        if "HOME-RELAY no-identity-root" in out:
            raise core.Unobservable("the slot has no identity root of its own (TILO_IDENTITY_ROOT): "
                                    "R09 enrolls a host only into a throwaway identity")
        if "--home-peer-service" in out and "unexpected argument" in out:
            raise core.Unobservable("the image's tilo-enroll cannot record a home peer out of band "
                                    "(it predates `certify --home-peer-service`)")
        if "HOME-RELAY started" not in out:
            raise core.Unobservable(f"the home relay did not start: {out.strip()[-400:]!r}")
        return {"device": device, "service": service, "port": port}
    except Exception as exc:
        # Certification may already have persisted the home peer before startup fails.
        try:
            stop_home_relay(ctx, device)
        except Exception as cleanup:
            exc.add_note(f"home relay cleanup also failed: {cleanup}")
        raise


def without_home_peer(text: str, device: str) -> str | None:
    """A home-peer registry less `device`'s entry, or None when it has none (or the
    registry is not one): nothing to write back."""
    try:
        peers = json.loads(text)
    except json.JSONDecodeError:
        return None
    if not isinstance(peers, list):
        return None
    kept = [p for p in peers if not (isinstance(p, dict) and p.get("device_node_id") == device)]
    return None if len(kept) == len(peers) else json.dumps(kept, indent=2)


def stop_home_relay(ctx, device: str | None) -> None:
    """Stop the relay, keep its journal as evidence, and remove its home-peer entry so a
    slot that runs the suite again does not keep dialling relays that are gone."""
    ctx.write("R09-home-relay.log", ctx.vm.exec(_guest(HOME_RELAY_STOP)))
    if device is None:
        return
    text = ctx.vm.exec(f"cat {HOME_PEERS} 2>/dev/null; true", check=False)
    rest = without_home_peer(text, device)
    if rest is not None:
        blob = base64.b64encode(rest.encode()).decode()
        ctx.vm.exec(f"f={HOME_PEERS}; echo {blob} | base64 -d > \"$f.acc-tmp\" && "
                    "chmod 644 \"$f.acc-tmp\" && mv -f \"$f.acc-tmp\" \"$f\"")


def committed_workspace(state: dict, host_device: str) -> dict | None:
    """The workspace whose room this device's native owner holds under `host_device`'s
    governance, when that governor may not own workspaces: a room whose app records the
    owner commits (shell `commits_native_documents`), not one whose channels a node
    stewards. None while the enrollment is pending or the room is another's."""
    tree = state["workspace_tree"]
    rooms = (tree.get("native_authority") or {}).get("rooms") or {}
    want = bytes.fromhex(host_device)
    for w in tree["workspaces"]:
        stored = rooms.get(str(w.get("room")))
        governor = (stored or {}).get("governor") or {}
        if bytes(governor.get("device_public_key") or []) != want:
            continue
        caps = governor.get("capabilities") or {}
        if not (caps.get("can_own_workspaces") and caps.get("can_act_as_host") and not caps.get("foreign")):
            return w
    return None


def describe_native_rooms(state: dict) -> str:
    """The native owner's rooms and pending enrollments, for a failure message."""
    authority = state["workspace_tree"].get("native_authority") or {}
    rooms, pending = authority.get("rooms") or {}, authority.get("pending") or {}
    governed = [f"{room} (governor {bytes((r.get('governor') or {}).get('device_public_key') or []).hex()[:12]})"
                for room, r in rooms.items()]
    return f"rooms: {', '.join(governed) or 'none'}; pending: {', '.join(pending) or 'none'}"


def invoke_settled(ctx, provider: str, action: str, args: dict, timeout: float = 90):
    """Invoke `action` and follow it until it settles; its output when it succeeded."""
    reply = ctx.vm.control_json("actions", "invoke", provider, action, "--args-json", json.dumps(args),
                                "--wait=30000", timeout=60)
    deadline = time.monotonic() + timeout
    while True:
        receipt = (reply or {}).get("receipt") or {}
        update = receipt.get("update") or {}
        state = update.get("state")
        if state == "succeeded":
            return update.get("output")
        if state in ("failed", "cancelled"):
            raise core.CheckFailed(f"{action} {state}: {json.dumps(update)[:400]}")
        job = receipt.get("broker_invocation_id")
        if job is None or time.monotonic() > deadline:
            raise core.CheckFailed(f"{action} did not settle within {timeout:.0f} s: {json.dumps(update)[:400]}")
        reply = ctx.vm.control_json("actions", "status", str(job), "--wait=30000", timeout=60)


def _select_workspace(ctx, wid) -> None:
    if ctx.state()["workspace_tree"]["active"] != wid:
        room = next(w["room"] for w in ctx.state()["workspace_tree"]["workspaces"] if w["id"] == wid)
        ctx.vm.invoke("actions", f"workspace.select.{room}")
        ctx.wait_state(lambda s: s["workspace_tree"]["active"] == wid, 8, f"workspace {wid} active")


def share_new_workspace(ctx, host_device: str) -> dict:
    """Create a workspace, make it active and invite into it, which shares it on the home
    relay; return it once this device's enrollment into it is committed."""
    before = {w["id"] for w in ctx.state()["workspace_tree"]["workspaces"]}
    ctx.vm.invoke("actions", "workspace.create", {"name": SHARED_WORKSPACE})
    st = ctx.wait_state(lambda s: [w for w in s["workspace_tree"]["workspaces"] if w["id"] not in before],
                        8, "a new workspace")
    _select_workspace(ctx, next(w for w in st["workspace_tree"]["workspaces"] if w["id"] not in before)["id"])
    # Shared on the relay: nobody redeems the token, and it is never written down.
    invoke_settled(ctx, "actions", "invitation.issue",
                   {"role": "viewer", "expires_in_seconds": 3600, "confirm": True})
    last = {}

    def committed():
        last["s"] = ctx.state()
        return committed_workspace(last["s"], host_device)

    shared = wait_until(committed, 60, 1.0)
    if shared is None:
        raise core.CheckFailed("the invited workspace was not committed under the home relay within "
                               f"60 s ({describe_native_rooms(last['s'])})")
    _select_workspace(ctx, shared["id"])
    return shared


def shared_idle(m: dict, budgets: dict) -> tuple[dict, list[str]]:
    """The compositor's broker threads and the apps' shovels in an idle shared workspace,
    against `idle_shared_{broker,shovel}_wakeups_per_s`. Unobservable without either: a
    desktop with no broker or no app shovel did not hold the waits this measures."""
    pid, rows = m["compositor_pid"], m.get("named_threads") or []
    broker = [r for r in rows if r["pid"] == pid and r["thread"] == BROKER_THREAD]
    shovels = [r for r in rows if r["pid"] != pid and r["thread"] == SHOVEL_THREAD]
    if not broker:
        raise core.Unobservable(f"the compositor ran no {BROKER_THREAD} thread in the shared workspace")
    if not shovels:
        raise core.Unobservable(f"no app ran a {SHOVEL_THREAD} thread in the shared workspace")
    out = {"broker_wakeups_per_s": round(sum(r["ctxt_switches_per_s"] for r in broker), 2),
           "broker_threads": sum(r["threads"] for r in broker),
           "shovel_wakeups_per_s": round(sum(r["ctxt_switches_per_s"] for r in shovels), 2),
           "shovels": sum(r["threads"] for r in shovels),
           "shovel_processes": sorted({r["process"] or "?" for r in shovels})}
    problems = []
    if out["broker_wakeups_per_s"] > budgets["idle_shared_broker_wakeups_per_s"]:
        problems.append(f"the compositor's collaboration broker woke {out['broker_wakeups_per_s']}/s > "
                        f"{budgets['idle_shared_broker_wakeups_per_s']}/s in an idle shared workspace "
                        "(a held read re-checked on a clock rather than on announcements)")
    if out["shovel_wakeups_per_s"] > budgets["idle_shared_shovel_wakeups_per_s"]:
        woke = ", ".join(f"{r['process']}[{r['pid']}] {r['ctxt_switches_per_s']}/s"
                         for r in sorted(shovels, key=lambda r: -r["ctxt_switches_per_s"])[:4]
                         if r["ctxt_switches_per_s"] > 0)
        problems.append(f"the apps' collaboration shovels woke {out['shovel_wakeups_per_s']}/s > "
                        f"{budgets['idle_shared_shovel_wakeups_per_s']}/s ({woke})")
    return out, problems


@check("R09", AREA, "Idle in a shared workspace: the broker and the apps' shovels sleep until told")
def idle_shared(ctx, res):
    """A natively committed workspace, made by the real enrollment, with Notes and Files
    open and nothing edited: the broker holds each committed read's wait and re-checks it
    only when an identity or membership input announces a change, and the apps'
    collaboration shovels have nothing to carry."""
    ctx.reset()
    home = ctx.state()["workspace_tree"]["active"]
    device = None
    try:
        relay = start_home_relay(ctx)
        device = relay["device"]
        res.metrics["home_relay"] = relay
        shared = share_new_workspace(ctx, device)
        res.metrics["workspace"] = {"id": shared["id"], "room": shared.get("room")}
        opened = _open_several(ctx, SHARED_APPS, "R09")
        res.metrics["opened"] = opened
        here = next(w for w in ctx.state()["workspace_tree"]["workspaces"] if w["id"] == shared["id"])
        groups = [g for s in here["strips"] for g in s["window_groups"]]
        failed = ctx.facts.get("apps_failed_to_open") or {}
        if failed:
            res.metrics["failed_to_open"] = failed
            ctx.write("R09-session.log", ctx.vm.exec(SHARED_SESSION_LOG, check=False))
        expect(len(opened) == len(SHARED_APPS) and len(groups) >= len(SHARED_APPS),
               f"{len(opened)} of {len(SHARED_APPS)} apps stayed open in the shared workspace "
               f"({opened}; {len(groups)} window groups there; never opened: {failed or 'none'})")
        m = measure(ctx, ctx.config["idle_seconds"], "R09", threads=(BROKER_THREAD, SHOVEL_THREAD))
        res.metrics.update(m, renderer=ctx.facts.get("renderer"))
        ctx.write("R09-idle-shared.json", json.dumps(m, indent=1))
        judged, problems = shared_idle(m, ctx.config["budgets"])
        res.metrics["shared"] = judged
        window = {"window_s": m["seconds"]}
        ctx.record("idle.wakeups_per_s.shared_broker", "per_s", [judged["broker_wakeups_per_s"]],
                   threads=judged["broker_threads"], **window)
        ctx.record("idle.wakeups_per_s.shared_shovels", "per_s", [judged["shovel_wakeups_per_s"]],
                   shovels=judged["shovels"], apps=",".join(opened), **window)
        expect(not problems, "; ".join(problems))
    finally:
        _leave_shared(ctx, res, home, device)


def _leave_shared(ctx, res, home, device: str | None) -> None:
    """Close R09's windows, return to the workspace the run was in and stop the relay.
    Every step runs whatever the one before did; what failed is kept in the metrics."""
    errors = []
    for step in (ctx.reset, lambda: _select_workspace(ctx, home), lambda: stop_home_relay(ctx, device)):
        try:
            step()
        except Exception as e:  # noqa: BLE001 - every cleanup step must run
            errors.append(str(e)[:300])
    if errors:
        res.metrics.setdefault("cleanup_errors", []).extend(errors)
        # Keep the original failure when unwinding, but never pass dirty cleanup.
        if sys.exception() is None:
            raise core.CheckFailed("shared workspace cleanup failed: " + "; ".join(errors))

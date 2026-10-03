"""Checks, their results, and the facts every area reads off a state dump."""

from __future__ import annotations

import time
import traceback
from dataclasses import dataclass, field
from pathlib import Path

# Who fixes what: each area's failures are routed to its owner.
AREAS = {
    "provenance": "Integrator / local install",
    "window": "Window management, animations and gestures",
    "ui": "UI consistency",
    "apps": "App lifecycle",
    "search": "Search and history",
    "resources": "Resources",
    "basics": "Basics: hardware controls, keyboard, displays",
}


class CheckFailed(Exception):
    """The subject was observed and is wrong."""


class Unobservable(CheckFailed):
    """The check could not observe its subject. Reported as a failure, never a skip."""


@dataclass
class Result:
    id: str
    area: str
    title: str
    status: str = "fail"
    detail: str = ""
    metrics: dict = field(default_factory=dict)
    evidence: list = field(default_factory=list)
    seconds: float = 0.0
    unobservable: bool = False


@dataclass
class Check:
    id: str
    area: str
    title: str
    func: object
    needs: tuple = ()


REGISTRY: list[Check] = []


def check(id: str, area: str, title: str, needs: tuple = ()):
    """Register a check. The function gets (ctx, result) and raises to fail."""
    assert area in AREAS, area

    def wrap(func):
        REGISTRY.append(Check(id, area, title, func, needs))
        return func

    return wrap


def run_check(ctx, chk: Check) -> Result:
    res = Result(chk.id, chk.area, chk.title)
    started = time.monotonic()
    ctx.current = res
    try:
        for need in chk.needs:
            if not ctx.facts.get(need):
                raise Unobservable(f"precondition `{need}` not met: "
                                   f"{ctx.facts.get(need + ':why', 'an earlier check failed')}")
        chk.func(ctx, res)
        res.status = "pass"
    except Unobservable as exc:
        res.status, res.unobservable, res.detail = "fail", True, _join(res.detail, str(exc))
    except CheckFailed as exc:
        res.status, res.detail = "fail", _join(res.detail, str(exc))
    except Exception as exc:  # noqa: BLE001 - any error is a failed check, with its trace
        res.status = "fail"
        res.unobservable = True
        res.detail = _join(res.detail, f"{type(exc).__name__}: {exc}")
        (ctx.out / "errors").mkdir(exist_ok=True)
        (ctx.out / "errors" / f"{chk.id}.txt").write_text(traceback.format_exc())
    res.seconds = round(time.monotonic() - started, 2)
    ctx.current = None
    return res


def _join(a: str, b: str) -> str:
    return f"{a}; {b}" if a else b


def expect(cond, message: str):
    if not cond:
        raise CheckFailed(message)


# -- facts off a state dump ----------------------------------------------------


def active_workspace(state: dict) -> dict:
    tree = state["workspace_tree"]
    return next(w for w in tree["workspaces"] if w["id"] == tree["active"])


def active_strip(state: dict) -> dict:
    tree = state["workspace_tree"]
    ws = active_workspace(state)
    sid = tree["active_strip"].get(str(ws["id"]))
    return next((s for s in ws["strips"] if s["id"] == sid), ws["strips"][0])


def groups(state: dict) -> list[dict]:
    return active_strip(state)["window_groups"]


def all_groups(state: dict) -> list[dict]:
    return [g for w in state["workspace_tree"]["workspaces"]
            for s in w["strips"] for g in s["window_groups"]]


def active_group_id(state: dict):
    strip = active_strip(state)
    return state["workspace_tree"]["active_group"].get(str(strip["id"]))


def group_by_id(state: dict, gid):
    return next((g for g in all_groups(state) if g["id"] == gid), None)


def tab_app(tab: dict) -> str | None:
    tile = tab.get("tile") or {}
    if isinstance(tile, str):
        return tile
    if "Foreign" in tile:
        return tile["Foreign"].get("app_id")
    if "AppPlaceholder" in tile:
        return "placeholder:" + str(tile["AppPlaceholder"].get("kind"))
    return next(iter(tile), None)


def overlays(state: dict) -> dict:
    return state["chrome"]["overlays"]


def display_of_group(state: dict, gid):
    for d in state["chrome"]["displays"]:
        if gid in d.get("groups", []):
            return d["id"]
    return None


def display_rects(state: dict) -> dict:
    return {d["id"]: (d["x"], d["y"], d["width"], d["height"])
            for d in state["workspace_tree"]["outputs"]["displays"]}


def target_map(targets: list[dict]) -> dict:
    return {t["name"]: tuple(t["rect"]) for t in targets}


def rect_inside(inner, outer, slack: float = 1.0) -> bool:
    x, y, w, h = inner
    ox, oy, ow, oh = outer
    return (x >= ox - slack and y >= oy - slack
            and x + w <= ox + ow + slack and y + h <= oy + oh + slack)


def overlap(a, b) -> float:
    ax, ay, aw, ah = a
    bx, by, bw, bh = b
    w = min(ax + aw, bx + bw) - max(ax, bx)
    h = min(ay + ah, by + bh) - max(ay, by)
    return w * h if w > 0 and h > 0 else 0.0


# -- the session's frame trace (TILO_FRAME_TRACE, set by tilo-session) ---------

TRACE = "$HOME/.local/state/tilo/session-logs/frame-trace.csv"


def read_trace(ctx) -> list[dict]:
    text = ctx.vm.exec(f'cat "{TRACE}"', timeout=60)
    lines = [l for l in text.splitlines() if l and not l.startswith("receipt:")]
    if not lines or not lines[0].startswith("seq,"):
        raise Unobservable("the session has no frame trace (TILO_FRAME_TRACE)")
    header = lines[0].split(",")
    rows = []
    for line in lines[1:]:
        cells = line.split(",")
        if len(cells) != len(header):
            continue
        rows.append({k: (int(v) if v.isdigit() else v) for k, v in zip(header, cells)})
    return rows


def trace_since(rows: list[dict], start_us: int) -> list[dict]:
    return [r for r in rows if r["start_us"] >= start_us]


def trace_now_us(ctx) -> int:
    rows = read_trace(ctx)
    return rows[-1]["start_us"] if rows else 0


def percentile(values: list, pct: float):
    if not values:
        return None
    s = sorted(values)
    k = max(0, min(len(s) - 1, int(round(pct / 100 * len(s) + 0.5)) - 1))
    return s[k]


def frame_stats(rows: list[dict]) -> dict:
    """Paint cadence and CPU work of the painted ticks in `rows`."""
    painted = [r for r in rows if r.get("painted") == 1]
    work = []
    prev_sleep = None
    for r in rows:
        if prev_sleep is not None and r.get("painted") == 1:
            work.append(max(0, r["period_us"] - prev_sleep))
        prev_sleep = r["sleep_us"]
    periods = [r["period_us"] for r in painted if r["period_us"] > 0]
    latency = [r["input_latency_us"] for r in rows if isinstance(r.get("input_latency_us"), int)]
    ms = lambda v: None if v is None else round(v / 1000, 2)  # noqa: E731
    return {
        "ticks": len(rows),
        "painted": len(painted),
        "work_ms_p50": ms(percentile(work, 50)),
        "work_ms_p95": ms(percentile(work, 95)),
        "work_ms_p99": ms(percentile(work, 99)),
        "paint_period_ms_p50": ms(percentile(periods, 50)),
        "paint_period_ms_p95": ms(percentile(periods, 95)),
        "paint_period_ms_max": ms(max(periods) if periods else None),
        "input_to_submit_ms_p50": ms(percentile(latency, 50)),
        "input_to_submit_ms_p95": ms(percentile(latency, 95)),
        "backpressured": sum(1 for r in rows if r.get("backpressured") == 1),
    }


def burst_stats(rows: list[dict], window_ms: float, idle_ms: float = 250.0) -> dict:
    """Pacing inside animations: a burst starts at the first painted frame after at least
    `idle_ms` without one and spans `window_ms` (the animation's length). Intervals are
    painted-to-painted, so the idle park before a burst never counts as a stall."""
    painted = [r["start_us"] for r in rows if r.get("painted") == 1]
    late = [r["deadline_late_us"] for r in rows
            if r.get("painted") == 1 and isinstance(r.get("deadline_late_us"), int)]
    bursts, cur, start, prev = [], [], None, None
    for t in painted:
        if prev is None or t - prev > idle_ms * 1000:
            if cur:
                bursts.append(cur)
            cur, start = [t], t
        elif t - start <= window_ms * 1000:
            cur.append(t)
        prev = t
    if cur:
        bursts.append(cur)
    gaps = [b - a for burst in bursts for a, b in zip(burst, burst[1:])]
    ms = lambda v: None if v is None else round(v / 1000, 2)  # noqa: E731
    return {
        "bursts": len(bursts),
        "frames_per_burst": [len(b) for b in bursts],
        "interval_ms_p50": ms(percentile(gaps, 50)),
        "interval_ms_p95": ms(percentile(gaps, 95)),
        "interval_ms_max": ms(max(gaps) if gaps else None),
        "deadline_late_ms_p95": ms(percentile(late, 95)),
    }


# -- pixels (Pillow is optional on the host; without it pixel checks fail) -----


def load_image(path: Path):
    try:
        from PIL import Image  # type: ignore
    except ImportError as exc:
        raise Unobservable("Pillow is not installed on the host, so pixels cannot be read") from exc
    return Image.open(path).convert("RGB")


def region_stats(img, rect) -> dict:
    """Mean luminance (0-1) and luminance spread of `rect` (x, y, w, h)."""
    from PIL import ImageStat  # type: ignore

    x, y, w, h = (int(round(v)) for v in rect)
    x, y = max(0, x), max(0, y)
    crop = img.crop((x, y, min(img.width, x + max(1, w)), min(img.height, y + max(1, h)))).convert("L")
    st = ImageStat.Stat(crop)
    return {"mean": round(st.mean[0] / 255, 3), "stddev": round(st.stddev[0] / 255, 3)}


def region_diff(a, b, rect) -> float:
    """Mean absolute luminance difference (0-1) between two images inside `rect`."""
    from PIL import ImageChops, ImageStat  # type: ignore

    x, y, w, h = (int(round(v)) for v in rect)
    box = (max(0, x), max(0, y), min(a.width, x + w), min(a.height, y + h))
    diff = ImageChops.difference(a.crop(box).convert("L"), b.crop(box).convert("L"))
    return round(ImageStat.Stat(diff).mean[0] / 255, 4)


def save_thumb(src: Path, dst: Path, width: int = 1600) -> Path:
    img = load_image(src)
    if img.width > width:
        img = img.resize((width, int(img.height * width / img.width)))
    img.save(dst)
    return dst


# -- processes, the app each belongs to, and its cgroup --------------------------

# Read as root: the compositor is not dumpable. One line per process with its parent,
# owner, RSS, anonymous RSS, cgroup and, for `tilo-launcher`, the `--app-id` it runs;
# then one line per cgroup seen with memory.current and the memory.stat anon/file split.
PROCESSES = r'''
cgs=""
for d in /proc/[0-9]*; do
  st=$(awk '/^PPid:/ {p=$2} /^Uid:/ {u=$2} /^VmRSS:/ {r=$2} /^RssAnon:/ {a=$2}
            END {if (r) print p, u, r, a+0}' $d/status 2>/dev/null)
  [ -n "$st" ] || continue
  comm=$(tr ' \t' '__' 2>/dev/null < $d/comm)
  cg=$(sed -n "s/^0:://p" 2>/dev/null < $d/cgroup)
  app=""
  [ "$comm" = tilo-launcher ] && \
    app=$(tr '\0' '\n' 2>/dev/null < $d/cmdline | awk 'p {print; exit} $0 == "--app-id" {p = 1}')
  set -- $st
  printf 'P\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "${d#/proc/}" "$1" "$2" "$comm" "$3" "$4" "${app:--}" "$cg"
  cgs="$cgs$cg
"
done
printf '%s' "$cgs" | sort -u | while IFS= read -r cg; do
  f="/sys/fs/cgroup$cg"
  if [ -r "$f/memory.current" ]; then
    printf 'C\t%s\t%s\t%s\n' "$cg" "$(cat "$f/memory.current")" \
      "$(awk '$1 == "anon" {a=$2} $1 == "file" {f=$2} END {print a+0 "\t" f+0}' "$f/memory.stat")"
  else
    printf 'C\t%s\t-\t-\t-\n' "$cg"
  fi
done
awk '/MemTotal|MemAvailable/ {print "M", $1, $2}' /proc/meminfo
'''


def _mb(kib_or_bytes: str, unit: int) -> float | None:
    return None if kib_or_bytes == "-" else round(int(kib_or_bytes) * unit / 2**20, 1)


def parse_processes(text: str) -> dict:
    """`PROCESSES` output -> {"procs": {pid: ...}, "cgroups": {path: ...}, "meminfo_mb": {...}}.

    Each process carries `app`: the `--app-id` of the nearest `tilo-launcher` at or above it,
    so an app is its launcher's whole process tree, whatever its processes are called."""
    procs, cgroups, mem = {}, {}, {}
    for line in text.splitlines():
        f = line.split("\t")
        if f[0] == "P" and len(f) == 9 and f[1].isdigit():
            procs[int(f[1])] = {"pid": int(f[1]), "ppid": int(f[2]), "uid": int(f[3]), "comm": f[4],
                                "rss_mb": _mb(f[5], 1024), "anon_rss_mb": _mb(f[6], 1024),
                                "launches": None if f[7] == "-" else f[7], "cgroup": f[8]}
        elif f[0] == "C" and len(f) == 5:
            cgroups[f[1]] = {"memory_mb": _mb(f[2], 1), "anon_mb": _mb(f[3], 1), "file_mb": _mb(f[4], 1)}
        elif line.startswith("M "):
            _, key, kib = line.split()[:3]
            mem[key.rstrip(":")] = int(kib) // 1024
    for p in procs.values():
        cur, seen = p, set()
        while cur and cur["launches"] is None and cur["pid"] not in seen:
            seen.add(cur["pid"])
            cur = procs.get(cur["ppid"])
        p["app"] = cur["launches"] if cur else None
    return {"procs": procs, "cgroups": cgroups, "meminfo_mb": mem}


def processes(ctx) -> dict:
    return parse_processes(ctx.vm.root_exec(PROCESSES))


def app_cgroup(table: dict, app_id: str) -> dict:
    """Where `app_id`'s processes are accounted: its cgroups, and whether they hold the app alone.

    A cgroup that also holds a process outside the app's launcher tree (the compositor, a
    session service, another app) is shared: its memory is not this app's."""
    mine = [p for p in table["procs"].values() if p["app"] == app_id]
    paths = sorted({p["cgroup"] for p in mine})
    strangers = {path: sorted({p["comm"] for p in table["procs"].values()
                               if p["cgroup"] == path and p["app"] != app_id})
                 for path in paths}
    return {"pids": sorted(p["pid"] for p in mine), "cgroups": paths,
            "shared_with": {k: v for k, v in strangers.items() if v},
            "anon_rss_mb": round(sum(p["anon_rss_mb"] or 0 for p in mine), 1)}

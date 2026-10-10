"""Checks, their results, and the facts every area reads off a state dump."""

from __future__ import annotations

import re
import time
import traceback
from dataclasses import dataclass, field
from pathlib import Path

from .vm import VmError

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
    leave_unlocked(ctx, chk, res)
    res.seconds = round(time.monotonic() - started, 2)
    ctx.current = None
    return res


LEFT_LOCKED = "left the session locked"


def leave_unlocked(ctx, chk: Check, res: Result) -> None:
    """Every check runs in the one session the run shares, so none may hand the next a locked
    one: while locked the compositor refuses every action and snapshot (on purpose), and every
    later check would report `session_locked` instead of its subject. A check that ends with
    the session locked fails, however it ended, and the harness restarts the slot's session
    (a fresh compositor starts unlocked; nothing goes around the lock itself)."""
    try:
        locked = ctx.state()["chrome"].get("session_locked")
    except (VmError, KeyError, TypeError):
        return  # no state to read: the next check that needs one reports why
    if not locked:
        return
    res.status = "fail"
    ctx.facts.setdefault("session_recovered_after", []).append(chk.id)
    try:
        ctx.vm.restart_session()
    except VmError as exc:
        res.detail = _join(res.detail, f"{LEFT_LOCKED}, and restarting the slot session failed: "
                                       f"{exc.reason()}")
        return
    res.detail = _join(res.detail, f"{LEFT_LOCKED}; the harness restarted the slot session")


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
#
# One row per loop tick. The recorder buffers rows and writes them out on the first
# tick that ends a second or more after its last write, and a tick's row is only
# written once its park ends. Read straight after a transition, the file lacks the
# transition's tail; read before one, its last row names a moment long gone. So a
# window is cut by `seq` between two synced reads, never by a guessed clock.

TRACE = "$HOME/.local/state/tilo/session-logs/frame-trace.csv"
TRACE_FLUSH_S = 1.05
SESSION_LOG = "$HOME/.local/state/tilo/session-logs/latest.log"

# The compositor's own timing of each workspace switch
# (crates/session/shell/src/compositor/switch_latency.rs), one session-log line each.
SWITCH_FIELDS = ("first_queued_us", "first_shown_us", "settled_queued_us", "settled_shown_us",
                 "frames", "dropped", "interval_us")
SWITCH_LINE = re.compile(r"(?:^|\s)WORKSPACE-SWITCH ((?:[a-z_]+=[0-9]+ ?)+)$")


def parse_switch_lines(text: str) -> list[dict]:
    """Every `WORKSPACE-SWITCH` line in `text`, in order."""
    samples = []
    for line in text.splitlines():
        match = SWITCH_LINE.search(line.rstrip())
        if match is None:
            continue
        fields = dict(pair.split("=", 1) for pair in match.group(1).split())
        if any(name not in fields for name in SWITCH_FIELDS):
            raise Unobservable(f"a WORKSPACE-SWITCH line lacks fields: {line.strip()}")
        samples.append({name: int(fields[name]) for name in SWITCH_FIELDS})
    return samples


def session_log_mark(ctx) -> int:
    """Lines in the session log now; `switch_lines_after` reads past them."""
    out = ctx.vm.exec(f'wc -l < "{SESSION_LOG}" 2>/dev/null || echo 0', check=False)
    digits = [l.strip() for l in out.splitlines() if l.strip().isdigit()]
    return int(digits[0]) if digits else 0


def switch_lines_after(ctx, mark: int) -> list[dict]:
    return parse_switch_lines(ctx.vm.exec(
        f'tail -n +{mark + 1} "{SESSION_LOG}" 2>/dev/null | grep -a "WORKSPACE-SWITCH"; true', check=False))


def _parse_trace(text: str) -> tuple[list[dict], str]:
    lines = [l for l in text.splitlines() if l and not l.startswith("receipt:")]
    if not lines or not lines[0].startswith("seq,"):
        raise Unobservable("the session has no frame trace (TILO_FRAME_TRACE)")
    header = lines[0].split(",")
    rows, last = [], ""
    for line in lines[1:]:
        if line.startswith("LASTSEQ:"):
            last = line[len("LASTSEQ:"):]
            continue
        cells = line.split(",")
        if len(cells) == len(header):
            rows.append({k: (int(v) if v.isdigit() else v) for k, v in zip(header, cells)})
    return rows, last


def read_trace(ctx, after_seq: int = -1) -> list[dict]:
    """Rows with seq > `after_seq`, complete lines only (a buffered write can end
    mid-line). A trace that started over under the window fails it as unobservable."""
    script = (f'f="{TRACE}"; [ -s "$f" ] || exit 0; head -n1 "$f"; part=$(tail -c1 "$f"); '
              f'awk -F, -v m={int(after_seq)} -v part="$part" '
              "'NR>1 {before=seq; seq=$1} NR>1 && $1+0>m {if (have) print prev; prev=$0; have=1} "
              "END {if (have && part==\"\") print prev; print \"LASTSEQ:\" (part==\"\" ? seq : before)}' \"$f\"")
    rows, last = _parse_trace(ctx.vm.exec(script, timeout=60))
    if after_seq >= 0 and last.isdigit() and int(last) < after_seq:
        raise Unobservable("the compositor restarted inside the measured window "
                           f"(its frame trace started over, now at seq {last})")
    return rows


def trace_csv(rows: list[dict]) -> str:
    """Rows back as CSV, with their header, for evidence."""
    if not rows:
        return ""
    return "\n".join([",".join(rows[0])] + [",".join(str(v) for v in r.values()) for r in rows]) + "\n"


def sync_trace(ctx) -> None:
    """Get every tick that began before this call onto disk: wake the loop through the
    control socket, wait out the flush interval, wake it twice more. The second wake's
    tick began a flush interval after the first, so the third wake, which ends it,
    writes everything out."""
    ctx.vm.state()
    time.sleep(TRACE_FLUSH_S)
    ctx.vm.state()
    ctx.vm.state()


def trace_mark(ctx) -> int:
    """The seq a window starts after: the newest row of the synced trace."""
    sync_trace(ctx)
    script = f'f="{TRACE}"; [ -s "$f" ] || exit 0; head -n1 "$f"; tail -n4 "$f" | head -n3'
    rows, _ = _parse_trace(ctx.vm.exec(script, timeout=60))
    return rows[-1]["seq"] if rows else -1


def trace_after(ctx, mark: int) -> list[dict]:
    """Every tick after `mark`, synced first; the sync's own wakes add two or three
    unpainted ticks at the end."""
    sync_trace(ctx)
    return read_trace(ctx, mark)


def demanding(r: dict) -> bool:
    """The compositor had a frame to show on this tick: it painted one, or the
    presenter still held earlier ones (backpressured)."""
    return r.get("painted") == 1 or r.get("backpressured") == 1


def trace_until_quiet(ctx, mark: int, quiet_ms: float = 500,
                      timeout_s: float = 20) -> tuple[list[dict], bool]:
    """Ticks after `mark` once the compositor has asked for no frame for `quiet_ms` of
    its own time. While it animates the control socket is left alone (a state dump
    costs the loop tens of milliseconds): the file grows by a write a second while
    frames flow, and stops growing when the loop parks. Returns the rows and whether
    the compositor went quiet before `timeout_s`."""
    deadline = time.monotonic() + timeout_s
    while True:
        seen, still = None, 0.0
        while time.monotonic() < deadline and still < 1.2:
            n = len(read_trace(ctx, mark))
            still = still + 0.4 if n == seen else 0.0
            seen = n
            time.sleep(0.4)
        rows = trace_after(ctx, mark)
        want = [r for r in rows if demanding(r)]
        quiet = bool(rows) and (not want or rows[-1]["start_us"] - want[-1]["start_us"] >= quiet_ms * 1000)
        if quiet or time.monotonic() >= deadline:
            return rows, quiet


def percentile(values: list, pct: float):
    if not values:
        return None
    s = sorted(values)
    k = max(0, min(len(s) - 1, int(round(pct / 100 * len(s) + 0.5)) - 1))
    return s[k]


def _ms(v):
    return None if v is None else round(v / 1000, 2)


def has_cpu(rows: list[dict]) -> bool:
    """Whether the trace carries the loop thread's CPU time per tick."""
    return any(isinstance(r.get("cpu_us"), int) for r in rows)


def _tick_walls(rows: list[dict]):
    """(tick, wall time outside its park) for each tick with a successor: a row's
    `period_us` runs from the previous tick's start, so it holds the previous tick's
    work and park."""
    for a, b in zip(rows, rows[1:]):
        yield a, max(0, b["period_us"] - a["sleep_us"])


def frame_stats(rows: list[dict]) -> dict:
    """Paint cadence and work of the painted ticks in `rows`.

    `cpu_ms_*` is the CPU the loop thread spent on a painted tick: the compositor's
    own work, whatever the host or the display does meanwhile. `work_ms_*` is the
    same tick's wall time outside the park; the difference is time the thread was
    descheduled or blocked."""
    painted = [r for r in rows if r.get("painted") == 1]
    work = [wall for r, wall in _tick_walls(rows) if r.get("painted") == 1]
    cpu = [r["cpu_us"] for r in painted if isinstance(r.get("cpu_us"), int)]
    periods = [r["period_us"] for r in painted if r["period_us"] > 0]
    latency = [r["input_latency_us"] for r in rows if isinstance(r.get("input_latency_us"), int)]
    blocked = [r for r in rows if r.get("backpressured") == 1]
    return {
        "ticks": len(rows),
        "painted": len(painted),
        "cpu_ms_p50": _ms(percentile(cpu, 50)),
        "cpu_ms_p95": _ms(percentile(cpu, 95)),
        "cpu_ms_p99": _ms(percentile(cpu, 99)),
        "cpu_ms_max": _ms(max(cpu) if cpu else None),
        "work_ms_p50": _ms(percentile(work, 50)),
        "work_ms_p95": _ms(percentile(work, 95)),
        "work_ms_p99": _ms(percentile(work, 99)),
        "paint_period_ms_p50": _ms(percentile(periods, 50)),
        "paint_period_ms_p95": _ms(percentile(periods, 95)),
        "paint_period_ms_max": _ms(max(periods) if periods else None),
        "input_to_submit_ms_p50": _ms(percentile(latency, 50)),
        "input_to_submit_ms_p95": _ms(percentile(latency, 95)),
        "backpressured": len(blocked),
        "display_wait_ms": _ms(sum(r["sleep_us"] for r in blocked)),
    }


def _to_submit_us(r: dict) -> int:
    """How far into its tick a frame was submitted."""
    return sum(r.get(k) or 0 for k in ("input_us", "pump_us", "layout_us", "scene_us", "submit_us")
               if isinstance(r.get(k), int))


def transition_stats(rows: list[dict]) -> dict:
    """One transition, from the first tick that wanted a frame to the last.

    The compositor's side: how many frames it painted (a transition held while the
    presenter is full paints every step, however slowly the display takes them), how
    late each was against its own deadline, the CPU each tick took, and how long the
    first frame took after the input once time spent waiting on a full presenter is
    taken out. The display's side: painted-to-painted intervals and the time frames
    waited for it."""
    first = next((i for i, r in enumerate(rows) if demanding(r)), None)
    rows = rows[first:] if first is not None else []
    last = max((i for i, r in enumerate(rows) if demanding(r)), default=-1)
    # One tick past the last, which closes the last tick's period.
    walls = list(_tick_walls(rows[:last + 2]))
    rows = rows[:last + 1]
    painted = [r for r in rows if r.get("painted") == 1]
    blocked = [r for r in rows if r.get("backpressured") == 1]
    gaps = [b["start_us"] - a["start_us"] for a, b in zip(painted, painted[1:])]
    # Lateness against the compositor's own deadline, but never counted from before the
    # frame could be made: the clock moves on when the display retires a frame, so a
    # frame painted before that carries a deadline from before the previous frame. From
    # whichever is later, the previous submit or the end of the last park behind a full
    # presenter, the rest is the compositor's. The first frame is judged as input latency.
    late = []
    prev_submit, ready = None, None
    for x, nxt in zip(rows, rows[1:] + [None]):
        if x.get("painted") == 1 and isinstance(x.get("deadline_late_us"), int):
            submit = x["start_us"] + _to_submit_us(x)
            if prev_submit is not None:
                late.append(max(0, min(x["deadline_late_us"], submit - max(prev_submit, ready or 0))))
            prev_submit, ready = submit, None
        elif x.get("backpressured") == 1 and nxt is not None:
            ready = nxt["start_us"]
    cpu = [r["cpu_us"] for r in rows if isinstance(r.get("cpu_us"), int)]
    # The longest tick by wall time outside its park, and how much of that was CPU.
    longest = max(((wall, r) for r, wall in walls), key=lambda t: t[0], default=(None, {}))
    input_ms = input_compositor_ms = None
    for i, r in enumerate(rows):
        if r.get("painted") == 1 and isinstance(r.get("input_latency_us"), int):
            # Parks behind a full presenter between the input and this frame's submit; a
            # tick's park ends where the next tick starts.
            submitted = r["start_us"] + _to_submit_us(r)
            arrived = submitted - r["input_latency_us"]
            waited = 0
            for x, nxt in zip(rows[:i], rows[1:i + 1]):
                if x.get("backpressured") == 1:
                    end = nxt["start_us"]
                    waited += max(0, min(end, submitted) - max(end - x["sleep_us"], arrived))
            input_ms = _ms(r["input_latency_us"])
            input_compositor_ms = _ms(max(0, r["input_latency_us"] - waited))
            break
    return {
        "frames": len(painted),
        "blocked_ticks": len(blocked),
        "span_ms": _ms(painted[-1]["start_us"] - painted[0]["start_us"]) if painted else None,
        "late_judged": len(late),
        "late_ms_p95": _ms(percentile(late, 95)),
        "late_ms_max": _ms(max(late) if late else None),
        "cpu_ms_p95": _ms(percentile(cpu, 95)),
        "cpu_ms_max": _ms(max(cpu) if cpu else None),
        "longest_tick_ms": _ms(longest[0]),
        "longest_tick_cpu_ms": _ms(longest[1].get("cpu_us") if isinstance(longest[1].get("cpu_us"), int) else None),
        "input_to_first_frame_ms": input_ms,
        "input_to_first_frame_compositor_ms": input_compositor_ms,
        "present_interval_ms_p50": _ms(percentile(gaps, 50)),
        "present_interval_ms_p95": _ms(percentile(gaps, 95)),
        "present_interval_ms_max": _ms(max(gaps) if gaps else None),
        "display_wait_ms": _ms(sum(r["sleep_us"] for r in blocked)),
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

# Read as root: the compositor is not dumpable, and every user's processes count. One
# line per process with its parent, owner, RSS, anonymous and shmem RSS, its PSS split
# from smaps_rollup (what it costs once shared pages are divided among their mappers),
# its cgroup and, for `tilo-launcher`, the `--app-id` it runs. Then one line per cgroup
# seen, and per ancestor a cgroup without a memory controller of its own is charged to,
# with memory.current and memory.stat's anon, file (which holds shmem), shmem,
# file_mapped, kernel and sock. Then the whole of /proc/meminfo, greetd's uid (whose
# slice is the seat0 greeter) and each tmpfs's use: shmem that no process need map.
PROCESSES = r'''
STAT='{v[$1] = $2} END {n = split("anon file shmem file_mapped kernel sock", k, " "); s = ""
      for (i = 1; i <= n; i++) s = s "\t" ((k[i] in v) ? v[k[i]] : "-"); print substr(s, 2)}'
charged_at() {
  at=$1
  while [ -n "$at" ] && [ ! -r "/sys/fs/cgroup$at/memory.current" ]; do at=${at%/*}; done
  printf '%s\n' "${at:--}"
}
cgs=""
for d in /proc/[0-9]*; do
  # status and smaps_rollup name their fields apart (VmRSS/RssAnon vs Pss/Pss_Anon), so
  # one stream holds both; a process gone by the second read only lacks its PSS.
  st=$(cat $d/status $d/smaps_rollup 2>/dev/null | awk '/^PPid:/ {p=$2} /^Uid:/ {u=$2}
         /^VmRSS:/ {r=$2} /^RssAnon:/ {a=$2} /^RssShmem:/ {s=$2} /^Pss:/ {ps=$2} /^Pss_Anon:/ {pa=$2}
         /^Pss_Shmem:/ {ph=$2}
         END {if (r) print p, u, r, a+0, s+0, (ps == "" ? "-" : ps), (pa == "" ? "-" : pa),
                           (ph == "" ? "-" : ph)}')
  [ -n "$st" ] || continue
  comm=$(tr ' \t' '__' 2>/dev/null < $d/comm)
  cg=$(sed -n "s/^0:://p" 2>/dev/null < $d/cgroup)
  app=""
  [ "$comm" = tilo-launcher ] && \
    app=$(tr '\0' '\n' 2>/dev/null < $d/cmdline | awk 'p {print; exit} $0 == "--app-id" {p = 1}')
  set -- $st
  printf 'P\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "${d#/proc/}" "$1" "$2" "$comm" \
    "$3" "$4" "$5" "$6" "$7" "$8" "${app:--}" "$cg"
  [ -z "$cg" ] || cgs="$cgs$cg
"
done
{ printf '%s' "$cgs"; printf '%s' "$cgs" | sort -u | while IFS= read -r cg; do charged_at "$cg"; done; } \
  | grep -v '^-$' | sort -u | while IFS= read -r cg; do
  f="/sys/fs/cgroup$cg"
  if [ -r "$f/memory.current" ]; then
    printf 'C\t%s\t%s\t%s\t%s\n' "$cg" "$cg" "$(cat "$f/memory.current")" "$(awk "$STAT" "$f/memory.stat")"
  else
    printf 'C\t%s\t%s\t-\t-\t-\t-\t-\t-\t-\n' "$cg" "$(charged_at "$cg")"
  fi
done
awk '{print "M", $1, $2}' /proc/meminfo
echo "G $(id -u greetd 2>/dev/null || echo -)"
df -kP -t tmpfs 2>/dev/null | awk 'NR > 1 {print "T", $3, $6}'
'''

# The memory.stat fields of a `C` line, after memory.current.
CGROUP_STAT = ("anon", "file", "shmem", "file_mapped", "kernel", "sock")


def _mb(kib_or_bytes: str, unit: int) -> float | None:
    return None if kib_or_bytes == "-" else round(int(kib_or_bytes) * unit / 2**20, 1)


def parse_processes(text: str) -> dict:
    """`PROCESSES` output -> {"procs": {pid: ...}, "cgroups": {path: ...}, "meminfo_mb",
    "meminfo_kib", "greeter_uid", "tmpfs"}.

    Each process carries `app`: the `--app-id` of the nearest `tilo-launcher` at or above it,
    so an app is its launcher's whole process tree, whatever its processes are called. Each
    cgroup carries `charged_at`: itself, or the nearest ancestor with a memory controller,
    where its memory is charged (None: nowhere the dump could read)."""
    procs, cgroups, kib, tmpfs, greeter = {}, {}, {}, [], None
    for line in text.splitlines():
        f = line.split("\t")
        if f[0] == "P" and len(f) == 13 and f[1].isdigit():
            procs[int(f[1])] = {"pid": int(f[1]), "ppid": int(f[2]), "uid": int(f[3]), "comm": f[4],
                                "rss_mb": _mb(f[5], 1024), "anon_rss_mb": _mb(f[6], 1024),
                                "shmem_rss_mb": _mb(f[7], 1024), "pss_mb": _mb(f[8], 1024),
                                "pss_anon_mb": _mb(f[9], 1024), "pss_shmem_mb": _mb(f[10], 1024),
                                "launches": None if f[11] == "-" else f[11], "cgroup": f[12]}
        elif f[0] == "C" and len(f) == 4 + len(CGROUP_STAT):
            cgroups[f[1]] = {"charged_at": None if f[2] == "-" else f[2], "memory_mb": _mb(f[3], 1),
                             **{f"{k}_mb": _mb(v, 1) for k, v in zip(CGROUP_STAT, f[4:])}}
        else:
            w = line.split()
            if len(w) >= 3 and w[0] == "M" and w[2].isdigit():
                kib[w[1].rstrip(":")] = int(w[2])
            elif len(w) == 2 and w[0] == "G" and w[1].isdigit():
                greeter = int(w[1])
            elif len(w) >= 3 and w[0] == "T" and w[1].isdigit():
                tmpfs.append({"mount": " ".join(w[2:]), "used_mib": round(int(w[1]) / 1024, 1)})
    for p in procs.values():
        cur, seen = p, set()
        while cur and cur["launches"] is None and cur["pid"] not in seen:
            seen.add(cur["pid"])
            cur = procs.get(cur["ppid"])
        p["app"] = cur["launches"] if cur else None
    return {"procs": procs, "cgroups": cgroups, "meminfo_kib": kib,
            "meminfo_mb": {k: v // 1024 for k, v in kib.items()}, "greeter_uid": greeter,
            "tmpfs": sorted(tmpfs, key=lambda t: -t["used_mib"])}


def processes(ctx) -> dict:
    return parse_processes(ctx.vm.root_exec(PROCESSES))


# Read as root, for one process (the compositor): what its anonymous memory is. Its
# status lines, its smaps_rollup (Anonymous, Pss_Anon, Rss, ...), and its 20 largest
# mappings by Anonymous kB: one `[heap]` or a run of 64 MiB-aligned `[anon]` regions
# (glibc arenas) reads differently from one GPU buffer that doubled.
SHELL_MEMORY = r'''
pid=__PID__
awk '/^(VmRSS|RssAnon|RssFile|RssShmem|VmSwap|Threads):/ {print "S", $1, $2}' /proc/$pid/status
awk 'NR > 1 && $3 == "kB" {print "R", $1, $2}' /proc/$pid/smaps_rollup
awk '/^[0-9a-f]+-[0-9a-f]+ / {range = $1; name = "[anon]"; if (NF >= 6) {name = $6; for (i = 7; i <= NF; i++) name = name " " $i}}
     /^Anonymous:/ && $2 > 0 {print "A", $2, range, name}' /proc/$pid/smaps | sort -k2,2nr | head -n 20
'''


def parse_shell_memory(text: str) -> dict:
    """`SHELL_MEMORY` output -> {"status_kib", "smaps_rollup_kib", "top_anon_mappings"}."""
    status, rollup, top = {}, {}, []
    for line in text.splitlines():
        f = line.split()
        if len(f) >= 3 and f[0] in ("S", "R") and f[2].isdigit():
            (status if f[0] == "S" else rollup)[f[1].rstrip(":")] = int(f[2])
        elif len(f) >= 4 and f[0] == "A" and f[1].isdigit():
            top.append({"anon_kib": int(f[1]), "range": f[2], "name": " ".join(f[3:])})
    return {"status_kib": status, "smaps_rollup_kib": rollup, "top_anon_mappings": top}


def shell_memory(ctx, pid: int) -> dict:
    """The compositor's memory breakdown; Unobservable when its anonymous RSS cannot be read."""
    m = parse_shell_memory(ctx.vm.root_exec(SHELL_MEMORY.replace("__PID__", str(int(pid)))))
    if "RssAnon" not in m["status_kib"] or "Anonymous" not in m["smaps_rollup_kib"]:
        raise Unobservable(f"the compositor's (pid {pid}) status and smaps_rollup could not be read")
    m["pid"] = pid
    m["anon_rss_mib"] = round(m["status_kib"]["RssAnon"] / 1024, 1)
    return m


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

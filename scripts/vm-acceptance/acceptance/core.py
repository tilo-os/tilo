"""Checks, their results, and the facts every area reads off a state dump."""

from __future__ import annotations

import re
import time
import traceback
from dataclasses import dataclass, field
from pathlib import Path

from . import session_unit
from .vm import VmError, wait_until

# Who fixes what: each area's failures are routed to its owner.
AREAS = {
    "provenance": "Integrator / local install",
    "window": "Window management, animations and gestures",
    "ui": "UI consistency",
    "apps": "App lifecycle",
    "search": "Search and history",
    "resources": "Resources",
    "basics": "Basics: hardware controls, keyboard, displays",
    "agent": "Agent door: the in-session MCP path",
    "input": "Input fidelity: real device descriptors, layouts, touchpad",
    "secrets": "Security and identity: secret custody",
}


class CheckFailed(Exception):
    """The subject was observed and is wrong."""


class Unobservable(CheckFailed):
    """The check could not observe its subject. Reported as a failure, never a skip."""


class ExpectedFailure(CheckFailed):
    """The subject is known to be broken or missing in this image, and the reason is named.

    Reported as `xfail`, which does not fail the run: the check keeps proving the gap and
    starts passing the moment the product does. Raise it only for a *named, checkable*
    precondition (a socket the image should serve and does not), never for an observation
    that merely went wrong: that stays a failure."""


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


def check(id: str, area: str, title: str, needs: tuple = (), calibrating: str = ""):
    """Register a check. The function gets (ctx, result) and raises to fail.

    `calibrating` names why the check's expectation has not yet been confirmed against a
    real run (an observable assumed from the code, not seen working): while it is set, a
    failure is reported as `xfail` with that reason and a pass is a pass. Remove it after the
    first green CI run; a check that stays red then is a real finding."""
    assert area in AREAS, area

    def wrap(func):
        REGISTRY.append(Check(id, area, title, _calibrated(func, calibrating) if calibrating else func, needs))
        return func

    return wrap


def _calibrated(func, why: str):
    """`func` whose failures, of any kind, are expected ones while its expectation is unconfirmed."""
    def run(ctx, res):
        try:
            return func(ctx, res)
        except ExpectedFailure:
            raise
        except Exception as exc:  # noqa: BLE001 - an assumed observable may fail in any way
            raise ExpectedFailure(f"{type(exc).__name__}: {exc} (still calibrating: {why})") from exc

    run.__module__, run.__name__ = func.__module__, func.__name__
    return run


# The session locks after 5 minutes without a person's input (Settings' default), and the
# harness drives most checks through the control socket, which is no person's input: R01 to
# R03 alone run that long, and the idle lock then fired inside R03 (its compositor memory
# taken over the lock overlay, the session left locked). A lone Shift press, real input that
# no binding takes, restarts the person's idle period before a check once the last one is
# this old; the settle keeps its handling out of the check's own idle windows.
AWAKE_EVERY_S = 120
AWAKE_SETTLE_S = 2.0


def keep_awake(ctx) -> None:
    """Press Shift on the slot's seat when the last press is `AWAKE_EVERY_S` old (or there was
    none), so no check meets the idle dim, blank or lock it is not about. run.py calls it
    before each check."""
    now = time.monotonic()
    last = ctx.facts.get("awake_at")
    if last is not None and now - last < AWAKE_EVERY_S:
        return
    try:
        ctx.vm.key("shift")
    except VmError as exc:
        ctx.facts["awake_failed"] = exc.reason()
        return
    ctx.facts["awake_at"] = now
    time.sleep(AWAKE_SETTLE_S)


def run_check(ctx, chk: Check) -> Result:
    res = Result(chk.id, chk.area, chk.title)
    started = time.monotonic()
    ctx.current = res
    if "session_unit" not in ctx.facts:
        ctx.facts["session_unit"] = session_unit.show(ctx)
        ctx.facts["session_requested"] = getattr(ctx.vm, "requested_restarts", 0)
    try:
        for need in chk.needs:
            if not ctx.facts.get(need):
                raise Unobservable(f"precondition `{need}` not met: "
                                   f"{ctx.facts.get(need + ':why', 'an earlier check failed')}")
        chk.func(ctx, res)
        res.status = "pass"
    except ExpectedFailure as exc:
        res.status, res.detail = "xfail", _join(res.detail, f"expected failure: {exc}")
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
        res.evidence.append(f"errors/{chk.id}.txt")
    leave_running(ctx, chk, res)
    leave_unlocked(ctx, chk, res)
    leave_no_dialog(ctx, chk, res)
    restarted = session_restart(ctx, chk.id)
    if restarted:
        res.status = "fail"
        res.detail = _join(res.detail, restarted)
    res.seconds = round(time.monotonic() - started, 2)
    ctx.current = None
    return res


LEFT_STOPPED = "left no slot session answering"
# How long a session that is not answering after a check is given to come back by itself
# (systemd's Restart= after a crash, a session still starting) before the harness restarts it.
SESSION_RETURN_S = 60


def leave_running(ctx, chk: Check, res: Result) -> None:
    """No check may hand the next one a slot whose session does not answer: every later
    check would fail on a missing control socket instead of its subject (after A17 ended
    the session on purpose and the next one died too, R04 and W01-W07 all failed so). A
    session that does not answer is waited for until its control socket answers again,
    as long as systemd may still be bringing it back; one still silent after that is
    restarted by the harness and the check fails, however it ended."""
    def answering():
        try:
            return ctx.state()
        except VmError:
            return None

    if answering() is not None:
        return
    if wait_until(answering, SESSION_RETURN_S, 2.0) is not None:
        return
    res.status = "fail"
    ctx.facts.setdefault("session_recovered_after", []).append(chk.id)
    try:
        ctx.vm.restart_session()
    except VmError as exc:
        res.detail = _join(res.detail, f"{LEFT_STOPPED} within {SESSION_RETURN_S} s, and restarting "
                                       f"the slot session failed: {exc.reason()}")
        return
    res.detail = _join(res.detail, f"{LEFT_STOPPED} within {SESSION_RETURN_S} s; the harness "
                                   "restarted the slot session")


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


SESSION_RESTARTED = "the slot's desktop session restarted on its own"


def session_restart(ctx, during: str) -> str | None:
    """Why the check fails if the slot's session ended on its own since the last reading
    (`acceptance/session_unit.py`): every window was lost, and nothing the check measured
    after it is about the session it started in. Writes what the ended run left behind as
    `data/<check>-<n>-session-restart.log` once per ending; None while the session is the
    one the last reading saw, the harness asked for the restart (`TiloVm.restart_session`),
    or the unit cannot be read (P03 reports that)."""
    fields = session_unit.show(ctx)
    before = ctx.facts.get("session_unit")
    requested = getattr(ctx.vm, "requested_restarts", 0)
    asked = requested != ctx.facts.get("session_requested", requested)
    if fields is not None:
        ctx.facts["session_unit"] = fields
    ctx.facts["session_requested"] = requested
    ended, ctx.facts["session_pending"] = session_unit.ended_on_its_own(
        before, fields, asked, ctx.facts.get("session_pending", False))
    if not ended:
        return None
    seen = ctx.facts.setdefault("session_restarted_during", [])
    seen.append(during)
    where, how = session_unit.dump(ctx, f"{ctx.current.id if ctx.current is not None else 'run'}-{len(seen)}",
                                   fields)
    return (f"{SESSION_RESTARTED} during {during}: {how}, and systemd starts "
            f"{session_unit.unit(ctx.vm.slot)} again with every window gone; what the ended run "
            f"left: {where}")


def adopt_session(ctx) -> None:
    """Take the slot session as it is now as the guard's baseline: for a check that ends
    the session on purpose, the way a crash would (A17), once it has recovered. Counted in
    `session_restarts_caused`, so P03 does not report that restart either."""
    ctx.facts["session_restarts_caused"] = ctx.facts.get("session_restarts_caused", 0) + 1
    ctx.facts["session_unit"] = session_unit.show(ctx)
    ctx.facts["session_requested"] = getattr(ctx.vm, "requested_restarts", 0)
    ctx.facts["session_pending"] = False


LEFT_OPEN = "left a dialog open"

# Escape presses before the harness gives up on closing what a check left open: the
# Invite composer's review step takes two (review, then the composer), a stack of
# surfaces one each.
DISMISS_ESCAPES = 4

# Shell surfaces the state dump reports under `input_capture`, beside `overlays`.
_CAPTURING = (("onboarding", "first-login onboarding"),
              ("shortcut_help", "the shortcut help"),
              ("projection", "the projection picker"))


def open_surfaces(state: dict) -> list[str]:
    """Every modal, overlay, prompt or dialog the state dump shows open, by name.

    Each of these takes the keyboard from the window behind it (the compositor's
    `chrome_captures_input`), so one left open by a check receives the next check's typing:
    run 89618's A01 typed its Terminal command into the Invite composer W20 had opened.
    `input_capture.chrome` is that same predicate, so it catches a capturing surface the
    dump has no field of its own for; pointer hover over a maximized window's controls
    (`maximized_controls`) is part of it too, and is not a dialog."""
    chrome = state.get("chrome") or {}
    found = [f"the {name.replace('_', ' ')} overlay"
             for name, on in (chrome.get("overlays") or {}).items() if on]
    if (chrome.get("palette") or {}).get("visible"):
        found.append("search")
    if (chrome.get("window_switcher") or {}).get("open"):
        found.append("the window switcher")
    peer = chrome.get("peer_sync") or {}
    if peer.get("invite_name") is not None:
        found.append("the Invite someone composer")
    elif peer.get("invite_token") is not None:
        found.append("an invite card showing a live token")
    if peer.get("connect_input") is not None:
        found.append("the Join card")
    portal = chrome.get("portal_dialog")
    if portal:
        kind = portal.get("kind") if isinstance(portal, dict) else None
        found.append(f"a portal dialog ({kind})" if kind else "a portal dialog")
    if chrome.get("pending_grants_count"):
        found.append("a permission prompt")
    if chrome.get("editing_workspace_name") is not None:
        found.append("the workspace-name editor")
    capture = chrome.get("input_capture") or {}
    found += [label for key, label in _CAPTURING if capture.get(key)]
    # The power menu is an inner surface the overlays already name.
    if capture.get("inner_surface") and not (chrome.get("overlays") or {}).get("power_menu"):
        found.append("a modal shell surface (screenshot markup or capture)")
    if not found and capture.get("chrome") and not capture.get("maximized_controls"):
        found.append("a shell surface that captures input")
    return found


def dismiss_surfaces(ctx) -> str:
    """Close whatever `open_surfaces` reports, the way a person cancels: Escape, re-reading
    the dump after each press. Escape on a prompt answers it as dismissed (deny), never
    confirmed. Still open after that, restart the slot's session: a fresh compositor opens
    with none of them. Returns how it went; never raises."""
    for _ in range(DISMISS_ESCAPES):
        try:
            ctx.vm.key("Escape")
            time.sleep(0.6)
            if not open_surfaces(ctx.state()):
                return "the harness closed it with Escape"
        except (VmError, KeyError, TypeError):
            break
    try:
        ctx.vm.restart_session()
    except VmError as exc:
        return f"Escape did not close it, and restarting the slot session failed: {exc.reason()}"
    try:
        still = open_surfaces(ctx.state())
    except (VmError, KeyError, TypeError):
        still = []
    if still:
        return f"Escape did not close it, and after a slot session restart {', '.join(still)} is still open"
    return "Escape did not close it; the harness restarted the slot session"


def leave_no_dialog(ctx, chk: Check, res: Result) -> None:
    """No check may hand the next one a desktop with a modal, overlay, prompt or dialog
    open: it takes the keyboard, and the next check's input lands in it instead of in its
    subject. A check that ends with any `open_surfaces` fails ("left a dialog open"), however
    it ended, and the harness closes them before the next check. Runs after
    `leave_unlocked`, which owns a locked session."""
    try:
        state = ctx.state()
        if state["chrome"].get("session_locked"):
            return
        left = open_surfaces(state)
    except (VmError, KeyError, TypeError):
        return  # no state to read: the next check that needs one reports why
    if not left:
        return
    res.status = "fail"
    route = dismiss_surfaces(ctx)
    ctx.facts.setdefault("dialog_recovered_after", []).append(
        {"check": chk.id, "open": left, "route": route})
    res.detail = _join(res.detail, f"{LEFT_OPEN} ({', '.join(left)}); {route}")


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


# The compositor warns with this line, its phase timings attached, whenever the work
# it does before a paint holds input up.
PUMP_STALL = "pre-paint pump"


def parse_cpu_ticks(stat: str) -> tuple[int, int] | None:
    """(total, steal) jiffies of all CPUs from `/proc/stat`'s first line."""
    for line in stat.splitlines():
        parts = line.split()
        if parts and parts[0] == "cpu":
            values = [int(v) for v in parts[1:9] if v.isdigit()]
            if len(values) < 8:
                return None
            return sum(values), values[7]
    return None


def cpu_ticks(ctx) -> tuple[int, int] | None:
    """The guest's (total, steal) CPU jiffies now; `steal_percent` compares two."""
    return parse_cpu_ticks(ctx.vm.exec("head -n 1 /proc/stat", check=False) or "")


def steal_percent(before: tuple[int, int] | None, after: tuple[int, int] | None) -> float | None:
    """The share of the guest's CPU time the host took (steal) between two readings."""
    if not before or not after:
        return None
    total = after[0] - before[0]
    if total <= 0:
        return None
    return round(100.0 * (after[1] - before[1]) / total, 1)


def pump_lines_after(ctx, mark: int) -> list[str]:
    """The compositor's pre-paint pump stall lines since session-log line `mark`."""
    out = ctx.vm.exec(f'tail -n +{mark + 1} "{SESSION_LOG}" 2>/dev/null '
                      f'| grep -a "{PUMP_STALL}"; true', check=False) or ""
    return [line.strip() for line in out.splitlines() if PUMP_STALL in line]


def frame_pacing_evidence(ctx, check_id: str, log_mark: int, cpu_before) -> str:
    """What a failed frame-pacing scenario leaves to tell its stalls apart: the
    compositor's per-phase pump breakdown for every pump that held input up since
    `log_mark` (all of them in `<check_id>-pump.log`) and the share of the guest's CPU
    the host took meanwhile. A sentence fragment for the failure message."""
    steal = steal_percent(cpu_before, cpu_ticks(ctx))
    lines = pump_lines_after(ctx, log_mark)
    ctx.write(f"{check_id}-pump.log", "\n".join(lines) + "\n")
    steal_text = "unreadable" if steal is None else f"{steal}%"
    if not lines:
        pumps = "no pre-paint pump stall line in the session log"
    else:
        shown = "; ".join(line[-320:] for line in lines[-3:])
        pumps = f"{len(lines)} pre-paint pump stall line(s), the last: {shown}"
    return f" [guest CPU steal over the scenario {steal_text}; {pumps}; {check_id}-pump.log has them all]"


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


def own_cpu_us(r: dict):
    """The loop thread's CPU on tick `r` less what it spent answering this harness on
    the test-control socket (`testctl_cpu_us`): a request lands in whichever tick runs
    when it arrives, and a state dump is the harness's cost, not the session's. `None`
    where the trace has no `cpu_us`."""
    cpu = r.get("cpu_us")
    if not isinstance(cpu, int):
        return None
    harness = r.get("testctl_cpu_us")
    return max(0, cpu - harness) if isinstance(harness, int) else cpu


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
    cpu = [c for c in (own_cpu_us(r) for r in painted) if c is not None]
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


def paint_reasons(r: dict) -> set[str]:
    """The reasons the compositor painted this tick (its `paint_reasons` column)."""
    return {x for x in str(r.get("paint_reasons") or "").split("+") if x}


def tick_phases(r):
    """Where one tick's time went (wall ms per loop phase; `cpu_ms` is the loop
    thread's CPU), so a stall names the phase it stalled in."""
    if r is None:
        return None
    phases = {k[:-3] + "_ms": _ms(r[k]) for k in ("input_us", "pump_us", "layout_us", "scene_us",
                                                  "submit_us", "clients_us", "testctl_us")
              if isinstance(r.get(k), int)}
    cpu = _ms(own_cpu_us(r))
    return {"seq": r.get("seq"), "cpu_ms": cpu, **phases, "input_step": r.get("input_step") or "",
            "paint_reasons": r.get("paint_reasons") or ""}


def describe_phases(t) -> str:
    """`tick_phases` as a sentence fragment, slowest phase first."""
    if not t:
        return ""
    phases = sorted(((k[:-3], v) for k, v in t.items() if k.endswith("_ms") and k != "cpu_ms"
                     and isinstance(v, (int, float)) and v), key=lambda kv: -kv[1])
    text = ", ".join(f"{k} {v} ms" for k, v in phases)
    step = f"; slowest input step {t['input_step']}" if t.get("input_step") else ""
    why = f"; painted for {t['paint_reasons']}" if t.get("paint_reasons") else ""
    return f" (tick {t.get('seq')}: {text} wall{step}{why})"


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
    # `late_cpu` bounds each frame's lateness by the CPU its tick spent: the part of
    # the lateness that was the compositor's own work rather than the loop thread
    # waiting for a CPU (on a software renderer the rasteriser's threads compete for
    # them) or blocked.
    late, late_cpu = [], []
    prev_submit, ready = None, None
    for x, nxt in zip(rows, rows[1:] + [None]):
        if x.get("painted") == 1 and isinstance(x.get("deadline_late_us"), int):
            submit = x["start_us"] + _to_submit_us(x)
            if prev_submit is not None:
                value = max(0, min(x["deadline_late_us"], submit - max(prev_submit, ready or 0)))
                late.append(value)
                if own_cpu_us(x) is not None:
                    late_cpu.append(min(value, own_cpu_us(x)))
            prev_submit, ready = submit, None
        elif x.get("backpressured") == 1 and nxt is not None:
            ready = nxt["start_us"]
    cpu = [c for c in (own_cpu_us(r) for r in rows) if c is not None]
    worst = max((r for r in rows if own_cpu_us(r) is not None), key=own_cpu_us,
                default=None)
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
        "late_cpu_ms_p95": _ms(percentile(late_cpu, 95)),
        # Why each frame painted (the compositor's paint_reasons column; absent from
        # traces of compositors that predate it).
        "reasons_recorded": any("paint_reasons" in r for r in painted),
        "transition_frames": sum("transition" in paint_reasons(r) for r in painted),
        "cpu_max_tick": tick_phases(worst),
        "cpu_ms_p95": _ms(percentile(cpu, 95)),
        "cpu_ms_max": _ms(max(cpu) if cpu else None),
        "longest_tick_ms": _ms(longest[0]),
        "longest_tick_cpu_ms": _ms(own_cpu_us(longest[1])),
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


def region_ink(img, rect, inset: int = 2, spread: float = 0.08) -> int:
    """Pixels of `rect` (x, y, w, h), `inset` px in from its edges, whose luminance is more
    than `spread` (0-1) away from the region's most common one: what a client drew on its
    background. A flat fill has none; a terminal's one-line prompt has a few hundred."""
    x, y, w, h = (int(round(v)) for v in rect)
    box = (max(0, x + inset), max(0, y + inset),
           min(img.width, x + w - inset), min(img.height, y + h - inset))
    if box[2] <= box[0] or box[3] <= box[1]:
        return 0
    return histogram_ink(img.crop(box).convert("L").histogram(), spread)


def histogram_ink(hist, spread: float = 0.08) -> int:
    """Of a 256-bin luminance histogram, the pixels more than `spread` (0-1) away from
    its most common value."""
    mode = max(range(len(hist)), key=hist.__getitem__)
    reach = spread * 255
    return sum(count for value, count in enumerate(hist) if abs(value - mode) > reach)


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
# line per process with its parent, owner, RSS split into anon, file and shmem, swap, its
# cgroup and, for `tilo-launcher`, the `--app-id` it runs. (Tilo's kernel is built
# without CONFIG_PROC_PAGE_MONITOR, so there is no smaps or smaps_rollup and no PSS: a
# process's own memory is its RssAnon; shmem and file pages are shared and are counted
# once, per cgroup, below.) Then one line per cgroup seen, and per ancestor a cgroup
# without a memory controller of its own is charged to, with memory.current and
# memory.stat's anon, file (which holds shmem), shmem, file_mapped, kernel and sock.
# Then the whole of /proc/meminfo, every zone's managed pages, low and high watermarks
# and largest lowmem protection (which make up the reserve MemAvailable withholds), the
# page size, greetd's uid (whose slice is the seat0 greeter) and each tmpfs's use.
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
  st=$(awk '/^PPid:/ {p=$2} /^Uid:/ {u=$2} /^VmRSS:/ {r=$2} /^RssAnon:/ {a=$2} /^RssFile:/ {f=$2}
            /^RssShmem:/ {s=$2} /^VmSwap:/ {w=$2}
            END {if (r) print p, u, r, a+0, f+0, s+0, w+0}' $d/status 2>/dev/null)
  [ -n "$st" ] || continue
  comm=$(tr ' \t' '__' 2>/dev/null < $d/comm)
  cg=$(sed -n "s/^0:://p" 2>/dev/null < $d/cgroup)
  app=""
  [ "$comm" = tilo-launcher ] && \
    app=$(tr '\0' '\n' 2>/dev/null < $d/cmdline | awk 'p {print; exit} $0 == "--app-id" {p = 1}')
  set -- $st
  printf 'P\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "${d#/proc/}" "$1" "$2" "$comm" \
    "$3" "$4" "$5" "$6" "$7" "${app:--}" "$cg"
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
awk 'function out() {if (z != "") print "Z", z, managed + 0, low + 0, high + 0, prot + 0}
     /^Node/ {out(); z = $2 $4; managed = low = high = prot = 0}
     $1 == "managed" {managed = $2} $1 == "low" {low = $2} $1 == "high" {high = $2}
     $1 == "protection:" {gsub(/[(),]/, " "); for (i = 2; i <= NF; i++) if ($i + 0 > prot) prot = $i + 0}
     END {out()}' /proc/zoneinfo
echo "PS $(getconf PAGESIZE)"
echo "G $(id -u greetd 2>/dev/null || echo -)"
df -kP -t tmpfs 2>/dev/null | awk 'NR > 1 {print "T", $3, $6}'
'''

# The memory.stat fields of a `C` line, after memory.current.
CGROUP_STAT = ("anon", "file", "shmem", "file_mapped", "kernel", "sock")


def _mb(kib_or_bytes: str, unit: int) -> float | None:
    return None if kib_or_bytes == "-" else round(int(kib_or_bytes) * unit / 2**20, 1)


def parse_processes(text: str) -> dict:
    """`PROCESSES` output -> {"procs": {pid: ...}, "cgroups": {path: ...}, "meminfo_mb",
    "meminfo_kib", "zones", "page_size", "greeter_uid", "tmpfs"}.

    Each process carries `app`: the `--app-id` of the nearest `tilo-launcher` at or above it,
    so an app is its launcher's whole process tree, whatever its processes are called. Each
    cgroup carries `charged_at`: itself, or the nearest ancestor with a memory controller,
    where its memory is charged (None: nowhere the dump could read)."""
    procs, cgroups, kib, zones, tmpfs, greeter, page = {}, {}, {}, {}, [], None, None
    for line in text.splitlines():
        f = line.split("\t")
        if f[0] == "P" and len(f) == 12 and f[1].isdigit():
            procs[int(f[1])] = {"pid": int(f[1]), "ppid": int(f[2]), "uid": int(f[3]), "comm": f[4],
                                "rss_mb": _mb(f[5], 1024), "anon_rss_mb": _mb(f[6], 1024),
                                "file_rss_mb": _mb(f[7], 1024), "shmem_rss_mb": _mb(f[8], 1024),
                                "swap_mb": _mb(f[9], 1024),
                                "launches": None if f[10] == "-" else f[10], "cgroup": f[11]}
        elif f[0] == "C" and len(f) == 4 + len(CGROUP_STAT):
            cgroups[f[1]] = {"charged_at": None if f[2] == "-" else f[2], "memory_mb": _mb(f[3], 1),
                             **{f"{k}_mb": _mb(v, 1) for k, v in zip(CGROUP_STAT, f[4:])}}
        else:
            w = line.split()
            if len(w) >= 3 and w[0] == "M" and w[2].isdigit():
                kib[w[1].rstrip(":")] = int(w[2])
            elif len(w) == 6 and w[0] == "Z" and all(v.isdigit() for v in w[2:]):
                zones[w[1]] = dict(zip(("managed", "low", "high", "protection"), map(int, w[2:])))
            elif len(w) == 2 and w[0] == "PS" and w[1].isdigit():
                page = int(w[1])
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
            "meminfo_mb": {k: v // 1024 for k, v in kib.items()}, "zones": zones, "page_size": page,
            "greeter_uid": greeter, "tmpfs": sorted(tmpfs, key=lambda t: -t["used_mib"])}


def processes(ctx) -> dict:
    return parse_processes(ctx.vm.root_exec(PROCESSES))


# Read as root, for one process (the compositor): what its anonymous memory is. Its
# status lines, its smaps_rollup (Anonymous, Pss_Anon, Rss, ...), and its 20 largest
# mappings by Anonymous kB: one `[heap]` or a run of 64 MiB-aligned `[anon]` regions
# (glibc arenas) reads differently from one GPU buffer that doubled. Tilo's own kernel
# has no smaps (CONFIG_PROC_PAGE_MONITOR=n, os/kernel/config-fragments/tilo.config), so
# there only the status lines exist. Its shmem is read by fd instead: per memfd, dma-buf,
# DRM and /dev/shm fd, its size and the bytes allocated to it (`stat -L` through the fd:
# a memfd's st_blocks are its shmem pages, mapped or not), and from fdinfo a dma-buf's
# size and exporter and a DRM fd's GEM totals. Shmem the compositor's cgroup holds that
# nothing maps is named here.
SHELL_MEMORY = r'''
pid=__PID__
awk '/^(VmRSS|RssAnon|RssFile|RssShmem|VmSwap|Threads):/ {print "S", $1, $2}' /proc/$pid/status
for fd in /proc/$pid/fd/*; do
  target=$(readlink "$fd" 2>/dev/null) || continue
  case "$target" in
    /memfd:*|anon_inode:dmabuf*|/dmabuf:*|/dev/dri/*|/dev/udmabuf|/dev/shm/*) ;;
    *) continue ;;
  esac
  n=${fd##*/}
  sizes=$(stat -L -c '%s %b %B' "$fd" 2>/dev/null || echo '- - -')
  info=$(awk '$1 ~ /^(size|exp_name|drm-driver|drm-total-[a-z0-9-]+|drm-resident-[a-z0-9-]+):$/ {
              sub(/:$/, "", $1); v = $2; for (i = 3; i <= NF; i++) v = v "_" $i; printf "%s=%s ", $1, v}' \
            "/proc/$pid/fdinfo/$n" 2>/dev/null)
  printf 'F\t%s\t%s\t%s\t%s\n' "$n" "$sizes" "$info" "$target"
done
[ -d /proc/$pid ] && [ ! -e /proc/$pid/smaps_rollup ] && echo "PAGE-MONITOR no" && exit 0
awk 'NR > 1 && $3 == "kB" {print "R", $1, $2}' /proc/$pid/smaps_rollup
awk '/^[0-9a-f]+-[0-9a-f]+ / {range = $1; name = "[anon]"; if (NF >= 6) {name = $6; for (i = 7; i <= NF; i++) name = name " " $i}}
     /^Anonymous:/ && $2 > 0 {print "A", $2, range, name}' /proc/$pid/smaps | sort -k2,2nr | head -n 20
'''


def _shm_fd(f: list[str]) -> dict | None:
    """One `F` line, split on tabs: fd, `size blocks block_size`, fdinfo `key=value`s, target."""
    sizes = f[2].split()
    if not f[1].isdigit() or len(sizes) != 3:
        return None
    size, blocks, unit = (int(v) if v.isdigit() else None for v in sizes)
    target = f[4]
    kind = ("memfd" if target.startswith("/memfd:") else "dmabuf" if "dmabuf" in target
            else "drm" if target.startswith("/dev/dri/") else "udmabuf" if target == "/dev/udmabuf"
            else "shm")
    name = target.removeprefix("/memfd:").removesuffix(" (deleted)") if kind == "memfd" else target
    return {"fd": int(f[1]), "kind": kind, "name": name, "size_bytes": size,
            "allocated_bytes": blocks * unit if blocks is not None and unit is not None else None,
            "fdinfo": dict(kv.split("=", 1) for kv in f[3].split() if "=" in kv)}


def shm_by_name(fds: list[dict]) -> dict:
    """The compositor's memfds and /dev/shm files summed by name: how many, their size and
    the shmem allocated to them, in MiB, largest allocation first."""
    out: dict[str, dict] = {}
    for f in fds:
        if f["kind"] not in ("memfd", "shm") or f["allocated_bytes"] is None:
            continue
        o = out.setdefault(f["name"], {"count": 0, "size_mib": 0.0, "allocated_mib": 0.0})
        o["count"] += 1
        o["size_mib"] = round(o["size_mib"] + (f["size_bytes"] or 0) / 2**20, 1)
        o["allocated_mib"] = round(o["allocated_mib"] + f["allocated_bytes"] / 2**20, 1)
    return dict(sorted(out.items(), key=lambda kv: -kv[1]["allocated_mib"]))


def parse_shell_memory(text: str) -> dict:
    """`SHELL_MEMORY` output -> {"status_kib", "smaps_rollup_kib", "top_anon_mappings",
    "shm_fds", "shm_by_name"}."""
    status, rollup, top, fds = {}, {}, [], []
    for line in text.splitlines():
        if line.startswith("F\t"):
            parts = line.split("\t", 4)
            fd = _shm_fd(parts) if len(parts) == 5 else None
            if fd:
                fds.append(fd)
            continue
        f = line.split()
        if len(f) >= 3 and f[0] in ("S", "R") and f[2].isdigit():
            (status if f[0] == "S" else rollup)[f[1].rstrip(":")] = int(f[2])
        elif len(f) >= 4 and f[0] == "A" and f[1].isdigit():
            top.append({"anon_kib": int(f[1]), "range": f[2], "name": " ".join(f[3:])})
    return {"status_kib": status, "smaps_rollup_kib": rollup, "top_anon_mappings": top,
            "shm_fds": fds, "shm_by_name": shm_by_name(fds)}


def shell_memory(ctx, pid: int) -> dict:
    """The compositor's memory breakdown; Unobservable when its anonymous RSS cannot be read."""
    text = ctx.vm.root_exec(SHELL_MEMORY.replace("__PID__", str(int(pid))))
    m = parse_shell_memory(text)
    # A kernel without the page monitor has no smaps to read: only its status is evidence.
    m["page_monitor"] = "PAGE-MONITOR no" not in text
    if "RssAnon" not in m["status_kib"] or (m["page_monitor"] and "Anonymous" not in m["smaps_rollup_kib"]):
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


# -- Terminal: where its shell writes, and what its runtime is doing -----------

# Terminal's shell runs in a `tilo-ush` guest whose `~` is the environment's own
# home in the ush store, not the session's $HOME (docs/app-model/ush.md, "Storage").
# A host shell (foot, a legacy terminal) writes to $HOME itself.
_TERMINAL_HOMES = ('"$HOME" "${XDG_DATA_HOME:-$HOME/.local/share}"/ush/environments/*/home '
                   '"${XDG_DATA_HOME:-$HOME/.local/share}"/ush/sessions/*/home')


def terminal_probe(ctx, name: str) -> list[str]:
    """Paths of `name` in any home a terminal's `~` can be: the session's and every
    ush environment's (persistent) or session's (ephemeral)."""
    out = ctx.vm.exec(f"for h in {_TERMINAL_HOMES}; do [ -e \"$h/{name}\" ] && echo \"$h/{name}\"; "
                      "done; true")
    return [l.strip() for l in out.splitlines() if l.strip() and not l.startswith("receipt:")]


# How long a new Terminal window may stand before its shell runs. The first
# session of an account also imports the environment's runtime into the user's
# package store: 7.5-21 s on the CI VM depending on its load, 40-46 s while the
# import synced once per file (#3997; runs 90460, 90980 and 90397, 90249).
TERMINAL_SHELL_S = 45

# Performance is a separate verdict from eventual readiness. The batched
# import measured 7.5–21 s in runs 90460/90980; 30 s allows nine seconds of
# load headroom but rejects the old 39.6 s baseline and 40–46 s imports.
TERMINAL_SHELL_PERFORMANCE_S = 30


def assert_terminal_shell_performance(seconds: float | None) -> None:
    expect(seconds is not None, "Terminal shell readiness was not observed")
    expect(seconds < TERMINAL_SHELL_PERFORMANCE_S,
           f"Terminal shell startup took {seconds} s; performance requires "
           f"less than {TERMINAL_SHELL_PERFORMANCE_S} s (readiness timeout is "
           f"{TERMINAL_SHELL_S} s)")

# Terminal's shells are not the app's processes: each runs in a guest the
# `tilo-ush` user service starts, so they are found in its cgroup.
_USH_SHELLS = ('cg=$(systemctl --user show -p ControlGroup --value tilo-ush.service 2>/dev/null); '
               '[ -n "$cg" ] || exit 0; '
               'for p in $(cat $(find "/sys/fs/cgroup$cg" -name cgroup.procs 2>/dev/null) 2>/dev/null); do '
               '[ "$(cat /proc/$p/comm 2>/dev/null)" = bash ] && echo "$p"; done; true')


def terminal_shells(ctx) -> set[int]:
    """Pids of the shells running in Terminal's ush guests."""
    out = ctx.vm.exec(_USH_SHELLS)
    return {int(l) for l in out.split() if l.isdigit()}


def wait_new_terminal_shell(ctx, before: set[int], timeout: float) -> float | None:
    """Seconds until a shell not in `before` runs in a ush guest, or None after `timeout`.
    A Terminal shows its pane and echoes keys before its guest exists; only a running
    shell reads what is typed."""
    started = time.monotonic()
    if wait_until(lambda: terminal_shells(ctx) - before, timeout, 0.5):
        return round(time.monotonic() - started, 1)
    return None


_USH_DIAGNOSTICS = r'''set +e
data=${XDG_DATA_HOME:-$HOME/.local/share}
echo "== units"
systemctl --user status --no-pager -l tilo-ush.socket tilo-ush.service 2>&1 | head -40
echo "== journal"
journalctl --user -u tilo-ush.service -u tilo-ush.socket --no-pager -o short-precise -n 300 2>&1 | cut -c1-400
echo "== processes"
ps -u "$(id -u)" -o pid,ppid,etimes,time,pcpu,stat,wchan:24,args --forest 2>&1 | cut -c1-300
for round in 1 2; do
  echo "== tilo-ush threads (round $round): tid comm utime stime state"
  for p in $(pgrep -u "$(id -u)" -x tilo-ush); do
    echo "-- $p $(tr '\0' ' ' < /proc/$p/cmdline | cut -c1-200)"
    for t in /proc/$p/task/*; do
      printf '%s %s %s\n' "${t##*/}" "$(cat $t/comm)" "$(sed 's/.*) //' $t/stat | cut -d' ' -f1,12,13)"
    done
  done
  [ $round = 2 ] || sleep 5
done
echo "== store"
ls -la "$data/ush" "$data/ush/environments" "$data/ush/sessions" 2>&1 | head -40
echo "== session logs mentioning ush or terminal"
grep -rIh -i -E "tilo-ush|ush\.sock|so\.tilo\.terminal" "$HOME/.local/state/tilo/session-logs" 2>/dev/null \
  | grep -v APP-STARTUP | tail -80 | cut -c1-400
'''


# What the shell runtime logs when it refuses to start an environment: an
# image that ships no env-runtime, a store it cannot open (crates/runtime/ush).
_UNBUILDABLE = "the environment cannot be built"
_ANSI = re.compile(r"\x1b\[[0-9;]*m")


def environment_refusal(journal: str) -> str | None:
    """Why the shell runtime last refused an environment, from its journal, or None.

    A run's report data is not published when the check fails, so a Terminal
    with no shell has to say in its detail what stopped it."""
    lines = [_ANSI.sub("", line) for line in journal.splitlines() if _UNBUILDABLE in _ANSI.sub("", line)]
    if not lines:
        return None
    last = lines[-1]
    reason = last.split("reason=", 1)[1] if "reason=" in last else last.split(_UNBUILDABLE, 1)[1]
    return reason.strip().strip('"') or None


def ush_environment_refusal(ctx) -> str | None:
    """`environment_refusal` of the slot's `tilo-ush` journal."""
    try:
        journal = ctx.vm.exec("journalctl --user -u tilo-ush.service --no-pager -o cat -n 400 2>&1",
                              timeout=60, check=False)
    except VmError:
        return None
    return environment_refusal(journal)


_USH_EVIDENCE = r'''set +e
echo "journal: $(journalctl --user -u tilo-ush.service --no-pager -o cat -n 200 2>&1 | grep -v '^$' | tail -n 4 | tr '\n' '|' | cut -c1-900)"
echo "session log: $(grep -rIh -i -E "tilo-ush|ush\.sock|so\.tilo\.terminal" "$HOME/.local/state/tilo/session-logs" 2>/dev/null | grep -v APP-STARTUP | tail -n 3 | tr '\n' '|' | cut -c1-900)"
echo "guests: $(ps -u "$(id -u)" -o pid=,etimes=,time=,args= 2>/dev/null | grep -E 'tilo-ush (run|guest-init)' | grep -v grep | cut -c1-160 | tr '\n' '|')"
'''


def ush_evidence(ctx) -> str | None:
    """What a Terminal with no shell can say in its detail, since a failed run publishes no
    report data: the shell runtime's last journal lines (its runtime import's start and
    duration among them), the session log's last lines about it or Terminal, and the guest
    processes still running with their age and CPU time. None when nothing could be read."""
    try:
        text = ctx.vm.exec(_USH_EVIDENCE, timeout=60, check=False)
    except VmError:
        return None
    lines = [_ANSI.sub("", line).strip() for line in text.splitlines()
             if line.strip() and not line.rstrip().endswith(":")]
    return "; ".join(lines) or None


def ush_diagnostics(ctx) -> str:
    """The shell runtime's units, journal, process tree, per-thread CPU over 5 s,
    store layout and the session log's lines about it."""
    try:
        return ctx.vm.exec(_USH_DIAGNOSTICS, timeout=90, check=False)
    except VmError as exc:
        return f"diagnostics unavailable: {exc}"

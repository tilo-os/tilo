"""What the run is testing: the commit, the image, the session."""

from __future__ import annotations

import shlex

from .. import session_unit
from ..core import Unobservable, check, expect
from ..vm import VmError

AREA = "provenance"


@check("P01", AREA, "The VM runs the image built from the commit under test")
def commit(ctx, res):
    p = ctx.facts.get("provenance") or {}
    res.metrics.update(p)
    if p.get("why"):
        raise Unobservable(p["why"])
    expect(p.get("record"), "no provenance record for this disk")
    expect(p.get("booted_digest"), "no booted image digest")
    expect(not p.get("mismatch"), p.get("mismatch", ""))
    swap = p.get("swap")
    if not swap:
        return
    # Base image plus components swapped in from a ref: say so, and pass only when every
    # requested swap is the layer its account runs. Anything else is a mixed image nobody asked for.
    base = (p["record"].get("commit") or "?")[:12]
    done = [c for c in swap["components"] if not c.get("error") and (c.get("layer") or c["component"].startswith("app:"))]
    res.detail = (f"base {base} + {', '.join(c['component'] for c in done) or 'nothing'} "
                  f"swapped from {swap['ref']} ({swap['commit'][:12]})")
    failed = [f"{c['component']}: {c.get('error') or 'not a layer of account ' + c['account'] + ' after the swap'}"
              for c in swap["components"] if c not in done]
    expect(not failed, "not swapped: " + "; ".join(failed))


@check("P02", AREA, "A headful account with two outputs is up")
def session(ctx, res):
    st = ctx.state()
    displays = st["workspace_tree"]["outputs"]["displays"]
    res.metrics["displays"] = [(d["monitor"], d["width"], d["height"]) for d in displays]
    want = ctx.config["outputs"]
    # Displays are laid out in logical pixels: a mode at scale S spans mode / S.
    got = [f"{round(d['width'])}x{round(d['height'])}" for d in sorted(displays, key=lambda d: (d["x"], d["y"]))]
    logical = [_logical(w) for w in want]
    expect(len(displays) == len(want), f"{len(want)} outputs requested, {len(displays)} present")
    expect(got == logical, f"outputs {got} in logical pixels, requested {want} (that is {logical})")


def _logical(spec: str) -> str:
    mode, _, scale = spec.partition("@")
    w, h = (int(v) for v in mode.split("x"))
    s = float(scale or 1)
    return f"{round(w / s)}x{round(h / s)}"


RESTART_MARKER = "Scheduled restart job, restart counter is at "


def _this_boot_restarts(journal: str) -> dict[str, int]:
    """Highest 'Scheduled restart job' counter in this boot's user journal."""
    marker = f": {RESTART_MARKER}"
    restarts: dict[str, int] = {}
    for line in journal.splitlines():
        if marker not in line:
            continue
        unit, rest = line.split(marker, 1)
        unit = unit.strip().split()[-1]
        digits = "".join(ch for ch in rest if ch.isdigit())
        if unit and digits:
            n = int(digits)
            restarts[unit] = max(restarts.get(unit, 0), n)
    return restarts


def _unit_said(vm, unit: str) -> str:
    """What the failed unit itself last logged, this boot.

    Its own journal, not the boot's: a session's journal runs past the size a
    `tilo-vm` command returns, so asking for all of it loses the lines that
    say why. The user manager's lines about the unit (`<unit>: Main process
    exited ...`, `Failed to start ...`) are dropped: they say it failed, never
    why."""
    out = vm.exec(
        f"journalctl --user -u {shlex.quote(unit)} -b -o cat --no-pager -n 200",
        check=False)
    manager = (f"{unit}: ", f"Failed to start {unit}", f"Started {unit}",
               f"Stopping {unit}", f"Stopped {unit}")
    said = [line.strip() for line in out.splitlines()
            if line.strip() and line.strip() != "-- No entries --"
            and not line.startswith(manager)]
    return " | ".join(said[-3:])[:600]


def session_unit_problem(fields: dict | None, during: list[str], account: str, caused: int = 0) -> str | None:
    """Why P03 fails over the account's session unit, a system unit the user journal never
    holds: it could not be read, or its main process (the compositor's wrapper) ended on
    its own and `Restart=` started a fresh session, every window lost. `during` names the
    checks `run_check` saw it happen in; `NRestarts` also counts any before the first, and
    the `caused` restarts a check brought about on purpose (A17), which do not count."""
    unit = session_unit.unit(account)
    if fields is None:
        return f"{unit} could not be read, so a restart of the desktop session would go unseen"
    restarts = max(0, int(fields["NRestarts"]) - caused)
    if not restarts and not during:
        return None
    where = f" (during {', '.join(during)})" if during else ""
    return (f"the desktop session restarted on its own: systemd restarted {unit} "
            f"{max(restarts, len(during))} time(s) this boot{where}; last, "
            f"{session_unit.how_it_ended(fields)}")


def _system_problems(ctx) -> list[str]:
    """The system's own tilo services this boot: a failed unit, a restarted device daemon, or a
    process killed by a syscall its sandbox refuses (SIGSYS, the kernel's audit type=1326)."""
    said = ctx.vm.root_exec(
        "systemctl list-units --failed --plain --no-legend | awk '$1 ~ /^tilo/ {print \"failed \" $1}'; "
        "n=$(systemctl show -p NRestarts --value tilo-runtime.service); "
        "[ \"$n\" = 0 ] || echo \"restarted $n times\"; "
        "journalctl -b -o cat --no-pager --grep 'status=31/SYS\\|type=1326\\|SECCOMP' | "
        "grep -E 'tilo[-a-z0-9@._]*\\.service|comm=\"tilo|exe=\"/usr/(libexec/tilo/|bin/tilo)' | "
        "tail -n 3 | sed 's/^/killed by a refused syscall: /' || true")
    return said.strip().splitlines()


@check("P03", AREA, "No failed, restarting or refused services in the session or the system")
def services(ctx, res):
    # Failed units are live state. Restart counts must be this boot: NRestarts
    # is the user manager's lifetime and can repeat image-bake history when
    # the journal is the source of truth for "did this boot loop?"
    failed_out = ctx.vm.exec(
        "systemctl --user list-units --failed --plain --no-legend")
    failed = [line.split()[0] for line in failed_out.splitlines() if line.strip()]
    try:
        # One line proves the user journal readable; only then may its silence mean
        # healthy. The marker query is bounded: the boot's whole user journal is larger
        # than a `tilo-vm` command returns, and the rest of it says nothing about
        # restarts. journalctl exits 1 when nothing matched, which here is the answer.
        ctx.vm.exec("journalctl --user -b -o cat --no-pager -n 1")
        journal = ctx.vm.exec(
            f"journalctl --user -b -o cat --no-pager --grep {shlex.quote(RESTART_MARKER)} "
            "|| test $? -eq 1")
    except VmError as exc:
        # Some images have no readable user journal. Keep the live-counter fallback,
        # but every query must succeed before an empty result can mean healthy.
        res.metrics["journal_error"] = str(exc)
        nrestart_out = ctx.vm.exec(
            "units=$(systemctl --user list-units --type=service --all --plain --no-legend 'tilo*') || exit $?; "
            "for u in $(printf '%s\\n' \"$units\" | awk '{print $1}'); do "
            "n=$(systemctl --user show -p NRestarts --value \"$u\") || exit $?; "
            "echo R \"$u\" \"$n\"; done")
        restarts = {}
        for line in nrestart_out.splitlines():
            parts = line.split()
            if not (len(parts) == 3 and parts[0] == "R" and parts[2].isdigit()):
                raise Unobservable(f"invalid NRestarts response: {line!r}")
            restarts[parts[1]] = int(parts[2])
        res.metrics["restart_source"] = "NRestarts (user journal unavailable)"
    else:
        restarts = _this_boot_restarts(journal)
        res.metrics["restart_source"] = "journalctl --user -b --grep 'Scheduled restart job'"
    looping = {u: n for u, n in restarts.items() if n >= 3}
    res.metrics.update(failed=failed, restarts={u: n for u, n in restarts.items() if n})
    fields = session_unit.show(ctx)
    res.metrics["session_unit"] = fields
    session = session_unit_problem(fields, ctx.facts.get("session_restarted_during") or [], ctx.vm.account,
                                   ctx.facts.get("session_restarts_caused", 0))
    # A failed run publishes no report data: each failed unit says in the detail what it
    # last logged, so the cause is in the log and not only in a file nobody can read.
    said = {unit: _unit_said(ctx.vm, unit) for unit in failed[:4]}
    problems = ([f"failed: {failed}" + "".join(f"; {unit} last logged: {text}" for unit, text in said.items()
                                                if text)] if failed else []) \
        + ([f"restart loops: {looping}"] if looping else [])
    system = _system_problems(ctx)
    res.metrics["system"] = system
    expect(not problems and not session and not system,
           "; ".join(problems + ([session] if session else []) + system))

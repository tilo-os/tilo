"""What the run is testing: the commit, the image, the session."""

from __future__ import annotations

from .. import session_unit
from ..core import check, expect

AREA = "provenance"


@check("P01", AREA, "The VM runs the image built from the commit under test")
def commit(ctx, res):
    p = ctx.facts.get("provenance") or {}
    res.metrics.update(p)
    expect(p.get("record"), p.get("why", "no provenance record for this disk"))
    expect(not p.get("mismatch"), p.get("mismatch", ""))
    swap = p.get("swap")
    if not swap:
        return
    # Base image plus components swapped in from a ref: say so, and pass only when every
    # requested swap is the layer its slot runs. Anything else is a mixed image nobody asked for.
    base = (p["record"].get("commit") or "?")[:12]
    done = [c for c in swap["components"] if not c.get("error") and (c.get("layer") or c["component"].startswith("app:"))]
    res.detail = (f"base {base} + {', '.join(c['component'] for c in done) or 'nothing'} "
                  f"swapped from {swap['ref']} ({swap['commit'][:12]})")
    failed = [f"{c['component']}: {c.get('error') or 'not a layer of slot ' + c['slot'] + ' after the swap'}"
              for c in swap["components"] if c not in done]
    expect(not failed, "not swapped: " + "; ".join(failed))


@check("P02", AREA, "A headful slot with two outputs is up")
def session(ctx, res):
    st = ctx.state()
    displays = st["workspace_tree"]["outputs"]["displays"]
    res.metrics["displays"] = [(d["monitor"], d["width"], d["height"]) for d in displays]
    res.metrics["identity"] = st["chrome"].get("local_user_id")
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


def _this_boot_restarts(journal: str) -> dict[str, int]:
    """Highest 'Scheduled restart job' counter in this boot's user journal."""
    marker = ": Scheduled restart job, restart counter is at "
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


def session_unit_problem(fields: dict | None, during: list[str], slot: str) -> str | None:
    """Why P03 fails over the slot's session unit, a system unit the user journal never
    holds: it could not be read, or its main process (the compositor's wrapper) ended on
    its own and `Restart=` started a fresh session, every window lost. `during` names the
    checks `run_check` saw it happen in; `NRestarts` also counts any before the first."""
    unit = session_unit.unit(slot)
    if fields is None:
        return f"{unit} could not be read, so a restart of the desktop session would go unseen"
    restarts = int(fields["NRestarts"])
    if not restarts and not during:
        return None
    where = f" (during {', '.join(during)})" if during else ""
    return (f"the desktop session restarted on its own: systemd restarted {unit} "
            f"{max(restarts, len(during))} time(s) this boot{where}; last, "
            f"{session_unit.how_it_ended(fields)}")


@check("P03", AREA, "No failed or restart-looping user services in the session")
def services(ctx, res):
    # Failed units are live state. Restart counts must be this boot: NRestarts
    # is the user manager's lifetime and can repeat image-bake history when
    # the journal is the source of truth for "did this boot loop?"
    failed_out = ctx.vm.exec(
        "systemctl --user list-units --failed --plain --no-legend 2>/dev/null | awk '{print $1}'",
        check=False)
    failed = [l.strip() for l in failed_out.splitlines() if l.strip()]
    journal = ctx.vm.exec("journalctl --user -b -o cat --no-pager 2>/dev/null", check=False)
    journal_ok = ctx.vm.exec(
        "journalctl --user -b -n 0 --quiet >/dev/null 2>&1; echo $?", check=False).strip() == "0"
    if journal_ok:
        restarts = _this_boot_restarts(journal)
        res.metrics["restart_source"] = "journalctl --user -b"
    else:
        nrestart_out = ctx.vm.exec(
            "for u in $(systemctl --user list-units --all --plain --no-legend 'tilo*' "
            "2>/dev/null | awk '{print $1}'); do "
            "echo R $u $(systemctl --user show -p NRestarts --value $u 2>/dev/null); done",
            check=False)
        restarts = {}
        for l in nrestart_out.splitlines():
            parts = l.split()
            if parts[:1] == ["R"] and len(parts) == 3 and parts[2].isdigit():
                restarts[parts[1]] = int(parts[2])
        res.metrics["restart_source"] = "NRestarts (user journal unavailable)"
    looping = {u: n for u, n in restarts.items() if n >= 3}
    res.metrics.update(failed=failed, restarts={u: n for u, n in restarts.items() if n})
    fields = session_unit.show(ctx)
    res.metrics["session_unit"] = fields
    session = session_unit_problem(fields, ctx.facts.get("session_restarted_during") or [], ctx.vm.slot)
    problems = ([f"failed: {failed}"] if failed else []) + ([f"restart loops: {looping}"] if looping else [])
    expect(not problems and not session, "; ".join(problems + ([session] if session else [])))

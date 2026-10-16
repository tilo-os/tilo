"""What the run is testing: the commit, the image, the session."""

from __future__ import annotations

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


@check("P02", AREA, "A headful slot with two outputs is up")
def session(ctx, res):
    st = ctx.state()
    displays = st["workspace_tree"]["outputs"]["displays"]
    res.metrics["displays"] = [(d["monitor"], d["width"], d["height"]) for d in displays]
    res.metrics["identity"] = st["chrome"].get("local_user_id")
    want = ctx.config["outputs"]
    got = [f"{int(d['width'])}x{int(d['height'])}" for d in displays]
    expect(len(displays) == len(want), f"{len(want)} outputs requested, {len(displays)} present")
    expect(all(w.split("@")[0] == g for w, g in zip(want, got)), f"output modes {got}, requested {want}")


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


@check("P03", AREA, "No failed or restart-looping user services in the session")
def services(ctx, res):
    # Failed units are live state. Restart counts must be this boot: NRestarts
    # is the user manager's lifetime and can repeat image-bake history when
    # the journal is the source of truth for "did this boot loop?"
    failed_out = ctx.vm.exec(
        "systemctl --user list-units --failed --plain --no-legend")
    failed = [line.split()[0] for line in failed_out.splitlines() if line.strip()]
    try:
        journal = ctx.vm.exec("journalctl --user -b -o cat --no-pager")
    except VmError as exc:
        # Some images have no readable user journal. Keep the live-counter fallback,
        # but every query must succeed before an empty result can mean healthy.
        res.metrics["journal_error"] = str(exc)
        nrestart_out = ctx.vm.exec(
            "units=$(systemctl --user list-units --all --plain --no-legend 'tilo*') || exit $?; "
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
        res.metrics["restart_source"] = "journalctl --user -b"
    looping = {u: n for u, n in restarts.items() if n >= 3}
    res.metrics.update(failed=failed, restarts={u: n for u, n in restarts.items() if n})
    expect(not failed and not looping,
           (f"failed: {failed}; " if failed else "") + (f"restart loops: {looping}" if looping else ""))

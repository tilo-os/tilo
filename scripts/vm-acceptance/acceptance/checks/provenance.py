"""What the run is testing: the commit, the image, the session."""

from __future__ import annotations

from ..core import check, expect

AREA = "provenance"


@check("P01", AREA, "The VM runs the image built from the commit under test")
def commit(ctx, res):
    p = ctx.facts.get("provenance") or {}
    res.metrics.update(p)
    expect(p.get("record"), p.get("why", "no provenance record for this disk"))
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


@check("P03", AREA, "No failed or restart-looping user services in the session")
def services(ctx, res):
    out = ctx.vm.exec(
        "systemctl --user list-units --failed --plain --no-legend 2>/dev/null | awk '{print $1}' | sed 's/^/F /'; "
        "for u in $(systemctl --user list-units --all --plain --no-legend 'tilo*' 2>/dev/null | awk '{print $1}'); do "
        "echo R $u $(systemctl --user show -p NRestarts --value $u 2>/dev/null); done", check=False)
    failed = [l[2:] for l in out.splitlines() if l.startswith("F ")]
    restarts = {}
    for l in out.splitlines():
        parts = l.split()
        if parts[:1] == ["R"] and len(parts) == 3 and parts[2].isdigit():
            restarts[parts[1]] = int(parts[2])
    looping = {u: n for u, n in restarts.items() if n >= 3}
    res.metrics.update(failed=failed, restarts={u: n for u, n in restarts.items() if n})
    expect(not failed and not looping,
           (f"failed: {failed}; " if failed else "") + (f"restart loops: {looping}" if looping else ""))

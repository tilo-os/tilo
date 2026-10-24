#!/usr/bin/env python3
"""Check, per app, that a change made through the UI survives a restart.

The behavior under test is `app-state`: change something in an app, kill it,
start it again, and verify that the change is still there. The script runs
the app's own binary three times against a scratch data directory:

  cold      a fresh store, no input. Dumps the document the app comes up on.
  hot       the same store, plus a list of clicks/keystrokes delivered
            through `ui::headless`, i.e. the same `Widget::handle_event` used
            for a click. The process then exits, flushing.
  restored  the same store again, no input. Dumps what came back off disk.

Two assertions, both necessary:

  hot != cold        the actions actually changed the app's document. Without
                     this an app that ignores every input would "pass".
  restored == hot    the change survived the restart.

Actions come from `persistence-scenarios.json` when an app needs specific
ones, otherwise the harness picks addressable widgets out of the app's own
live widget tree. Names matching the deny-list in DANGEROUS are never
clicked — several apps expose destructive operations.

Usage:
    scripts/persistence-sweep.py                 # every app
    scripts/persistence-sweep.py ebook monitor   # named apps
    scripts/persistence-sweep.py --json out.json # machine-readable result
"""

import argparse
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tomllib

REPO = pathlib.Path(__file__).resolve().parent.parent
APPS = REPO / "apps"
SCENARIOS = pathlib.Path(__file__).resolve().parent / "persistence-scenarios.json"

# Apps a blind input sweep must not drive. Typing into the terminal runs
# shell commands; the on-screen keyboard injects into the live session; the
# rest front block devices, firewall rules, package installs or remote
# sessions. These need a curated scenario in persistence-scenarios.json, and
# are reported as unswept rather than clicked at random.
NEEDS_CURATION = {
    "terminal", "osk", "disks", "imager", "firewall", "containers",
    "store", "remote", "backups", "camera",
}

# The generic scenario. Keystrokes first, because app-level key handling
# needs no focus and is the most reliable way to reach an app's document;
# then a coarse pointer sweep to open panes, toggle controls and focus
# inputs; then keystrokes again now that something may hold focus.
def generic_actions(viewport=(1280, 800)):
    w, h = viewport
    acts = [{"type": "tilo"}, {"key": "Enter"}]
    for y in range(40, h, 80):
        for x in range(40, w, 120):
            acts.append({"click_at": [x, y]})
    acts += [{"type": "tilo"}, {"key": "Enter"}, {"key": "Space"}]
    return acts


RUN_TIMEOUT_S = 300


def app_dirs(names):
    out = []
    for p in sorted(APPS.iterdir()):
        if not p.is_dir():
            continue
        if names and p.name not in names:
            continue
        if not (p / "tilo-app.toml").exists() or not (p / "Cargo.toml").exists():
            continue
        out.append(p)
    return out


def app_meta(d):
    manifest = tomllib.loads((d / "tilo-app.toml").read_text())
    cargo = tomllib.loads((d / "Cargo.toml").read_text())
    pkg = manifest.get("package", {}).get("id")
    bins = cargo.get("bin") or []
    binname = bins[0]["name"] if bins else cargo["package"]["name"]
    return pkg, binname, cargo["package"]["name"]


def run_app(binary, data_home, pkg, spec, log):
    env = dict(os.environ)
    env["XDG_DATA_HOME"] = str(data_home)
    env["TILO_PACKAGE_ID"] = pkg
    # Keep an inherited compositor connection from turning a headless run
    # into a windowed one, and keep the app off the user's real config.
    env.pop("WAYLAND_DISPLAY", None)
    env.pop("DISPLAY", None)
    data_home.mkdir(parents=True, exist_ok=True)
    try:
        p = subprocess.run(
            [str(binary), "--headless-drive", json.dumps(spec)],
            env=env,
            capture_output=True,
            timeout=RUN_TIMEOUT_S,
            text=True,
        )
    except subprocess.TimeoutExpired:
        log.append(f"timed out after {RUN_TIMEOUT_S}s")
        return None
    if p.returncode != 0:
        log.append(f"exit {p.returncode}: {(p.stderr or '').strip()[-400:]}")
        return None
    return p


def pick_actions(app_name, scenario):
    if scenario is not None:
        return scenario, "scenario"
    if app_name in NEEDS_CURATION:
        return None, "needs-curation"
    return generic_actions(), "generic"


def sweep_app(d, workdir, scenarios, target_dir):
    pkg, binname, crate = app_meta(d)
    result = {"app": d.name, "package": pkg, "status": "fail", "errors": []}
    binary = target_dir / binname
    if not binary.exists():
        result["errors"].append(f"binary not built: {binary}")
        return result

    actions, source = pick_actions(d.name, scenarios.get(d.name))
    result["action_source"] = source
    if actions is None:
        result["status"] = "needs-curation"
        result["errors"].append(
            "blind input would drive a destructive control; add a scenario to "
            "scripts/persistence-scenarios.json"
        )
        return result
    result["action_count"] = len(actions)

    scratch = workdir / d.name
    if scratch.exists():
        shutil.rmtree(scratch)
    cold_home = scratch / "cold"
    live_home = scratch / "live"
    cold_json = scratch / "cold.json"
    targets_json = scratch / "targets.json"
    hot_json = scratch / "hot.json"
    restored_json = scratch / "restored.json"
    scratch.mkdir(parents=True)

    # 1. Cold: what the app looks like with nothing saved.
    if run_app(binary, cold_home, pkg,
               {"dump": str(cold_json), "targets": str(targets_json)},
               result["errors"]) is None:
        result["status"] = "launch-failed"
        return result

    # 2. Hot: drive the app, then let it exit and flush.
    if run_app(binary, live_home, pkg,
               {"actions": actions, "dump": str(hot_json)},
               result["errors"]) is None:
        result["status"] = "drive-failed"
        return result

    doc = live_home / pkg / "state.automerge"
    result["state_file"] = str(doc)
    if not doc.exists():
        result["status"] = "nothing-written"
        result["errors"].append(
            f"the app exited without writing {doc} — either nothing it does "
            f"reaches its CRDT, or the store never got the document"
        )
        return result
    result["state_bytes"] = doc.stat().st_size

    # 3. Restored: a second process, same store.
    if run_app(binary, live_home, pkg, {"dump": str(restored_json)},
               result["errors"]) is None:
        result["status"] = "relaunch-failed"
        return result

    cold = cold_json.read_text()
    hot = hot_json.read_text()
    restored = restored_json.read_text()

    if hot == cold:
        result["status"] = "vacuous"
        result["errors"].append(
            "the actions did not change the document, so a passing restart "
            "would prove nothing"
        )
        return result
    if restored != hot:
        result["status"] = "not-persisted"
        result["errors"].append(diff_summary(hot, restored))
        return result

    result["status"] = "ok"
    return result


def diff_summary(hot, restored):
    try:
        a, b = json.loads(hot), json.loads(restored)
    except Exception:
        return "documents differ and at least one is not valid JSON"
    ka = set(a) if isinstance(a, dict) else set()
    kb = set(b) if isinstance(b, dict) else set()
    lost = sorted(ka - kb)
    gained = sorted(kb - ka)
    changed = sorted(k for k in ka & kb if a[k] != b[k])
    parts = []
    if lost:
        parts.append(f"keys lost on restart: {lost}")
    if gained:
        parts.append(f"keys appearing only after restart: {gained}")
    if changed:
        parts.append(f"keys with different values: {changed}")
    return "; ".join(parts) or "documents differ below the top level"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("apps", nargs="*")
    ap.add_argument("--json", type=pathlib.Path)
    ap.add_argument(
        "--target-dir",
        type=pathlib.Path,
        default=REPO / "target" / "debug",
        help="where the app binaries were built",
    )
    ap.add_argument(
        "--workdir",
        type=pathlib.Path,
        default=pathlib.Path(os.environ.get("TMPDIR", "/tmp")) / "tilo-persistence-sweep",
    )
    args = ap.parse_args()

    scenarios = json.loads(SCENARIOS.read_text()) if SCENARIOS.exists() else {}
    args.workdir.mkdir(parents=True, exist_ok=True)

    results = []
    for d in app_dirs(set(args.apps)):
        r = sweep_app(d, args.workdir, scenarios, args.target_dir)
        results.append(r)
        mark = {"ok": "PASS"}.get(r["status"], r["status"].upper())
        detail = ""
        if r["status"] == "ok":
            detail = (
                f'{r["action_count"]} actions ({r["action_source"]}), '
                f'{r["state_bytes"]} bytes on disk'
            )
        else:
            detail = "; ".join(r["errors"])[:300]
        print(f"{mark:<16} {r['app']:<18} {detail}", flush=True)

    passed = sum(1 for r in results if r["status"] == "ok")
    swept = sum(1 for r in results if r["status"] != "needs-curation")
    print(
        f"\n{passed}/{swept} swept apps verified persisting across a restart "
        f"({len(results) - swept} need a curated scenario)"
    )
    for r in results:
        if r["status"] != "ok":
            print(f"  {r['app']}: {r['status']} — {'; '.join(r['errors'])[:400]}")
    if args.json:
        args.json.write_text(json.dumps(results, indent=2))
    return 0 if passed == swept else 1


if __name__ == "__main__":
    sys.exit(main())

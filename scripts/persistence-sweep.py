#!/usr/bin/env python3
"""Prove, per app, that a change made through the real UI survives a restart.

The claim being tested is the whole point of `app-state`: change something
in an app, kill it, start it again, and the change is still there. This
script checks it the only way that means anything — by running the app's own
binary three times against a scratch data directory:

  cold      a fresh store, no input. Dumps the document the app comes up on.
  hot       the same store, plus a list of clicks/keystrokes delivered
            through `ui::headless`, i.e. the same `Widget::handle_event` a
            real click goes through. The process then exits, flushing.
  restored  the same store again, no input. Dumps what came back off disk.

Two assertions, both necessary:

  hot != cold        the actions actually changed the app's document. Without
                     this an app that ignores every input would "pass".
  restored == hot    the change survived the restart.

Actions come from `persistence-scenarios.json` when an app needs specific
ones, otherwise the harness picks addressable widgets out of the app's own
live widget tree. Names matching the deny-list in DANGEROUS are never
clicked — several apps front real destructive operations.

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

# Widgets whose names suggest an irreversible or system-level action. The
# sweep runs against a scratch data directory, but several apps front real
# devices and package managers, so these are never clicked.
DANGEROUS = re.compile(
    r"delete|remove|erase|format|wipe|flash|destroy|shutdown|reboot|poweroff"
    r"|uninstall|revoke|reset|kill|terminate|disconnect|unmount|eject|quit|exit",
    re.I,
)

# Widget kinds worth clicking when no scenario names anything specific.
INTERESTING = re.compile(
    r"Button|Toggle|Switch|Checkbox|Radio|Chip|Tab|Segmented|Row|Item|Card"
    r"|Slider|Stepper|Menu|Option|Star|Favorite|Pin",
    re.I,
)

MAX_GENERIC_CLICKS = 10
RUN_TIMEOUT_S = 90


def app_dirs(names):
    out = []
    for p in sorted(APPS.iterdir()):
        if not p.is_dir():
            continue
        if names and p.name not in names:
            continue
        if not (p / "qos-app.toml").exists() or not (p / "Cargo.toml").exists():
            continue
        out.append(p)
    return out


def app_meta(d):
    manifest = tomllib.loads((d / "qos-app.toml").read_text())
    cargo = tomllib.loads((d / "Cargo.toml").read_text())
    pkg = manifest.get("package", {}).get("id")
    bins = cargo.get("bin") or []
    binname = bins[0]["name"] if bins else cargo["package"]["name"]
    return pkg, binname, cargo["package"]["name"]


def run_app(binary, data_home, pkg, spec, log):
    env = dict(os.environ)
    env["XDG_DATA_HOME"] = str(data_home)
    env["QOS_PACKAGE_ID"] = pkg
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


def pick_actions(targets_path, scenario):
    if scenario is not None:
        return scenario, "scenario"
    try:
        targets = json.loads(targets_path.read_text())
    except Exception:
        return [], "no-targets"
    names = []
    for name, rect in targets:
        if rect[2] <= 1 or rect[3] <= 1:
            continue
        if DANGEROUS.search(name):
            continue
        if not INTERESTING.search(name):
            continue
        # Prefer the indexed form: the bare name is ambiguous once a second
        # sibling appears mid-run.
        if not name.endswith("]"):
            continue
        names.append(name)
    names.sort()
    return [{"click_named": n} for n in names[:MAX_GENERIC_CLICKS]], "generic"


def sweep_app(d, workdir, scenarios, target_dir):
    pkg, binname, crate = app_meta(d)
    result = {"app": d.name, "package": pkg, "status": "fail", "errors": []}
    binary = target_dir / binname
    if not binary.exists():
        result["errors"].append(f"binary not built: {binary}")
        return result

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

    actions, source = pick_actions(targets_json, scenarios.get(d.name))
    result["action_source"] = source
    result["actions"] = actions
    if not actions:
        result["status"] = "no-actions"
        result["errors"].append(
            "no addressable widget to drive; needs a scenario in "
            "persistence-scenarios.json"
        )
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
        result["errors"].append(f"no state document written at {doc}")
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
        default=pathlib.Path(os.environ.get("TMPDIR", "/tmp")) / "qos-persistence-sweep",
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
                f'{len(r["actions"])} actions ({r["action_source"]}), '
                f'{r["state_bytes"]} bytes on disk'
            )
        else:
            detail = "; ".join(r["errors"])[:300]
        print(f"{mark:<16} {r['app']:<18} {detail}", flush=True)

    passed = sum(1 for r in results if r["status"] == "ok")
    print(f"\n{passed}/{len(results)} apps verified persisting across a restart")
    for r in results:
        if r["status"] != "ok":
            print(f"  {r['app']}: {r['status']} — {'; '.join(r['errors'])[:400]}")
    if args.json:
        args.json.write_text(json.dumps(results, indent=2))
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())

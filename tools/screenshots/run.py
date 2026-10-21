#!/usr/bin/env python3
"""Screenshots of every first-party app in realistic situations, headless.

Each app's plan (`tools/screenshots/<app>.toml`) lists situations: a name, a
one-line description, and the steps that set the situation up, in the
`--headless-drive` grammar (`invoke` a published action by resource and
action, `bind` its answer for later steps; `wait_for_named`; `expect`), with
dates written relative to the day of the run (`${TODAY+1}`, `${MON+2}`: this
week's Wednesday). No pointer or keyboard steps: a
situation is reached through the app's resources and actions, the door every
other caller uses. An app without a plan gets one situation, the window as it
opens.

Services (`kind = "service"` in the manifest: the agent, the on-screen
keyboard the shell draws, compute workers) have no window and are skipped.
For every situation the app runs once, headless, in a fresh profile (its own
HOME, XDG directories and runtime directory; no display, no session bus), and
captures four pictures after the steps settle: dark and light, at the wide
(1280x800) and compact (420x800) viewports, and at any larger size its
plan names (`sizes = ["large"]`: 2560x1440). Each app's directory gets a
`manifest.json` naming its situations and pictures, and `resources.json`, the
published resource tree after the first situation: what a plan can invoke.
`summary.json` at the top lists every failure; the exit status is 1 when
there is one.

Usage:
  tools/screenshots/run.py --bin-dir DIR --out DIR [--lib-dir DIR ...]
                           [--apps "notes chat ..."] [--jobs N]

`--bin-dir` holds the app executables (and a `fonts/` catalog beside them);
`--lib-dir` adds directories the dynamic loader searches (a thin build's
runtime library and libstd). tools/screenshots/build.sh builds both, and
t.toml's `app-screenshots` check runs the two together:
`t outputs app-screenshots`.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import datetime
import json
import re
import os
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
PLANS = Path(__file__).resolve().parent
THEMES = ("dark", "light")
VIEWPORTS = {"wide": (1280, 800), "compact": (420, 800)}
# Sizes a plan adds for its app (`sizes = ["large"]`): a window maximised on
# a 1440p-class display, where a page laid out for a laptop can strand a
# narrow strip of small text in empty space.
EXTRA_VIEWPORTS = {"large": (2560, 1440)}
# One headless run: start, the plan's steps, four captures and the close.
# A run past this is stuck (waiting on a service a fresh profile does not
# have), and is reported as such rather than holding the whole runner.
RUN_SECONDS = 240
# Steps a plan may use: the action-driven part of the drive grammar.
ALLOWED_STEPS = {"invoke", "wait_for_named", "expect", "perform"}


# Dates in a plan relative to the day the shots are taken, so a calendar
# week or an agenda shows its content whenever the runner runs:
# `${TODAY}`, `${TODAY+3}`, `${MON+2}` (this week's Wednesday), `${MON-7}`.
DATE_TOKEN = re.compile(r"\$\{(TODAY|MON)([+-]\d+)?\}")


def resolve_dates(value, today: datetime.date):
    if isinstance(value, str):
        def date(match):
            base = today if match.group(1) == "TODAY" else today - datetime.timedelta(days=today.weekday())
            return (base + datetime.timedelta(days=int(match.group(2) or 0))).isoformat()
        return DATE_TOKEN.sub(date, value)
    if isinstance(value, list):
        return [resolve_dates(item, today) for item in value]
    if isinstance(value, dict):
        return {key: resolve_dates(item, today) for key, item in value.items()}
    return value


@dataclass
class Situation:
    name: str
    title: str
    steps: list = field(default_factory=list)


@dataclass
class Plan:
    app: str
    binary: str
    app_id: str
    situations: list
    # What the person already has in their home before the app opens:
    # files (`[[file]]`: path, text) and git repositories (`[[repository]]`:
    # path, commits of message and files, branches, uncommitted changes).
    # Steps name them under `${HOME}`.
    files: list = field(default_factory=list)
    repositories: list = field(default_factory=list)
    # Sizes beyond `VIEWPORTS` its pictures are taken at (`EXTRA_VIEWPORTS`).
    sizes: list = field(default_factory=list)


def catalog() -> list[dict]:
    """The default collection's apps: name (directory), binary, id and manifest."""
    out = subprocess.run(
        [sys.executable, str(REPO / "tools/apps/app_release.py"), "--collection", "default"],
        check=True, capture_output=True, text=True,
    ).stdout
    return json.loads(out)["catalog"]


def has_window(entry: dict) -> bool:
    """Whether the app opens a window of its own: `kind = "app"` in its
    manifest. A service (the agent, the on-screen keyboard the shell draws,
    a compute worker) has nothing to photograph."""
    manifest = tomllib.loads((REPO / entry["manifest"]).read_text())
    return manifest.get("package", {}).get("kind", "app") == "app"


def load_plan(entry: dict) -> Plan:
    path = PLANS / f"{entry['name']}.toml"
    situations = []
    if path.is_file():
        data = tomllib.loads(path.read_text())
        unknown = set(data) - {"situation", "file", "repository", "sizes"}
        if unknown:
            raise ValueError(f"{path}: unknown keys {sorted(unknown)}")
        for raw in data.get("situation", []):
            name = raw.get("name", "")
            if not name or not name.replace("-", "").isalnum() or name != name.lower():
                raise ValueError(f"{path}: situation names are lowercase words joined by '-': {name!r}")
            steps = raw.get("steps", [])
            for step in steps:
                if len(step) != 1 or next(iter(step)) not in ALLOWED_STEPS:
                    raise ValueError(
                        f"{path}: situation {name}: a step is one of {sorted(ALLOWED_STEPS)}, got {step}"
                    )
            situations.append(Situation(name, raw.get("title", name), steps))
        if not situations:
            raise ValueError(f"{path}: no situations")
        names = [s.name for s in situations]
        if len(set(names)) != len(names):
            raise ValueError(f"{path}: situation names repeat")
    else:
        situations.append(Situation("opened", "The window as it opens"))
    sizes = data.get("sizes", []) if path.is_file() else []
    if not isinstance(sizes, list) or any(size not in EXTRA_VIEWPORTS for size in sizes):
        raise ValueError(f"{path}: sizes are a list of {sorted(EXTRA_VIEWPORTS)}, got {sizes!r}")
    files = data.get("file", []) if path.is_file() else []
    repositories = data.get("repository", []) if path.is_file() else []
    for item in [*files, *repositories]:
        relative = Path(item.get("path", ""))
        if not item.get("path") or relative.is_absolute() or ".." in relative.parts:
            raise ValueError(f"{path}: a home item's path is relative to the home: {item!r}")
    return Plan(
        entry["name"], entry["binary"], entry.get("id", entry["name"]), situations,
        files, repositories, sizes,
    )


def seed_home(plan: Plan, home: Path, env: dict) -> None:
    """Put the plan's files and repositories in the profile's home."""
    for item in plan.files:
        target = home / item["path"]
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(item.get("text", ""))
    git_env = {**env, "GIT_AUTHOR_DATE": "2026-06-30T09:00:00", "GIT_COMMITTER_DATE": "2026-06-30T09:00:00"}

    def git(repo: Path, *argv: str) -> None:
        subprocess.run(["git", "-C", str(repo), *argv], env=git_env, check=True, capture_output=True)

    for item in plan.repositories:
        repo = home / item["path"]
        repo.mkdir(parents=True, exist_ok=True)
        git(repo, "init", "-q", "-b", item.get("default_branch", "main"))
        git(repo, "config", "user.name", item.get("author", "Lena Hoffmann"))
        git(repo, "config", "user.email", item.get("email", "lena@example.org"))
        for commit in item.get("commit", []):
            for name, text in commit.get("files", {}).items():
                target = repo / name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(text)
            git(repo, "add", "-A")
            git(repo, "commit", "-q", "-m", commit["message"])
        for branch in item.get("branches", []):
            git(repo, "branch", branch)
        for name, text in item.get("changes", {}).items():
            target = repo / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text)


def resolve_home(value, home: Path):
    """`${HOME}` in a step's strings: the profile's home directory."""
    if isinstance(value, str):
        return value.replace("${HOME}", str(home))
    if isinstance(value, list):
        return [resolve_home(item, home) for item in value]
    if isinstance(value, dict):
        return {key: resolve_home(item, home) for key, item in value.items()}
    return value


def profile_env(root: Path, lib_dirs: list[Path]) -> dict:
    """A fresh profile: nothing of the caller's session reaches the app."""
    env = {
        key: value
        for key, value in os.environ.items()
        if key in {"PATH", "LANG", "LC_ALL", "TZ", "RUST_BACKTRACE", "RUST_LOG",
                   "VK_DRIVER_FILES", "VK_ICD_FILENAMES", "WGPU_BACKEND",
                   "WGPU_ADAPTER_NAME", "TILO_PDFIUM_LIB"}
    }
    dirs = {
        "HOME": root / "home",
        "XDG_CONFIG_HOME": root / "config",
        "XDG_DATA_HOME": root / "data",
        "XDG_STATE_HOME": root / "state",
        "XDG_CACHE_HOME": root / "cache",
        "XDG_RUNTIME_DIR": root / "run",
    }
    for path in dirs.values():
        path.mkdir(parents=True, exist_ok=True)
    (root / "run").chmod(0o700)
    env.update({key: str(path) for key, path in dirs.items()})
    # No display and no bus: a bus address nothing listens on, so an app
    # that looks for one fails fast instead of reaching the caller's.
    env["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path={root / 'run' / 'no-bus'}"
    env["GSETTINGS_BACKEND"] = "memory"
    env.setdefault("LANG", "en_US.UTF-8")
    env.setdefault("TZ", "Europe/Berlin")
    if lib_dirs:
        env["LD_LIBRARY_PATH"] = ":".join(str(d) for d in lib_dirs)
    return env


def captures(app_dir: Path, situation: Situation, sizes: list) -> list[dict]:
    viewports = {**VIEWPORTS, **{size: EXTRA_VIEWPORTS[size] for size in sizes}}
    shots = []
    for theme in THEMES:
        for size, viewport in viewports.items():
            shots.append({
                "path": str(app_dir / f"{situation.name}-{theme}-{size}.png"),
                "theme": theme,
                "viewport": list(viewport),
            })
    return shots


def run_situation(plan: Plan, situation: Situation, index: int, args) -> dict:
    app_dir = args.out / plan.app
    app_dir.mkdir(parents=True, exist_ok=True)
    shots = captures(app_dir, situation, plan.sizes)
    with tempfile.TemporaryDirectory(prefix=f"shots-{plan.app}-") as tmp:
        tmp = Path(tmp)
        env = profile_env(tmp / "profile", args.lib_dir)
        home = Path(env["HOME"])
        seed_home(plan, home, env)
        spec = {
            "viewport": list(VIEWPORTS["wide"]),
            "actions": resolve_home(
                resolve_dates(situation.steps, datetime.date.today()), home
            ),
            "results": str(tmp / "results.json"),
            "capture": shots,
        }
        if index == 0:
            spec["resources"] = str(app_dir / "resources.json")
        command = [str(args.bin_dir / plan.binary), "--headless-drive", json.dumps(spec)]
        started = time.monotonic()
        try:
            done = subprocess.run(
                command, env=env, cwd=tmp,
                capture_output=True, text=True, timeout=RUN_SECONDS,
            )
            status, log = done.returncode, done.stdout + done.stderr
        except subprocess.TimeoutExpired as expired:
            status = "timeout"
            log = (expired.stdout or b"").decode(errors="replace") + (expired.stderr or b"").decode(errors="replace")
        except OSError as error:
            status, log = "could not start", str(error)
        seconds = round(time.monotonic() - started, 1)
        results = tmp / "results.json"
        if results.is_file():
            shutil.copyfile(results, app_dir / f"{situation.name}-results.json")
    missing = [shot["path"] for shot in shots if not Path(shot["path"]).is_file()]
    ok = status == 0 and not missing
    if not ok:
        (app_dir / f"{situation.name}.log").write_text(log)
    return {
        "app": plan.app,
        "situation": situation.name,
        "title": situation.title,
        "ok": ok,
        "status": status,
        "seconds": seconds,
        "missing": [Path(path).name for path in missing],
        "shots": [
            {"file": Path(s["path"]).name, "theme": s["theme"], "viewport": s["viewport"]}
            for s in shots if Path(s["path"]).is_file()
        ],
        "log_tail": "" if ok else "\n".join(log.strip().splitlines()[-15:]),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--lib-dir", type=Path, action="append", default=[])
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--apps", default="", help="space-separated app directory names (default: all)")
    parser.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 2) // 2))
    args = parser.parse_args()
    args.bin_dir = args.bin_dir.resolve()
    args.lib_dir = [d.resolve() for d in args.lib_dir]
    args.out = args.out.resolve()

    entries = catalog()
    if args.apps.split():
        wanted = set(args.apps.split())
        unknown = wanted - {e["name"] for e in entries}
        if unknown:
            parser.error(f"not in the default collection: {' '.join(sorted(unknown))}")
        entries = [e for e in entries if e["name"] in wanted]
    services = sorted(e["name"] for e in entries if not has_window(e))
    entries = [e for e in entries if has_window(e)]
    plans = [load_plan(entry) for entry in entries]
    for plan in plans:
        shutil.rmtree(args.out / plan.app, ignore_errors=True)
    args.out.mkdir(parents=True, exist_ok=True)

    started = time.monotonic()
    jobs = [(plan, situation, index) for plan in plans for index, situation in enumerate(plan.situations)]
    outcomes = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futures = [pool.submit(run_situation, plan, situation, index, args) for plan, situation, index in jobs]
        for future in concurrent.futures.as_completed(futures):
            outcome = future.result()
            mark = "ok  " if outcome["ok"] else "FAIL"
            print(f"{mark} {outcome['app']}/{outcome['situation']} ({outcome['seconds']}s)", flush=True)
            outcomes.append(outcome)

    for plan in plans:
        mine = {o["situation"]: o for o in outcomes if o["app"] == plan.app}
        manifest = {
            "app": plan.app,
            "id": plan.app_id,
            "situations": [
                {"name": s.name, "title": s.title, "shots": mine[s.name]["shots"]}
                for s in plan.situations
            ],
        }
        (args.out / plan.app / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    failures = [o for o in outcomes if not o["ok"]]
    summary = {
        "apps": len(plans),
        "services_without_a_window": services,
        "situations": len(outcomes),
        "shots": sum(len(o["shots"]) for o in outcomes),
        "seconds": round(time.monotonic() - started, 1),
        "failures": sorted(failures, key=lambda o: (o["app"], o["situation"])),
    }
    (args.out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(
        f"{summary['shots']} screenshots of {summary['apps']} apps in {summary['seconds']}s; "
        f"{len(failures)} situation(s) failed (see {args.out / 'summary.json'})"
    )
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

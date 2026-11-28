#!/usr/bin/env python3
"""Screenshots of every first-party app in realistic situations, headless.

Each app's plan (`tools/screenshots/<app>.toml`) lists situations: a name, a
one-line description, and the steps that set the situation up, in the
`--headless-drive` grammar (`invoke` a published action by resource and
action, `bind` its answer for later steps; `press` a control by the name a
person reads it by, or the resource a list row presents; `search` what the app
holds, in the system's search scoped to it; `wait_for_named`;
`expect`), with dates written relative to the day of the run (`${TODAY+1}`,
`${MON+2}`: this week's Wednesday). No pointer or keyboard steps: a situation is
reached through the app's resources and actions, or through the controls its
window offers by name — the same addresses a walk presses — never by a point
on a picture. An app without a plan gets one situation, the window as it
opens. An app that shows the machine it runs on (its journal, processes,
drives, files) is built from one of its own scenarios instead: each of its
situations names one (`scenario`), and the app refuses a capture without. A
plan may also stage the machine's journal (`[[journal]]`): the entries are
written where the profile's daemon reads them in place of the machine's own
journal, so a picture of Logs shows what the plan wrote and never the machine
the shots run on.

Services (`kind = "service"` in a native app's manifest: the launcher, the
on-screen keyboard the shell draws, compute workers) have no window and are
skipped. A tilolang package (`kind = "tilolang"`) opens a window like a native
app, and is driven through the same `--headless-drive` spec: the launcher starts
it on the tilolang runtime bundle the build baked beside the apps.
Each app runs headless in a fresh profile (its own HOME, XDG directories and
runtime directory; no display, no session bus): a daemon started there founds
its person once, installs the build beside these apps (`build.sh` stages one
per app) as the device's own development build once, and starts it on each
situation's capture (`tilo-runtime --run-app … --headless-drive`), with the
connectors the device runs for the app's origins. Situations built from a
scenario, or with no steps, share that device and run side by side; a
situation whose steps change anything gets a device of its own, so no picture
shows another's data. No app binary is run by this
script, so what a capture shows is what a person's app reaches on a device. It
captures four pictures after the steps settle, each the whole window as a
session shows it, the desktop's window bar above the app's frame (the harness
draws the bar and hosts the app's slots, search and History in it): dark and
light, at the wide (1280x800) and compact (420x800) viewports, and at any
larger size its plan names (`sizes = ["large"]`: 2560x1440). Each app's directory gets a
`manifest.json` naming its situations and pictures, and `resources.json`, the
published resource tree after the first situation: what a plan can invoke.
`inspect.json` beside it is the window the first situation laid out, with what
each widget says to assistive technology: the names, identities and targets a
plan presses by. `summary.json` at the top lists every failure; the exit status
is 1 when there is one.

Usage:
  tools/screenshots/run.py --bin-dir DIR --out DIR [--apps "tasks chat ..."] [--jobs N]

`--bin-dir` holds the daemon, the launcher and the tools a capture runs through
(and a `fonts/` catalog beside them), with the runtime bundles and the staged
builds beside it. tools/screenshots/build.sh builds it.
"""

from __future__ import annotations

import argparse
import base64
import concurrent.futures
import datetime
import json
import re
import os
import shutil
import socket as socket_module
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
# Steps a plan may use: the action-driven and control-driven part of the drive grammar.
ALLOWED_STEPS = {"invoke", "wait_for_named", "expect", "perform", "press", "search"}


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
    # The app's own scenario the window is built from (`"scenario"` in the
    # drive): an app that shows the machine it runs on refuses a capture
    # without one, so its pictures show made-up data, never the machine's.
    scenario: str | None = None
    # Whether the capture runs over the workspace its device serves (`workspace = true`): what it
    # stages and calls goes through the device's dispatch, so a call reaches another app (a
    # session's playback). It writes into the workspace, so it gets a device of its own.
    workspace: bool = False


@dataclass
class Plan:
    app: str
    app_id: str
    situations: list
    # What the person already has in their home before the app opens:
    # files (`[[file]]`: path, and text or base64 for what is not text) and
    # git repositories (`[[repository]]`: path, commits of message and files,
    # branches, uncommitted changes).
    # Steps name them under `${HOME}`.
    files: list = field(default_factory=list)
    repositories: list = field(default_factory=list)
    # This device's journal, as a plan stages it (`[[journal]]`: how many minutes before the run
    # it was written, its severity, unit, program and message): the daemon reads the fixture in
    # place of the machine's own journal, so a picture of Logs never shows the machine the shots
    # run on.
    journal: list = field(default_factory=list)
    # Sizes beyond `VIEWPORTS` its pictures are taken at (`EXTRA_VIEWPORTS`).
    sizes: list = field(default_factory=list)


def catalog() -> list[dict]:
    """The default collection's apps: name (directory), kind, id and manifest."""
    out = subprocess.run(
        [sys.executable, str(REPO / "tools/apps/app_release.py"), "--collection", "default"],
        check=True, capture_output=True, text=True,
    ).stdout
    return json.loads(out)["catalog"]


def has_window(entry: dict) -> bool:
    """Whether the app opens a window of its own: a tilolang package does, and
    a native app does unless its manifest declares a service (the launcher, the
    on-screen keyboard the shell draws, a compute worker), which has nothing to
    photograph."""
    if entry["kind"] == "tilolang":
        return True
    manifest = tomllib.loads((REPO / entry["manifest"]).read_text())
    return manifest.get("package", {}).get("kind", "app") == "app"


def load_plan(entry: dict) -> Plan:
    path = PLANS / f"{entry['name']}.toml"
    situations = []
    if path.is_file():
        data = tomllib.loads(path.read_text())
        unknown = set(data) - {"situation", "file", "repository", "journal", "sizes"}
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
            scenario = raw.get("scenario")
            if scenario is not None and (not isinstance(scenario, str) or not scenario):
                raise ValueError(f"{path}: situation {name}: a scenario is a name, got {scenario!r}")
            workspace = raw.get("workspace", False)
            if not isinstance(workspace, bool):
                raise ValueError(f"{path}: situation {name}: workspace is true or false, got {workspace!r}")
            situations.append(Situation(name, raw.get("title", name), steps, scenario, workspace))
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
    journal = data.get("journal", []) if path.is_file() else []
    for item in [*files, *repositories]:
        relative = Path(item.get("path", ""))
        if not item.get("path") or relative.is_absolute() or ".." in relative.parts:
            raise ValueError(f"{path}: a home item's path is relative to the home: {item!r}")
    for staged in journal:
        unknown = set(staged) - {"minutes_ago", "priority", "unit", "identifier", "message", "pid"}
        if unknown:
            raise ValueError(f"{path}: a journal entry has no {sorted(unknown)}")
        if not isinstance(staged.get("minutes_ago"), int) or staged["minutes_ago"] < 0:
            raise ValueError(f"{path}: a journal entry says how many minutes ago it was written: {staged!r}")
        if staged.get("priority") not in range(8):
            raise ValueError(f"{path}: a journal entry's priority is 0 (emergency) to 7 (debug): {staged!r}")
        if not isinstance(staged.get("message"), str) or not staged["message"]:
            raise ValueError(f"{path}: a journal entry has a message: {staged!r}")
        for name in ("unit", "identifier"):
            if name in staged and not isinstance(staged[name], str):
                raise ValueError(f"{path}: a journal entry's {name} is text: {staged!r}")
        if "pid" in staged and (not isinstance(staged["pid"], int) or staged["pid"] < 1):
            raise ValueError(f"{path}: a journal entry's pid is a process id: {staged!r}")
    for item in files:
        if "base64" not in item:
            continue
        try:
            base64.b64decode(item["base64"], validate=True)
        except (ValueError, TypeError) as error:
            raise ValueError(f"{path}: a seeded file's base64 does not decode: {error}") from error
    return Plan(
        entry["name"], entry.get("id", entry["name"]), situations, files, repositories, journal, sizes,
    )


def seed_home(plan: Plan, home: Path, env: dict) -> None:
    """Put the plan's files and repositories in the profile's home."""
    for item in plan.files:
        target = home / item["path"]
        target.parent.mkdir(parents=True, exist_ok=True)
        if "base64" in item:
            target.write_bytes(base64.b64decode(item["base64"]))
        else:
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


def seed_journal(plan: Plan, root: Path, env: dict) -> None:
    """Put the plan's journal where the profile's daemon reads it, in place of the machine's own:
    the `journalctl -o json` lines `TILO_JOURNAL_FIXTURE` names, oldest first."""
    if not plan.journal:
        return
    now = int(time.time() * 1_000_000)
    lines = []
    for at, entry in enumerate(sorted(plan.journal, key=lambda entry: -entry["minutes_ago"])):
        written = now - entry["minutes_ago"] * 60_000_000
        record = {
            "__REALTIME_TIMESTAMP": str(written),
            "__CURSOR": f"s=fixture;i={at:06d}",
            "_SYSTEMD_UNIT": entry.get("unit", ""),
            "SYSLOG_IDENTIFIER": entry.get("identifier", entry.get("unit", "")),
            "PRIORITY": str(entry["priority"]),
            "MESSAGE": entry["message"],
            "_HOSTNAME": "studio",
            "_BOOT_ID": "fixture",
            "_TRANSPORT": "journal",
        }
        if "pid" in entry:
            record["_PID"] = str(entry["pid"])
        lines.append(json.dumps(record))
    path = root / "journal.jsonl"
    path.write_text("\n".join(lines) + "\n")
    env["TILO_JOURNAL_FIXTURE"] = str(path)


def resolve_strings(value, replace):
    """`replace` applied to every string in a step."""
    if isinstance(value, str):
        return replace(value)
    if isinstance(value, list):
        return [resolve_strings(item, replace) for item in value]
    if isinstance(value, dict):
        return {key: resolve_strings(item, replace) for key, item in value.items()}
    return value


def profile_env(root: Path) -> dict:
    """A fresh profile: nothing of the caller's session reaches the app."""
    env = {
        key: value
        for key, value in os.environ.items()
        if key in {"PATH", "LANG", "LC_ALL", "TZ", "RUST_BACKTRACE", "RUST_LOG",
                   "VK_DRIVER_FILES", "VK_ICD_FILENAMES", "WGPU_BACKEND",
                   "WGPU_ADAPTER_NAME", "TILO_PDFIUM_LIB", "TILO_CHROMIUM_HOST"}
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
    # The profile is a test beside this machine's own session, not a session that owns it: no
    # launch asks the machine's systemd user manager for a scope (there is none here), and none
    # reaches the host's daemons. A launch that asked anyway would print why it runs outside a
    # scope into the capture's log, where it reads as the capture's failure.
    env["TILO_MACHINE"] = "hosted"
    # No display and no bus: a bus address nothing listens on, so an app
    # that looks for one fails fast instead of reaching the caller's.
    env["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path={root / 'run' / 'no-bus'}"
    env["GSETTINGS_BACKEND"] = "memory"
    env.setdefault("LANG", "en_US.UTF-8")
    env.setdefault("TZ", "Europe/Berlin")
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


def daemon_tail(log: Path, lines: int = 120) -> str:
    """What the disposable profile's daemon said last: the app's own output lands there."""
    try:
        text = log.read_text(errors="replace")
    except OSError:
        return ""
    return "\n".join(text.strip().splitlines()[-lines:])


def daemon_found(socket: Path) -> tuple[str, str]:
    """Found the person at `socket`, the call a computer's first `tilo` command makes: the daemon
    keeps their root and their workspaces, and the connection is theirs from then on."""
    ask = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "tilo_do", "arguments": {"path": "devices#found", "args": {}}},
    }
    try:
        with socket_module.socket(socket_module.AF_UNIX) as native:
            native.settimeout(120)
            native.connect(str(socket))
            native.sendall((json.dumps(ask) + "\n").encode())
            answer = native.makefile().readline()
    except OSError as error:
        return f"the device did not found its person: {error}", ""
    if '"error"' in answer or '"isError":true' in answer.replace(" ", ""):
        if "this device is a person's already" not in answer:
            return f"the device founded no person: {answer.strip()}", answer
        return "", answer
    return "", answer


def daemon_serving(socket: Path) -> str:
    """The workspace the daemon at `socket` serves: empty while its person's part is starting."""
    try:
        with socket_module.socket(socket_module.AF_UNIX) as native:
            native.settimeout(5)
            native.connect(str(socket))
            native.sendall(b'{"native":"status"}\n')
            answer = native.makefile().readline()
        return str(json.loads(answer).get("workspace") or "")
    except (OSError, ValueError):
        return ""


def staging_source(runtime: Path, plan: Plan) -> tuple[Path, str] | None:
    """Where `plan`'s tilolang app is built from, and the runtime its package names: what
    tools/screenshots/build.sh wrote for the app, which has no development build of its own
    until the device running it names one."""
    try:
        rows = (runtime / "apps.tsv").read_text().splitlines()
    except OSError:
        return None
    for row in rows:
        name, _, rest = row.partition("\t")
        if name != plan.app:
            continue
        directory, _, runtime_id = rest.rpartition("\t")
        return Path(directory), runtime_id
    return None


class Session:
    """One device as a person's computer starts an app: a disposable home's daemon founds its
    person and installs the build tools/screenshots/build.sh staged for the plan as their own
    development build, once. Every situation that reads nothing another one wrote is then
    captured against it (`tilo-runtime --run-app … --headless-drive`), with the connectors the
    device runs for the app's origins."""

    def __init__(self, args, plan: Plan):
        self.args = args
        self.plan = plan
        self.daemon = None
        self.app = None
        # How long each part of starting the device took, for the summary.
        self.phases: dict[str, float] = {}
        # A home is where its daemon keeps its sockets, and a socket's path must fit 103 bytes:
        # the directory it lives in is as short as one can be, in the system's own temp.
        self._tmp = tempfile.TemporaryDirectory(prefix="t-", dir="/tmp")
        self.tmp = Path(self._tmp.name)
        self.log = self.tmp / "daemon.log"
        self.env = profile_env(self.tmp / "profile")
        self.home = Path(self.env["HOME"])
        seed_home(plan, self.home, self.env)
        seed_journal(plan, self.tmp, self.env)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        if self.daemon is not None:
            self.daemon.terminate()
            try:
                self.daemon.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.daemon.kill()
                self.daemon.wait()
        self._tmp.cleanup()

    def _phase(self, name: str, started: float) -> None:
        self.phases[name] = round(time.monotonic() - started, 1)

    def start(self) -> tuple[object, str]:
        """Start the daemon, found its person, build and install the app: 0 and nothing said
        when the device is ready to capture, otherwise what went wrong and the logs that say so."""
        try:
            return self._start()
        except subprocess.TimeoutExpired as expired:
            said = expired.stdout or b""
            if isinstance(said, bytes):
                said = said.decode(errors="replace")
            return "timeout", said + "\n" + daemon_tail(self.log)
        except OSError as error:
            return "could not start", str(error)

    def _start(self) -> tuple[object, str]:
        args, plan, tmp = self.args, self.plan, self.tmp
        runtime = args.bin_dir.parent
        # A tilolang app is built for this device below; a native app's build is staged whole.
        source = staging_source(runtime, plan)
        native = runtime / "install" / plan.app
        if source is None and not (native / "tilo-app.toml").is_file():
            return "no staged build", (
                f"{runtime} stages no build of {plan.app}: rerun tools/screenshots/build.sh")
        env = self.env
        # The person's own device, as a computer's first command founds it: no profile named
        # apart, and no certificate made before it starts, so the connection to its socket is the
        # person's own and a development build is theirs to install. Its native endpoint is the
        # one every tool finds, under XDG_RUNTIME_DIR, short enough for a socket's path.
        env.pop("TILO_RUNTIME_PROFILE", None)
        env.pop("TILO_IDENTITY_ROOT", None)
        env.pop("TILO_APPS_DATA_DIR", None)
        # No display: a daemon started with one is a session's, and founds no person of its own.
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("DISPLAY", None)
        run_dir = Path(env["XDG_RUNTIME_DIR"])
        (run_dir / "tilo").mkdir(parents=True, exist_ok=True)
        (run_dir / "tilo").chmod(0o700)
        (run_dir / "wayland-0").write_bytes(b"")
        socket = run_dir / "tilo" / "native.sock"
        env.update({
            "TILO_RUNTIME_BUNDLE_DIR": str(runtime / "runtime"),
            "TILO_LAUNCHER_BIN": str(args.bin_dir / "tilo-launcher"),
            "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE": str(runtime / "runtime-publishers.txt"),
        })
        # The device looks for a runtime in its own store (`<data>/runtimes/<version>`), which its
        # services reach with a data directory of their own rather than the environment this
        # script starts it with: the bundles this build baked are linked where a device keeps
        # them, under every data directory a service of it may resolve, so an install finds the
        # runtime an app pins (Browser's Chromium engine) and the launcher starts that app on it.
        candidates = [
            Path(env["XDG_DATA_HOME"]) / "tilo-apps",
            self.home / ".local" / "share" / "tilo-apps",
        ]
        bundles = [bundle for bundle in sorted((runtime / "runtime").iterdir())
                   if (bundle / "bundle.toml").is_file()]
        for data in candidates:
            store = data / "runtimes"
            store.mkdir(parents=True, exist_ok=True)
            for bundle in bundles:
                linked = store / bundle.name
                if not linked.exists():
                    linked.symlink_to(bundle, target_is_directory=True)
        print(f"     {plan.app}: runtimes linked as {[b.name for b in bundles]}", flush=True)
        started = time.monotonic()
        with self.log.open("wb") as out:
            self.daemon = subprocess.Popen(
                [str(args.bin_dir / "tilo-runtime")], env=env, cwd=tmp,
                stdin=subprocess.DEVNULL, stdout=out, stderr=out,
                start_new_session=True,
            )
        deadline = time.monotonic() + 120
        while not socket.exists():
            if self.daemon.poll() is not None:
                return "no daemon", "the daemon ended as it started\n" + daemon_tail(self.log)
            if time.monotonic() >= deadline:
                return "no daemon", f"no socket at {socket}\n" + daemon_tail(self.log)
            time.sleep(0.1)
        self._phase("daemon", started)
        # The daemon's person is founded once its services are up: the call is the first one a
        # computer's `tilo` makes, and it is asked again while the daemon is still starting.
        started = time.monotonic()
        deadline = time.monotonic() + 120
        founded, _ = daemon_found(socket)
        while founded and time.monotonic() < deadline:
            time.sleep(0.25)
            founded, _ = daemon_found(socket)
        if founded:
            return "no person", founded + "\n" + daemon_tail(self.log)
        self._phase("found", started)
        # The device answers before its person's part runs, but the build and the install are
        # the person's own act: `tilolang build --dev` asks the daemon for the id it compiles
        # the app as, and an answer before that part runs is the device's alone, which refuses a
        # development build. Both come once it serves the workspace it starts.
        started = time.monotonic()
        deadline = time.monotonic() + 120
        while not daemon_serving(socket):
            if self.daemon.poll() is not None or time.monotonic() >= deadline:
                return "no workspace", "the device serves no workspace\n" + daemon_tail(self.log)
            time.sleep(0.1)
        self._phase("serving", started)
        if source is not None:
            # The development build is compiled for the id this device's daemon names for it,
            # which only that daemon answers: `tilolang build --dev` asks it, in this profile's
            # environment. Until the person's part answers, the device answers as itself — the
            # system's, which refuses a development build — so it is asked again while that part
            # comes up, and no longer.
            source_dir, runtime_id = source
            started = time.monotonic()
            deadline = time.monotonic() + 180
            while True:
                built = subprocess.run(
                    [str(args.bin_dir / "tilolang"), "build", str(source_dir), "--dev",
                     "--search", str(source_dir.parent), "--out", str(tmp),
                     "--tilo-runtime", runtime_id],
                    # From the app's own directory: the tool finds this checkout, and the packages
                    # the app reads, by it.
                    env=env, cwd=source_dir, capture_output=True, text=True, timeout=900,
                )
                refused = "a development build is installed by the person" in (
                    built.stdout + built.stderr)
                if built.returncode == 0 or not refused or time.monotonic() >= deadline:
                    break
                time.sleep(0.5)
            if built.returncode != 0:
                # The build's own words last: a log keeps its end, and the daemon's is long.
                return "could not build", (
                    daemon_tail(self.log) + "\n— the build's own output —\n"
                    + built.stdout + built.stderr)
            self._phase("build", started)
        # The staging holds the app and every package it reads, each compiled for the id this
        # device names it: all of them are installed, as `tilolang dev` installs them, and the
        # app is the one of its own id.
        started = time.monotonic()
        if source is None:
            app_dir, staged = native.name, [native]
        else:
            app_dir = plan.app_id.rsplit(".", 1)[-1]
            staged = sorted(
                path for path in (tmp / "dev").glob("*") if (path / "tilo-app.toml").is_file()
            )
            if not any(path.name == app_dir for path in staged):
                return "no staged build", (
                    f"{(tmp / 'dev') / app_dir} is not the app's staging; the build wrote "
                    f"{[path.name for path in staged]}\n" + built.stdout + built.stderr)
        ids = {}
        # The person's part starts beside the daemon: the installer is asked again while it does,
        # rather than failing a capture the daemon is ready for a moment later.
        deadline = time.monotonic() + 180
        for package in staged:
            named = tmp / f"id-{package.name}"
            install = [str(args.bin_dir / "tilo-install"), "install", "--dev", str(package),
                       "--id-file", str(named)]
            done = subprocess.run(install, env=env, cwd=tmp, capture_output=True, text=True, timeout=300)
            while (
                "has no person signed in yet" in done.stdout + done.stderr
                and time.monotonic() < deadline
            ):
                time.sleep(0.5)
                done = subprocess.run(install, env=env, cwd=tmp, capture_output=True, text=True, timeout=300)
            if done.returncode != 0 or not named.is_file() or not named.read_text().strip():
                return done.returncode or "no installed id", (
                    daemon_tail(self.log) + "\n— the installer's own output —\n"
                    + done.stdout + done.stderr)
            ids[package.name] = named.read_text().strip()
        self._phase("install", started)
        self.app = ids[app_dir]
        return 0, ""

    def capture(self, spec: dict) -> tuple[object, str]:
        """Run one situation's capture on the started device: its exit status, and what the
        daemon and the app said."""
        try:
            done = subprocess.run(
                [str(self.args.bin_dir / "tilo-runtime"), "--run-app", self.app,
                 "--headless-drive", json.dumps(spec)],
                env=self.env, cwd=self.tmp, capture_output=True, text=True, timeout=RUN_SECONDS,
            )
        except subprocess.TimeoutExpired as expired:
            said = expired.stdout or b""
            if isinstance(said, bytes):
                said = said.decode(errors="replace")
            return "timeout", said + "\n" + daemon_tail(self.log)
        except OSError as error:
            return "could not start", str(error)
        # The app's own words last: a log keeps its end, and the daemon's is long.
        return done.returncode, daemon_tail(self.log) + "\n— the app's own output —\n" + done.stdout + done.stderr


def shares_a_session(situation: Situation) -> bool:
    """Whether a situation can be captured on the device its app's other situations use: one
    built from a scenario with no steps reads that scenario's fixtures and changes nothing, and
    one without steps changes nothing. A situation whose steps change anything — the person's
    workspace, or the app's own records — gets a device of its own, so no picture shows what
    another situation wrote."""
    if situation.workspace or situation.steps:
        return False
    return True


def run_situation(session: Session, situation: Situation, first: bool) -> dict:
    plan, args = session.plan, session.args
    app_dir = args.out / plan.app
    app_dir.mkdir(parents=True, exist_ok=True)
    shots = captures(app_dir, situation, plan.sizes)
    # The drive's results land in a directory of their own: the daemon grants the capture the
    # directories it writes into, and never one that holds the profile's home.
    reports = session.tmp / "reports" / situation.name
    reports.mkdir(parents=True)
    spec = {
        "viewport": list(VIEWPORTS["wide"]),
        # `${HOME}` is the profile's home, and an address a plan writes under the app's id
        # (`tl:so.tilo.calendar:root-…`) names the development build this device installed of it.
        "actions": resolve_strings(
            resolve_dates(situation.steps, datetime.date.today()),
            lambda text: text.replace("${HOME}", str(session.home))
            .replace(f"tl:{plan.app_id}:", f"tl:{session.app}:"),
        ),
        "results": str(reports / "results.json"),
        "capture": shots,
        # The desktop's window bar over the app, as a Tilo session draws
        # it: the app's slots, search and History are hosted there.
        "window_bar": True,
    }
    if situation.scenario:
        spec["scenario"] = situation.scenario
    if situation.workspace:
        spec["workspace"] = True
    if first:
        spec["resources"] = str(app_dir / "resources.json")
        # Every widget as the window laid it out, with what it says to assistive technology: the
        # names, identities and targets a plan presses by, where a person finds them.
        spec["inspect"] = str(app_dir / "inspect.json")
    started = time.monotonic()
    status, log = session.capture(spec)
    seconds = round(time.monotonic() - started, 1)
    results = reports / "results.json"
    if results.is_file():
        shutil.copyfile(results, app_dir / f"{situation.name}-results.json")
    return situation_outcome(app_dir, plan, situation, shots, status, log, seconds)


def situation_outcome(
    app_dir: Path, plan: Plan, situation: Situation, shots: list, status, log: str, seconds: float,
) -> dict:
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


def run_device(plan: Plan, situations: list, args, first: str) -> tuple[list, dict]:
    """Start one device for `plan` and capture `situations` on it, as many at once as `--jobs`
    allows: their outcomes, and how long starting the device took."""
    with Session(args, plan) as session:
        started = time.monotonic()
        status, log = session.start()
        phases = {**session.phases, "start": round(time.monotonic() - started, 1)}
        if status != 0:
            app_dir = args.out / plan.app
            app_dir.mkdir(parents=True, exist_ok=True)
            return [
                situation_outcome(
                    app_dir, plan, situation, captures(app_dir, situation, plan.sizes), status, log, 0.0,
                )
                for situation in situations
            ], phases
        with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, args.jobs)) as pool:
            futures = [
                pool.submit(run_situation, session, situation, situation.name == first)
                for situation in situations
            ]
            return [future.result() for future in futures], phases


def pin_bundles(args) -> None:
    """Keep the runtimes this build mounts for the whole run: a collection runs after every
    install, and the engine bundle no package installs yet would be collected by one app's
    device before the app that needs it opens."""
    root = args.bin_dir.parent / "runtime"
    if not root.is_dir():
        return
    env = {**os.environ, "TILO_RUNTIME_BUNDLE_DIR": str(root)}
    with tempfile.TemporaryDirectory(prefix="t-pin-") as store:
        for bundle in sorted(path for path in root.iterdir() if (path / "bundle.toml").is_file()):
            pinned = subprocess.run(
                [str(args.bin_dir / "tilo-install"), "--offline-store", store,
                 "runtime", "pin", bundle.name],
                env=env, capture_output=True, text=True)
            if pinned.returncode != 0:
                print(f"     {bundle.name}: not pinned ({pinned.stderr.strip()})", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--apps", default="", help="space-separated app directory names (default: all)")
    parser.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 2) // 2))
    args = parser.parse_args()
    args.bin_dir = args.bin_dir.resolve()
    args.out = args.out.resolve()

    entries = catalog()
    # A plan is found by the app's own name in the collection. One left under a name no app has
    # would be skipped without a word, and the app captured as it opens: fewer situations than
    # its plan asks for, and nothing said about it. So a plan whose name no app answers to
    # stops the run.
    named = {entry["name"] for entry in entries}
    orphans = sorted(path.stem for path in PLANS.glob("*.toml") if path.stem not in named)
    if orphans:
        parser.error(
            f"no app of the default collection is named {', '.join(orphans)}: "
            f"rename each plan in {PLANS} to the app it captures"
        )
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
    pin_bundles(args)

    started = time.monotonic()
    # One device per app for every situation that can share it, and one each for the rest.
    devices = []
    for plan in plans:
        first = plan.situations[0].name
        shared = [s for s in plan.situations if shares_a_session(s)]
        if shared:
            devices.append((plan, shared, first))
        devices.extend((plan, [s], first) for s in plan.situations if not shares_a_session(s))
    outcomes = []
    starts = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futures = [pool.submit(run_device, plan, situations, args, first) for plan, situations, first in devices]
        for future in concurrent.futures.as_completed(futures):
            done, phases = future.result()
            names = " ".join(o["situation"] for o in done)
            starts.append({"app": done[0]["app"], "situations": names, **phases})
            print(f"     {done[0]['app']}: device started in {phases['start']}s {phases} for {names}", flush=True)
            for outcome in done:
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
        "devices": sorted(starts, key=lambda d: (d["app"], d["situations"])),
        "failures": sorted(failures, key=lambda o: (o["app"], o["situation"])),
    }
    (args.out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    for outcome in summary["failures"]:
        print(f"FAIL {outcome['app']}/{outcome['situation']}: {outcome['title']}", flush=True)
        if outcome["status"] != 0:
            print(f"  exit status {outcome['status']}", flush=True)
        if outcome["missing"]:
            print(f"  missing pictures: {', '.join(outcome['missing'])}", flush=True)
        for line in outcome["log_tail"].splitlines():
            print(f"  | {line}", flush=True)
    print(
        f"{summary['shots']} screenshots of {summary['apps']} apps in {summary['seconds']}s; "
        f"{len(failures)} situation(s) failed (see {args.out / 'summary.json'})"
    )
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

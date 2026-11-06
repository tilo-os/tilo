"""Start either desktop headless and drive it through its own test-control socket.

Two sides, two ways to start, one vocabulary:

- `OldSide` runs the shell of a checkout that still has `crates/session/shell`
  (`shell --mode native --backend headless`), with its apps started as binaries
  beside it — the way that session ran.
- `NewSide` runs the session of a checkout that has `crates/session/session`,
  through that checkout's own `dist/dev/nested-session.sh`: the tree's apps
  installed as packages and opened through the session's own launcher.

Both sides then answer the same ops: `list_targets`, `click_named`, `key`,
`type`, `drag`, `snapshot`, `actions_enumerate`/`actions_invoke`, `windows`,
`state_dump`. A difference that is real (`seat_key` against `type`) lives in the
method that needs it; nothing else in the audit knows which side it drives.

Every session runs on its own profile, its own private session bus and a
throwaway identity: nothing here reads or writes a person's own desktop.
"""
from __future__ import annotations

import importlib.util
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

SIZE = (1280, 800)
# The tool's own root lives in the repository it sits in.
REPO = Path(__file__).resolve().parents[3]
# English labels on both sides, whatever the developer's locale is, so two runs
# name the same controls.
LOCALE = {"LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "LANGUAGE": "C"}
# The apps a session installs unless the run names its own: what an audit of a
# desktop's everyday work needs.
APPS = "notes file-explorer chat settings terminal"
# The home folder both sides start on: the same ordinary files, so a Files story
# has folders and files to show and the two sides show the same ones.
HOME_FILES = {
    "Notes.txt": """Packing for the coast
- the red tent
- headlamp, batteries
- the good coffee
""",
    "Documents/Project brief.md": """# Project brief

Bring the workshop notes, the two photos of the bay and the quote from the
printer. One page, no slides.
""",
    "Documents/Quarter plan.md": """# Next quarter

1. Finish the workshop write-up
2. Move the shared folder to the new drive
3. Two weeks of nothing at the end of March
""",
    "Documents/Receipts/2025-11 rent.txt": """Rent, November
Paid 1 November, bank transfer
""",
    "Downloads/Invoice 0042.txt": """Invoice 0042
Workshop shelving, 4 units
Due on the 15th
""",
}


class Failed(Exception):
    """A step did not get what it waited for."""


class DoorMissing(Failed):
    """The side has no door for this: the run records it instead of the state."""


# --- plumbing shared by both sides -------------------------------------------


class Control:
    """The newline-JSON test-control socket both sides serve."""

    def __init__(self, path: Path, timeout: float = 30.0):
        self.path = Path(path)
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        self.sock.connect(str(self.path))
        self.buf = b""

    def send(self, payload: dict, timeout: float | None = None) -> dict:
        if timeout is not None:
            self.sock.settimeout(timeout)
        self.sock.sendall((json.dumps(payload) + "\n").encode())
        while b"\n" not in self.buf:
            chunk = self.sock.recv(1 << 20)
            if not chunk:
                raise ConnectionError("control socket closed")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass


def reply_body(reply: dict):
    """The payload of a reply, whose one key names its shape."""
    if not isinstance(reply, dict):
        return reply
    for key in ("answer", "ok", "state", "targets", "windows"):
        if key in reply:
            return reply[key]
    return reply


def wait_socket(path: Path, seconds: float) -> float | None:
    started = time.monotonic()
    while time.monotonic() - started < seconds:
        if path.exists():
            return time.monotonic() - started
        time.sleep(0.02)
    return None


def stop_tree(pid: int, timeout: float = 8.0) -> None:
    """End the process group a session was started in, and wait until it is gone."""
    if pid <= 0 or not Path(f"/proc/{pid}").exists():
        return
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(pid, sig)
        except ProcessLookupError:
            return
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if not Path(f"/proc/{pid}").exists():
                return
            time.sleep(0.05)


def unmount_under(root: Path) -> None:
    """Release every mount a run's sandbox left under `root`, so the profile goes."""
    try:
        mounts = Path("/proc/self/mounts").read_text().splitlines()
    except OSError:
        return
    prefix = str(root.resolve())
    for line in mounts:
        fields = line.split()
        if len(fields) < 2 or not fields[1].startswith(prefix):
            continue
        for command in (["fusermount3", "-u", "-z", fields[1]], ["umount", "-l", fields[1]]):
            try:
                if subprocess.run(command, capture_output=True, timeout=30).returncode == 0:
                    break
            except (OSError, subprocess.TimeoutExpired):
                continue


def git(repo: Path, *args: str) -> str:
    done = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True)
    if done.returncode != 0:
        raise Failed(f"git {' '.join(args)} failed: {done.stderr.strip()[-1000:]}")
    return done.stdout.strip()


def commit_of(repo: Path, ref: str) -> str:
    return git(repo, "rev-parse", ref)


class Side:
    """What a run drives: a session started, and the ops both sessions answer."""

    name = ""

    def __init__(self, ref: str, root: Path, *, size=SIZE, theme: str | None = "dark",
                 apps: str = APPS, bins: Path | None = None, build: Path | None = None,
                 app_collection: Path | None = None, keep: bool = False):
        self.ref = ref
        self.sha = ""
        self.root = Path(root) / self.name
        self.size = size
        self.theme = theme
        self.apps = apps
        self.given_bins = Path(bins) if bins else None
        self.given_build = Path(build) if build else None
        self.app_collection = Path(app_collection) if app_collection else None
        self.keep = keep
        self.proc: subprocess.Popen | None = None
        self.log = None
        self.ctl: Control | None = None
        self.started_at = 0.0
        self.details: dict = {}
        self.app_windows: dict[str, dict] = {}

    # -- lifecycle --

    def start(self, *, fresh: bool = True) -> dict:
        raise NotImplementedError

    def seed_home(self, home: Path) -> None:
        """Fill the home folder the session starts on with ordinary files, the
        same on both sides: a Files story shows files, not an empty room."""
        for relative, text in HOME_FILES.items():
            path = home / relative
            if path.exists():
                continue
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        self.details["home"] = str(home)

    def stop(self) -> None:
        raise NotImplementedError

    def restart(self, *, fresh: bool = True) -> dict:
        self.stop()
        return self.start(fresh=fresh)

    # -- ops both sides answer --

    def request(self, payload: dict, timeout: float = 30.0) -> dict:
        raise NotImplementedError

    def send(self, payload: dict, timeout: float = 30.0) -> dict:
        return self.request(payload, timeout)

    def ok(self, payload: dict, timeout: float = 30.0) -> dict:
        """Send an op and answer its body, raising the side's own refusal."""
        reply = self.request(payload, timeout)
        if isinstance(reply, dict) and "err" in reply:
            raise Failed(f"{payload.get('op')} refused: {json.dumps(reply['err'])[:800]}")
        return reply_body(reply)

    def targets(self) -> list[dict]:
        body = self.ok({"op": "list_targets"})
        rows = body.get("targets", []) if isinstance(body, dict) else body
        return [row for row in rows or [] if isinstance(row, dict)
                and (row.get("rect") or [0, 0, 0, 0])[2] > 0 and (row.get("rect") or [0, 0, 0, 0])[3] > 0]

    def target(self, *needles: str) -> dict | None:
        for row in self.targets():
            name = str(row.get("name") or "").lower()
            if all(needle.lower() in name for needle in needles):
                return row
        return None

    def target_names(self) -> list[str]:
        return [str(row.get("name")) for row in self.targets()]

    def click_named(self, name: str) -> dict:
        return self.ok({"op": "click_named", "name": name})

    def click(self, x: float, y: float) -> dict:
        return self.ok({"op": "click_at", "x": float(x), "y": float(y)})

    def move(self, x: float, y: float) -> dict:
        return self.ok({"op": "pointer_move", "x": float(x), "y": float(y)})

    def button(self, which: str = "left", pressed: bool = True) -> dict:
        return self.ok({"op": "pointer_button", "button": which, "pressed": pressed})

    def right_click(self, x: float, y: float) -> None:
        self.move(x, y)
        time.sleep(0.15)
        self.button("right", True)
        self.button("right", False)

    def drag(self, x_from: float, y_from: float, x_to: float, y_to: float,
             steps: int = 12) -> dict:
        return self.ok({"op": "drag", "x_from": float(x_from), "y_from": float(y_from),
                        "x_to": float(x_to), "y_to": float(y_to), "steps": steps})

    def scroll(self, dx: float = 0, dy: float = 0) -> dict:
        return self.ok({"op": "scroll", "dx": float(dx), "dy": float(dy)})

    def key(self, name: str) -> dict:
        """A key the desktop itself answers, from its own keymap."""
        return self.ok({"op": "key", "key": name})

    def key_down(self, name: str) -> dict:
        return self.ok({"op": "key_down", "key": name})

    def key_up(self, name: str) -> dict:
        return self.ok({"op": "key_up", "key": name})

    def modifiers(self, ctrl=False, shift=False, alt=False, logo=False) -> dict:
        return self.ok({"op": "modifiers", "ctrl": ctrl, "shift": shift,
                        "alt": alt, "logo": logo})

    def chord(self, key: str, *, ctrl=False, shift=False, alt=False, logo=False) -> None:
        self.modifiers(ctrl=ctrl, shift=shift, alt=alt, logo=logo)
        try:
            self.key(key)
        finally:
            self.modifiers()

    def type_text(self, text: str) -> dict:
        """Text as the desktop's own fields read it."""
        return self.ok({"op": "type", "text": text}, timeout=60)

    def client_key(self, key: str) -> dict:
        """One key as a keyboard delivers it, to the focused client."""
        raise NotImplementedError

    def client_type(self, text: str) -> None:
        raise NotImplementedError

    def windows(self) -> list[dict]:
        body = self.ok({"op": "windows"})
        rows = body.get("windows", body.get("rows", [])) if isinstance(body, dict) else body
        return rows if isinstance(rows, list) else []

    def state(self) -> dict:
        body = self.ok({"op": "state_dump"})
        return body if isinstance(body, dict) else {}

    def overlays(self) -> dict:
        chrome = self.state().get("chrome") or {}
        found = chrome.get("overlays")
        return found if isinstance(found, dict) else {}

    def snapshot(self, path: Path) -> Path:
        path = Path(path)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.unlink(missing_ok=True)
        self.ok({"op": "snapshot", "path": str(path)}, timeout=120)
        end = time.monotonic() + 30
        while time.monotonic() < end and not (path.exists() and path.stat().st_size > 0):
            time.sleep(0.05)
        if not path.exists():
            raise Failed(f"no frame written to {path}")
        return path

    # -- actions --

    def action_rows(self, query: str = "") -> list[dict]:
        body = self.ok({"op": "actions_enumerate", "query": query, "wait_ms": 4000,
                        "limit": 1000}, timeout=60)
        rows = body
        if isinstance(body, dict):
            rows = body.get("actions", body.get("entries", body.get("rows", [])))
        return [row for row in rows or [] if isinstance(row, dict)]

    @staticmethod
    def tail(row: dict) -> str:
        """The part of an action's id after the provider the listing splits by."""
        return str(row.get("id") or "").rsplit("\x1f", 1)[-1]

    def find_action(self, ident: str | None = None, *, contains: str | None = None,
                    provider: str | None = None) -> dict | None:
        """The action an id names: its tail after the provider, or the whole id
        when the caller spells the provider out (`desktop:chrome` + `open-app`)."""
        for row in self.action_rows():
            if provider is not None and row.get("provider") != provider:
                continue
            full = str(row.get("id") or "")
            tail = self.tail(row)
            if ident is not None:
                if "\x1f" in ident:
                    if full == ident or full.endswith(ident):
                        return row
                elif tail == ident:
                    return row
            if contains is not None and contains in tail:
                return row
        return None

    def invoke(self, row: dict, args: dict | None = None) -> dict:
        payload = {"op": "actions_invoke", "provider": row.get("provider"),
                   "id": row.get("id"), "args": args or {}}
        return self.ok(payload, timeout=90)

    def do(self, ident: str | None = None, args: dict | None = None, *,
           contains: str | None = None) -> dict:
        row = self.find_action(ident, contains=contains)
        if row is None:
            raise Failed(f"no action {ident or contains}")
        return self.invoke(row, args)

    def has_action(self, ident: str | None = None, *, contains: str | None = None) -> bool:
        return self.find_action(ident, contains=contains) is not None

    # -- turns --

    def launch_app(self, app: str) -> dict:
        raise NotImplementedError

    def wait_window(self, app: str, timeout: float = 90.0) -> dict:
        """The window of `app` once the desk holds it."""
        ident = app if "." in app else f"so.tilo.{'file-explorer' if app == 'files' else app}"
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            for row in self.windows():
                if row.get("app_id") == ident:
                    return row
            time.sleep(0.1)
        raise Failed(f"no window for {ident}")

    def wait_windows(self, count: int, timeout: float = 90.0) -> list[dict]:
        end = time.monotonic() + timeout
        rows: list[dict] = []
        while time.monotonic() < end:
            rows = self.windows()
            if len(rows) >= count:
                return rows
            time.sleep(0.1)
        raise Failed(f"{len(rows)} windows, expected {count}")

    def notify(self, summary: str, body: str = "", app: str = "Notes") -> str:
        raise NotImplementedError

    def bus_address(self) -> str | None:
        return None


# --- the shell of a checkout that still has it --------------------------------


class OldSide(Side):
    name = "old"

    def __init__(self, ref, root, **kwargs):
        super().__init__(ref, root, **kwargs)
        self.work = Path(root) / "refs" / "old"
        self.ctl_path = self.root / "run" / "test-control.sock"
        self.marks = self.root / "startup-marks.json"
        self.bus_pid = 0
        self.bus = ""
        self.keyring_pid = 0
        self.apps_up: list[dict] = []

    # -- build --

    def prepare(self) -> Path:
        """The shell and its app binaries for `ref`: given, cached, or built."""
        self.sha = commit_of(REPO, self.ref)
        if self.given_bins:
            self.details["binary_ref"] = str(self.given_bins)
            return self.given_bins
        out = REPO / "target" / "shell-audit" / "old" / self.sha[:12]
        if not (out / "shell").exists():
            tree = self.work
            if not (tree / "crates/session/shell").is_dir():
                if tree.exists():
                    git(REPO, "worktree", "remove", "--force", str(tree))
                git(REPO, "worktree", "add", "--detach", str(tree), self.sha)
            # Whatever cargo this tree builds with (a wrapper, a remote build).
            cargo = os.environ.get("SHELL_AUDIT_CARGO", "cargo").split()
            self.say(f"building the shell of {self.ref} ({self.sha[:12]})")
            done = subprocess.run(
                [*cargo, "build", "--release", "--locked", "-p", "shell", "-p", "notes",
                 "-p", "chat", "-p", "file-explorer", "-p", "terminal", "-p", "tilo-runtime"],
                cwd=tree, text=True)
            if done.returncode != 0:
                raise Failed(f"the shell of {self.ref} did not build")
            out.mkdir(parents=True, exist_ok=True)
            for name in ("shell", "notes", "chat", "file-explorer", "terminal",
                         "tilo-runtime-mcp"):
                source = tree / "target" / "release" / name
                if source.exists():
                    shutil.copy2(source, out / name)
        self.details["binary_ref"] = self.sha
        return out

    def say(self, message: str) -> None:
        print(f"    [{self.name}] {message}", file=sys.stderr, flush=True)

    # -- lifecycle --

    def start(self, *, fresh: bool = True) -> dict:
        bins = self.prepare()
        self.root.mkdir(parents=True, exist_ok=True)
        for name in ("run", "home", "config", "data", "cache", "state"):
            (self.root / name).mkdir(parents=True, exist_ok=True, mode=0o700)
        self.seed_home(self.root / "home")
        env = self.environment(bins)
        started = time.monotonic()
        self.start_bus(env)
        provision = subprocess.run([str(bins / "shell"), "--provision-identity",
                                    "--disable-peer-sync"],
                                   env=env, capture_output=True, text=True, timeout=600)
        if provision.returncode != 0:
            raise Failed(f"the old shell would not provision: {provision.stderr[-2000:]}")
        self.log = (self.root / "session.log").open("wb")
        self.proc = subprocess.Popen(
            [str(bins / "shell"), "--mode", "native", "--backend", "headless",
             "--disable-peer-sync"],
            env=env, stdout=self.log, stderr=subprocess.STDOUT, start_new_session=True)
        (self.root / "session.pid").write_text(str(self.proc.pid))
        waited = wait_socket(self.ctl_path, 240)
        if waited is None:
            raise Failed("the old shell bound no control socket:\n"
                         + (self.root / "session.log").read_text(errors="replace")[-4000:])
        self.ctl = Control(self.ctl_path)
        # A headless old shell only rasterizes under a viewer lease.
        self.ctl.send({"op": "watch", "watching": True}, timeout=20)
        self.started_at = time.monotonic()
        self.details.update({"socket_s": round(waited, 2), "binary": str(bins)})
        self.dismiss_onboarding()
        self.apply_theme()
        return {"seconds": round(time.monotonic() - started, 2), **self.details}

    def environment(self, bins: Path) -> dict:
        env = dict(os.environ)
        for name in ("WAYLAND_DISPLAY", "DISPLAY", "TILO_TEST_CONTROL", "TILO_RUN_SECONDS",
                     "TILO_STARTUP_METRICS_PATH", "TILO_FRAME_TRACE", "TILO_FRAME_SUMMARY",
                     "TILO_ADMIN_CONTROL", "TILO_APPS_SYSTEM_DIR", "DBUS_SESSION_BUS_ADDRESS",
                     "XDG_SESSION_TYPE", "XDG_SESSION_ID", "XDG_SEAT", "XDG_VTNR",
                     "XDG_CURRENT_DESKTOP", "XDG_SESSION_DESKTOP"):
            env.pop(name, None)
        env.update({
            "HOME": str(self.root / "home"),
            "XDG_CONFIG_HOME": str(self.root / "config"),
            "XDG_DATA_HOME": str(self.root / "data"),
            "XDG_CACHE_HOME": str(self.root / "cache"),
            "XDG_STATE_HOME": str(self.root / "state"),
            "XDG_RUNTIME_DIR": str(self.root / "run"),
            "TILO_TEST_CONTROL": str(self.ctl_path),
            "TILO_STARTUP_METRICS_PATH": str(self.marks),
            "TILO_APPS_DATA_DIR": str(self.root / "data" / "tilo" / "apps"),
            "TILO_STATE_PATH": str(self.root / "config" / "state.json"),
            "TILO_IDENTITY_ROOT": str(self.root / "identity-root"),
            "TILO_PEER_KEY_PATH": str(self.root / "keys" / "peer-key"),
            "TILO_HEADLESS_SIZE": f"{self.size[0]}x{self.size[1]}",
            "TILO_REFRESH_HZ": "60",
            "RUST_BACKTRACE": "1",
            "RUST_LOG": os.environ.get("RUST_LOG", "info"),
            **LOCALE,
        })
        if self.app_collection:
            env["TILO_APPS_SYSTEM_DIR"] = str(self.app_collection)
        return env

    def start_bus(self, env: dict) -> None:
        """A private session bus and a Secret Service on it: a session with an
        identity asks for the device's secrets as it starts."""
        run = self.root / "run"
        run.mkdir(parents=True, exist_ok=True)
        socket_path = run / "bus"
        done = subprocess.run(["dbus-daemon", "--session", "--fork",
                               f"--address=unix:path={socket_path}",
                               "--print-address=1", "--print-pid=1"],
                              capture_output=True, text=True, timeout=30)
        if done.returncode != 0:
            raise Failed(f"dbus-daemon failed: {done.stderr}")
        address, pid = done.stdout.strip().splitlines()[:2]
        self.bus_pid = int(pid)
        self.bus = address.strip()
        env["DBUS_SESSION_BUS_ADDRESS"] = self.bus
        keyring = subprocess.Popen(
            ["gnome-keyring-daemon", "--start", "--components=secrets", "--foreground"],
            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT,
            start_new_session=True)
        self.keyring_pid = keyring.pid
        time.sleep(1.5)

    def bus_address(self) -> str | None:
        return self.bus or None

    @property
    def wayland_runtime(self) -> Path | None:
        return self.root / "run" if self.proc is not None else None

    def stop(self) -> None:
        for entry in self.apps_up:
            stop_tree(entry["pid"])
        self.apps_up = []
        if self.ctl is not None:
            self.ctl.close()
            self.ctl = None
        if self.proc is not None:
            stop_tree(self.proc.pid)
            self.proc = None
        for pid in (self.keyring_pid, self.bus_pid):
            stop_tree(pid)
        self.keyring_pid = self.bus_pid = 0
        if self.log is not None:
            self.log.close()
            self.log = None
        if not self.keep:
            unmount_under(self.root)
            shutil.rmtree(self.root, ignore_errors=True)

    # -- ops --

    def request(self, payload: dict, timeout: float = 30.0) -> dict:
        if self.ctl is None:
            raise Failed("the old shell is not running")
        return self.ctl.send(payload, timeout=timeout)

    def client_key(self, key: str) -> dict:
        return self.ok({"op": "seat_key", "key": key})

    def client_type(self, text: str) -> None:
        for char in text:
            self.client_key(char)
            time.sleep(0.01)

    # -- apps --

    def launch_app(self, app: str) -> dict:
        bins = Path(self.details["binary"])
        name = "file-explorer" if app == "files" else app
        binary = bins / name
        if not binary.exists():
            raise DoorMissing(f"the old side has no {name} app binary (it has "
                              f"{', '.join(sorted(entry.name for entry in bins.iterdir() if entry.is_file() and entry.stat().st_mode & 0o111))})")
        socket_path = None
        for entry in sorted((self.root / "run").iterdir()):
            if entry.name.startswith("wayland-") and not entry.name.endswith(".lock"):
                socket_path = entry
        if socket_path is None:
            raise Failed("the old shell offers no wayland socket")
        env = dict(self.environment(bins))
        env["WAYLAND_DISPLAY"] = socket_path.name
        env["XDG_RUNTIME_DIR"] = str(socket_path.parent)
        log = (self.root / f"app-{name}.log").open("wb")
        proc = subprocess.Popen([str(binary)], env=env, stdout=log, stderr=subprocess.STDOUT,
                                start_new_session=True)
        entry = {"name": name, "pid": proc.pid}
        self.apps_up.append(entry)
        (self.root / f"app-{name}.pid").write_text(str(proc.pid))
        self.wait_window(app)
        return entry

    def notify(self, summary: str, body: str = "", app: str = "Notes") -> str:
        env = dict(os.environ)
        env.update(LOCALE)
        env["DBUS_SESSION_BUS_ADDRESS"] = self.bus
        done = subprocess.run(["notify-send", "--app-name", app, "--icon",
                               "dialog-information", summary, body],
                              env=env, capture_output=True, text=True, timeout=30)
        return done.stdout or done.stderr

    # -- chrome --

    def dismiss_onboarding(self, timeout: float = 25.0) -> bool:
        steps = ("onboarding.begin.button", "onboarding.understood.row",
                 "onboarding.create.button", "onboarding.backup.skip-acknowledged.row",
                 "onboarding.backup.continue.button", "onboarding.tour.skip.button",
                 "onboarding.finish.button")
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            names = set(self.target_names())
            onboarding = sorted(name for name in names if name.startswith("onboarding."))
            if not onboarding:
                return True
            for name in steps:
                if name in names:
                    self.click_named(name)
                    break
            else:
                return False
            time.sleep(0.2)
        return False

    def apply_theme(self) -> None:
        if not self.theme:
            return
        row = self.find_action(f"appearance.theme.{self.theme}")
        if row is None:
            self.details["theme"] = f"no appearance.theme.{self.theme}"
            return
        try:
            self.invoke(row)
            self.details["theme"] = self.theme
        except Failed as error:
            self.details["theme"] = str(error)[:200]
        time.sleep(0.8)


# --- the session of a checkout that has it -----------------------------------


class NewSide(Side):
    name = "new"

    def __init__(self, ref, root, **kwargs):
        super().__init__(ref, root, **kwargs)
        self.real_apps = None
        self.session = None
        self.state_dir = self.root / "state"
        self.work = Path(root) / "refs" / "new"

    # -- checkout and build --

    def say(self, message: str) -> None:
        print(f"    [{self.name}] {message}", file=sys.stderr, flush=True)

    def prepare(self) -> Path:
        """The checkout of `ref`, with its session build ready."""
        if self.given_build:
            self.details["build"] = str(self.given_build)
            self.sha = commit_of(REPO, self.ref)
            return self.given_build
        self.sha = commit_of(REPO, self.ref)
        tree = self.work
        if not (tree / "crates/session/session").is_dir():
            if tree.exists():
                git(REPO, "worktree", "remove", "--force", str(tree))
            git(REPO, "worktree", "add", "--detach", str(tree), self.sha)
        else:
            head = git(tree, "rev-parse", "HEAD")
            if head != self.sha:
                git(tree, "checkout", "--detach", self.sha)
        build = Path(os.environ.get("TILO_NESTED_SESSION_BUILD")
                     or tree / "target" / "nested-session")
        if not (build / "bin/tilo-session").is_file():
            self.say(f"building the session of {self.ref} ({self.sha[:12]}) in {tree}")
            module = self.module(tree)
            try:
                module.build(apps=self.apps,
                             remote=os.environ.get("SHELL_AUDIT_NEW_BUILD_EXEC") or None)
            except module.Failed as error:
                raise Failed(str(error))
        return build

    def module(self, tree: Path):
        """The checkout's own `real_apps`, which starts a session as the image does."""
        path = tree / "scripts/headless-examples/real_apps.py"
        spec = importlib.util.spec_from_file_location(f"shell_audit_real_apps_{self.name}", path)
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        return module

    # -- lifecycle --

    def start(self, *, fresh: bool = True) -> dict:
        build = self.prepare()
        if self.given_build:
            tree = REPO
        else:
            tree = self.work
        self.real_apps = self.module(tree)
        self.real_apps.BUILD = build
        # The session script finds a build given this way too.
        os.environ["TILO_NESTED_SESSION_BUILD"] = str(build)
        if not (build / "bin/tilo-session").is_file():
            raise Failed(f"no session build in {build}; build it with "
                         f"dist/dev/nested-session-build.sh, or pass --new-build")
        self.root.mkdir(parents=True, exist_ok=True)
        self.state_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        started = time.monotonic()
        # The session script finds a build given this way too, and needs it
        # while it prepares the profile, before the session starts.
        os.environ["TILO_NESTED_SESSION_BUILD"] = str(build)
        remote = os.environ.get("SHELL_AUDIT_NEW_BUILD_EXEC") or None
        self.real_apps.prepare(self.state_dir, apps=self.apps, built=False, remote=remote)
        self.seed_home(self.state_dir / "home")
        session = self.real_apps.Session(self.state_dir)
        # A machine under a heavy load can take longer than the session script
        # waits for; the run asks for the session once more before it gives up
        # on the whole catalogue.
        try:
            session.start(apps=self.apps, fresh=fresh, out=self.state_dir, tag="audit",
                          env={**LOCALE, "RUST_LOG": os.environ.get("RUST_LOG", "warn")})
        except self.real_apps.Failed as error:
            if "timed out waiting for the session to start" not in str(error):
                raise Failed(str(error))
            self.say(f"the session did not come up in time, asking again: {str(error)[:160]}")
            session.stop()
            session = self.real_apps.Session(self.state_dir)
            try:
                session.start(apps=self.apps, fresh=fresh, out=self.state_dir, tag="audit",
                              env={**LOCALE, "RUST_LOG": os.environ.get("RUST_LOG", "warn")})
            except self.real_apps.Failed as again:
                raise Failed(str(again))
        self.session = session
        self.started_at = time.monotonic()
        self.details.update({"build": str(build), "binary_ref": self.sha})
        self.details["surfaces_missing"] = self.missing_surfaces()
        if self.details["surfaces_missing"]:
            self.say("!! this session started without part of its desktop: "
                     + "; ".join(self.details["surfaces_missing"]))
        self.dismiss_onboarding()
        self.apply_theme()
        return {"seconds": round(time.monotonic() - started, 2), **self.details}

    # What the session logs when a surface it starts is not there. A dev
    # session without one is a broken desktop, and every story about it is
    # judged against nothing, so a run says so instead of quietly capturing it.
    SURFACE_WARNINGS = ("the view bound to this slot is not installed here",
                        "no quick settings app is installed",
                        "no on-screen keyboard is installed",
                        "no lock screen",
                        "the audio service is not installed",
                        "the audio service did not start")

    def missing_surfaces(self) -> list[str]:
        log = self.state_dir / "logs" / "session.log"
        try:
            text = log.read_text(errors="replace")
        except OSError:
            return [f"no session log at {log}"]
        return list(dict.fromkeys(
            line.strip() for line in text.splitlines()
            if any(warning in line for warning in self.SURFACE_WARNINGS)))

    def stop(self) -> None:
        if self.session is not None:
            self.session.stop()
            self.session = None

    # -- ops --

    def request(self, payload: dict, timeout: float = 30.0) -> dict:
        if self.session is None:
            raise Failed("the session is not running")
        if self.real_apps is None:
            raise Failed("no session checkout")
        try:
            return self.session.control(payload, timeout=timeout)
        except self.real_apps.Failed as error:
            return {"err": {"code": "failed", "message": str(error)}}

    def action_rows(self, query: str = "") -> list[dict]:
        """The session pages its listing; the audit wants all of it.

        The listing is asked for as it stands rather than waited on, because a
        story asks many times in a row and a settle that long is minutes a
        story. What a step changed is seen by asking again, and a page that
        moved on under the walk starts the walk again."""
        if self.session is None:
            raise Failed("the session is not running")
        for _ in range(10):
            rows: list[dict] = []
            cursor = None
            try:
                while True:
                    payload = {"op": "actions_enumerate", "query": query,
                               "include_programmatic": True, "limit": 1000, "wait_ms": 200}
                    if cursor:
                        payload["cursor"] = cursor
                    answer = self.ok(payload, timeout=60)
                    rows += answer.get("actions") or []
                    cursor = answer.get("next_cursor")
                    if not cursor:
                        return [row for row in rows if isinstance(row, dict)]
            except Failed as error:
                if "stale_cursor" not in str(error):
                    raise
        raise Failed(f"the actions for {query!r} kept moving on")

    def client_key(self, key: str) -> dict:
        """A key the desktop's keymap lets through reaches the focused app."""
        return self.key(key)

    def client_type(self, text: str) -> None:
        self.type_text(text)

    # -- apps --

    def launch_app(self, app: str) -> dict:
        """Open an app through the desktop's own open-app door."""
        if self.session is None:
            raise Failed("the session is not running")
        ident = app if "." in app else f"so.tilo.{'file-explorer' if app == 'files' else app}"
        row = self.find_action("desktop:chrome\x1fopen-app")
        if row is None:
            raise DoorMissing("the session lists no open-app action")
        self.invoke(row, {"app": ident})
        try:
            window = self.wait_window(app)
        except Failed as error:
            raise Failed(str(error))
        self.app_windows[app] = window
        return window

    @property
    def wayland_runtime(self) -> Path | None:
        if self.session is None:
            return None
        return Path(self.session.runtime_directory())

    def bus_address(self) -> str | None:
        if self.session is None:
            return None
        runtime = Path(self.session.runtime_directory())
        for entry in sorted(runtime.glob("session-bus.*/bus")):
            return f"unix:path={entry}"
        return None

    def notify(self, summary: str, body: str = "", app: str = "Notes") -> str:
        address = self.bus_address()
        if address is None:
            raise Failed("the session runs no private bus to post a notification on")
        env = dict(os.environ)
        env.update(LOCALE)
        env["DBUS_SESSION_BUS_ADDRESS"] = address
        done = subprocess.run(["notify-send", "--app-name", app, "--icon",
                               "dialog-information", summary, body],
                              env=env, capture_output=True, text=True, timeout=30)
        return done.stdout or done.stderr

    def dismiss_onboarding(self, timeout: float = 20.0) -> bool:
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            names = set(self.target_names())
            steps = [name for name in names if name.startswith("onboarding.")]
            if not steps:
                return True
            try:
                self.click_named(sorted(steps)[0])
            except Failed:
                return False
            time.sleep(0.3)
        return False

    def apply_theme(self) -> None:
        if not self.theme:
            return
        row = self.find_action("choose-theme")
        if row is None:
            self.details["theme"] = "no choose-theme action"
            return
        try:
            self.invoke(row, {"theme": self.theme.capitalize()})
            self.details["theme"] = self.theme
        except Failed as error:
            self.details["theme"] = str(error)[:200]
        time.sleep(0.8)


def open_side(name: str, ref: str, root: Path, **kwargs) -> Side:
    return {"old": OldSide, "new": NewSide}[name](ref, root, **kwargs)

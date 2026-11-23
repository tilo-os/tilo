"""A computer on which the `tilo` command is the whole of Tilo.

A headless computer of a person's: no desktop runs here, so the command's own
daemon (`tilo-runtime --hold`) holds the device's session with the workspace,
and every step is a command a person runs there — `tilo login`, `tilo device
add`, `tilo files put`, `tilo ls` and `tilo read`. Nothing here reaches into
the computer's store or talks to a daemon except through those commands.

Each computer keeps its own home, XDG tree, identity root and runtime
directory under the temporary directory, as a person's computer keeps them
under their own login; what the story reads the computer by (what each
command said, and the daemon's log) is kept beside the story.
"""

from __future__ import annotations

import json
import os
import shutil
import re
import signal
import subprocess
import sys
import tempfile
import time
from contextlib import suppress
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from story import Failed, slug, until  # noqa: E402

# The `tilo` command of this check (t.toml's TILO_CLI_BIN), and the daemon it
# starts beside itself.
CLI = os.environ.get("TILO_CLI_BIN", "")
# How long one command may take before the step that ran it fails.
COMMAND_SECONDS = 240
# A ticket is what admits a computer to the person's devices, and it is on
# several command lines: what is kept beside the story and said in a failure
# carries none of it whole.
TICKET = re.compile(r"tilo1[0-9a-z]{16,}")
# Environment that names another Tilo on this machine (a desktop's API, a test
# control socket, an agent's MCP link): a computer of its own has none of them.
NOT_THIS_COMPUTERS = (
    "TILO_API_SOCKET",
    "TILO_TEST_CONTROL",
    "TILO_MCP_SERVER",
    "TILO_API_KEY",
    "TILO_API_URL",
)


def redact(text: str) -> str:
    """`text` with every ticket in it shortened to what one is, not what it admits."""
    return TICKET.sub("tilo1…", text)


def redact_args(args: tuple[str, ...]) -> list[str]:
    """The arguments of a kept command, its tickets shortened."""
    return [redact(argument) for argument in args]


def cli() -> Path:
    """The `tilo` command this story runs."""
    if not CLI or not Path(CLI).is_file():
        raise Failed(f"no tilo command to run: TILO_CLI_BIN names {CLI!r}")
    return Path(CLI)


class Computer:
    """One headless computer of a person's: its own state, and the command."""

    def __init__(self, story, name: str, label: str):
        self.story = story
        self.name = name
        self.key = slug(name)
        self.label = label
        # A device's store is the files layer's, which a shared output mount
        # cannot always hold: it lives in the temporary directory, as a
        # session's does.
        self.root = Path(tempfile.mkdtemp(prefix=f"cli-{self.key}-"))
        self.logs = story.out / "people" / self.key
        self.logs.mkdir(parents=True, exist_ok=True)
        self.said: list[dict] = []
        for part in ("home", "config", "data", "state", "cache", "run", "identity"):
            (self.root / part).mkdir(parents=True, mode=0o700)

    # -- the computer's own environment --

    def env(self) -> dict:
        """The environment this computer's commands run in: its own home,
        identity, state and runtime directory, and none of another Tilo's."""
        env = dict(os.environ)
        for name in NOT_THIS_COMPUTERS:
            env.pop(name, None)
        env.update(
            HOME=str(self.root / "home"),
            XDG_CONFIG_HOME=str(self.root / "config"),
            XDG_DATA_HOME=str(self.root / "data"),
            XDG_STATE_HOME=str(self.root / "state"),
            XDG_CACHE_HOME=str(self.root / "cache"),
            XDG_RUNTIME_DIR=str(self.root / "run"),
            TILO_IDENTITY_ROOT=str(self.root / "identity"),
            # A check's machine runs no secret service: the identity's secrets
            # are kept in its own files, as a machine without one keeps them.
            TILO_IDENTITY_FILE_SECRETS="1",
        )
        return env

    # -- what a person runs --

    def run(self, *args: str, deadline: float = COMMAND_SECONDS) -> subprocess.CompletedProcess:
        """One `tilo` command of this computer, its answer kept beside the story."""
        began = time.monotonic()
        try:
            done = subprocess.run([str(cli()), *args], env=self.env(), capture_output=True,
                                  text=True, timeout=deadline)
        except subprocess.TimeoutExpired as ended:
            self.said.append({"args": redact_args(args), "timedOut": deadline,
                              "stdout": (ended.stdout or b"").decode(errors="replace")[:4000]
                              if isinstance(ended.stdout, bytes) else (ended.stdout or "")[:4000]})
            raise Failed(f"`tilo {redact(' '.join(args))}` did not end in {round(deadline)} s on "
                         f"{self.name}") from None
        self.said.append({"args": redact_args(args), "code": done.returncode,
                          "ms": round((time.monotonic() - began) * 1000),
                          "stdout": redact(done.stdout[-4000:]),
                          "stderr": redact(done.stderr[-4000:])})
        return done

    def ok(self, *args: str, deadline: float = COMMAND_SECONDS) -> str:
        """One command that must succeed: its standard output."""
        done = self.run(*args, deadline=deadline)
        if done.returncode != 0:
            raise Failed(f"`tilo {redact(' '.join(args))}` failed on {self.name} "
                         f"({done.returncode}): "
                         f"{redact((done.stderr.strip() or done.stdout.strip())[-1500:])}")
        return done.stdout

    def json(self, *args: str, deadline: float = COMMAND_SECONDS):
        """One command whose standard output is JSON."""
        said = self.ok(*args, deadline=deadline)
        try:
            return json.loads(said)
        except ValueError:
            raise Failed(f"`tilo {redact(' '.join(args))}` on {self.name} answered no JSON: "
                         f"{redact(said.strip()[:1500])}") from None

    # -- what the computer is --

    def device_key(self) -> str:
        """This computer's one device key, in hex."""
        return self.ok("device", "public-key").strip()

    def stop(self) -> None:
        """End this computer's daemon, and keep what it said beside the story."""
        markers = [f"XDG_STATE_HOME={self.root / 'state'}", f"HOME={self.root / 'home'}"]
        pids = daemons(markers)
        if pids:
            for pid in pids:
                signal_pid(pid, signal.SIGTERM)
            with suppress(Failed):
                until("this computer's daemon to end", lambda: not daemons(markers), 30, 0.2)
            for pid in daemons(markers):
                signal_pid(pid, signal.SIGKILL)
        for path in (self.root / "state").rglob("*.log"):
            with open(self.logs / path.name, "a", errors="replace") as kept:
                with path.open(errors="replace") as live:
                    shutil.copyfileobj(live, kept)
        (self.logs / "commands.json").write_text(json.dumps(self.said, indent=1))
        shutil.rmtree(self.root, ignore_errors=True)


def signal_pid(pid: int, number: int) -> None:
    """Send `number` to `pid`, doing nothing when it has ended already."""
    try:
        os.kill(pid, number)
    except ProcessLookupError:
        pass


def daemons(markers: list[str]) -> list[int]:
    """The pids of the daemons (`tilo-runtime --hold`) started with any of
    `markers` in their environment: the computers whose commands started one."""
    found = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            command = (entry / "cmdline").read_bytes()
        except OSError:
            continue
        if b"tilo-runtime" not in command or b"--hold" not in command:
            continue
        try:
            environment = (entry / "environ").read_bytes()
        except OSError:
            continue
        if any(marker.encode() in environment for marker in markers):
            found.append(int(entry.name))
    return found


def workspace_of(computer: Computer, deadline: float = 45.0) -> dict:
    """The workspace a computer that entered one holds, as `tilo workspace ls`
    lists it: its id and its name. A computer enters just after logging in, so
    the list is asked until it carries one, as a person typing the command a
    moment later sees it."""
    last: object = None

    def listed():
        nonlocal last
        try:
            answer = computer.json("workspace", "ls", "--json")
        except Failed as refusal:
            last = str(refusal)
            return None
        last = answer
        workspaces = answer.get("workspaces") if isinstance(answer, dict) else None
        if not workspaces:
            return None
        if len(workspaces) > 1:
            raise Failed(f"{computer.name} lists several workspaces, and the story has one: "
                         f"{json.dumps(answer)[:800]}")
        return workspaces[0]

    try:
        return until(f"{computer.name} to list the workspace it entered", listed, deadline, 1.0)
    except Failed:
        raise Failed(f"{computer.name} lists no workspace {round(deadline)} s after it entered "
                     f"one: {json.dumps(last)[:800]}") from None


def holds(root: Path, words: str, largest: int = 64 * 1024 * 1024,
          skip: tuple[str, ...] = ()) -> list[str]:
    """Every file under `root` whose bytes hold `words` as they stand, by its
    path below `root`: what a computer's own state keeps. A path ending as one
    of `skip` says names is left out."""
    found = []
    for path in sorted(root.rglob("*")):
        try:
            if path.is_symlink() or not path.is_file() or path.stat().st_size > largest:
                continue
            if skip and str(path).endswith(skip):
                continue
            if words.encode() in path.read_bytes():
                found.append(str(path.relative_to(root)))
        except OSError:
            continue
    return found

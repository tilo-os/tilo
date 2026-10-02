#!/usr/bin/env python3
"""An isolated session for testing what an agent experiences in a Tilo app.

The agent's only door is the ordinary one: `tilo-runtime-mcp` over the app's
published actions. This script only sets up and tears down what is around it:

  start APP   a headless Tilo compositor as the only display (nothing appears
              on your desktop), a private home and runtime profile, and
              `tilo-app dev APP`, which rebuilds and restarts the app on every
              source change. The runtime daemon owns the profile, so an MCP
              connection survives each restart.
  mcp APP     the MCP endpoint for that session on stdio: what an agent
              (`.cursor/mcp.json`, Claude Code, a model harness) runs.
  status APP  whether the session is up, and its paths.
  report APP  what the agent made and how, from the app's own views (its
              `history`, `inspect` and `look` actions, where it publishes
              them), written to target/agent-runs/APP-TIME.
  stop APP    stop the session and remove its runtime directory.

Everything lives under target/agent-loop/APP (state, logs) and a private
directory in $XDG_RUNTIME_DIR (sockets, the runtime profile). Your real home,
settings, identity and workspaces are never read or written.

Usage:
  scripts/agent-loop/session.py start apps/canvas
  scripts/agent-loop/session.py mcp canvas
  scripts/agent-loop/session.py stop canvas
"""
import argparse
import json
import os
import pathlib
import shutil
import signal
import subprocess
import sys
import time

REPO = pathlib.Path(__file__).resolve().parents[2]
TARGET = REPO / "target"
SHELL = TARGET / "debug" / "shell"
RUNTIME_HOST = TARGET / "debug" / "tilo-runtime-mcp"
TILO_APP = TARGET / "debug" / "tilo-app"


def runtime_root() -> pathlib.Path:
    base = os.environ.get("XDG_RUNTIME_DIR")
    if not base or not os.path.isabs(base):
        sys.exit("XDG_RUNTIME_DIR must be set to an absolute private directory")
    return pathlib.Path(base) / "tilo-agent-loop"


def app_name(app: str) -> str:
    return pathlib.Path(app).name or app


class Session:
    def __init__(self, name: str):
        self.name = name
        self.root = TARGET / "agent-loop" / name
        # Short: Unix socket paths inside the profile are limited to ~100 bytes.
        self.run = runtime_root() / name
        self.profile = self.run / "p"
        self.state_file = self.root / "session.json"

    def env(self) -> dict:
        """The environment every process of the session runs in."""
        home = self.root / "home"
        env = {
            key: value
            for key, value in os.environ.items()
            if key not in ("DISPLAY", "WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP", "TILO_TEST_CONTROL")
        }
        env.update(
            {
                "HOME": str(home),
                "XDG_CONFIG_HOME": str(self.root / "config"),
                "XDG_DATA_HOME": str(self.root / "data"),
                "XDG_STATE_HOME": str(self.root / "state"),
                "XDG_CACHE_HOME": str(self.root / "cache"),
                "XDG_RUNTIME_DIR": str(self.run),
                "TILO_IDENTITY_ROOT": str(self.root / "identity"),
                "TILO_AUTHORITY_SOCKET_DIR": str(self.run / "system"),
                "TILO_FS_AUTHORITY_CACHE_DIR": str(self.root / "system" / "fs-cache"),
                "TILO_FS_CONTENT_PEER_CACHE_DIR": str(self.root / "system" / "peer-cache"),
                "TILO_RUNTIME_PROFILE": str(self.profile),
                "TILO_RUNTIME_BINARY": str(RUNTIME_HOST),
                "TILO_TEST_CONTROL": str(self.run / "control.sock"),
                # The dev host keeps the user's real build cache.
                "CARGO_HOME": os.environ.get("CARGO_HOME", str(pathlib.Path.home() / ".cargo")),
                "RUSTUP_HOME": os.environ.get("RUSTUP_HOME", str(pathlib.Path.home() / ".rustup")),
            }
        )
        return env

    def load(self) -> dict:
        try:
            return json.loads(self.state_file.read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            return {}

    def alive(self, pid) -> bool:
        if not pid:
            return False
        try:
            os.kill(pid, 0)
            return True
        except OSError:
            return False


def prepare(session: Session) -> None:
    for sub in ("home", "config", "data", "state", "cache", "identity", "system/fs-cache", "system/peer-cache", "logs"):
        (session.root / sub).mkdir(parents=True, exist_ok=True, mode=0o700)
    for path in (session.run, session.run / "system"):
        path.mkdir(parents=True, exist_ok=True, mode=0o700)
        os.chmod(path, 0o700)


def wait_for(path: pathlib.Path, process: subprocess.Popen, seconds: float, what: str) -> None:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if path.exists():
            return
        if process.poll() is not None:
            sys.exit(f"FAIL: {what} exited ({process.returncode}) before {path.name} appeared")
        time.sleep(0.25)
    sys.exit(f"FAIL: {path} did not appear within {seconds:.0f}s")


def start(args) -> int:
    app_dir = pathlib.Path(args.app).resolve()
    if not (app_dir / "Cargo.toml").exists():
        sys.exit(f"FAIL: {app_dir} is not an app directory")
    session = Session(app_name(args.app))
    if session.alive(session.load().get("compositor")):
        sys.exit(f"FAIL: a {session.name} session is already running; stop it first")
    for binary in (SHELL, RUNTIME_HOST, TILO_APP):
        if not binary.exists():
            sys.exit(
                f"FAIL: {binary} is missing; build it with\n"
                "  cargo build -p shell --bin shell --bin tilo-runtime-mcp && cargo build -p tilo-app-cli"
            )
    prepare(session)
    env = session.env()
    # Rebuilds run as you, with your toolchain, cache and builder; only the
    # app runs in the session's private home.
    cargo = shutil.which("cargo")
    if cargo is None:
        sys.exit("FAIL: no cargo on PATH")
    # `cargo run` hands its own environment to the program, so a runner puts
    # the session's home and runtime directory back for the app itself.
    runner = session.root / "run-app"
    runner.write_text(
        "#!/bin/sh\n"
        f"exec env HOME={json.dumps(env['HOME'])} "
        f"XDG_RUNTIME_DIR={json.dumps(env['XDG_RUNTIME_DIR'])} \"$@\"\n"
    )
    runner.chmod(0o700)
    wrapper = session.root / "cargo"
    wrapper.write_text(
        "#!/bin/sh\n"
        f"exec env HOME={json.dumps(str(pathlib.Path.home()))} "
        f"XDG_RUNTIME_DIR={json.dumps(os.environ['XDG_RUNTIME_DIR'])} "
        f"CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER={json.dumps(str(runner))} "
        f"CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER={json.dumps(str(runner))} "
        f"{json.dumps(cargo)} \"$@\"\n"
    )
    wrapper.chmod(0o700)
    env["CARGO"] = str(wrapper)
    logs = session.root / "logs"
    compositor = subprocess.Popen(
        [str(SHELL), "--backend", "headless"],
        env=env,
        stdout=open(logs / "compositor.log", "wb"),
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    display_file = session.run / "tilo-session" / "wayland-display"
    wait_for(display_file, compositor, args.timeout, "the compositor")
    env["WAYLAND_DISPLAY"] = display_file.read_text().strip()
    dev = subprocess.Popen(
        [str(TILO_APP), "dev", str(app_dir), "--app-only"],
        env=env,
        stdout=open(logs / "app.log", "wb"),
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    state = {
        "app": str(app_dir),
        "compositor": compositor.pid,
        "dev": dev.pid,
        "wayland_display": env["WAYLAND_DISPLAY"],
        "profile": str(session.profile),
        "runtime_dir": str(session.run),
        "logs": str(logs),
        "started": time.strftime("%Y-%m-%dT%H:%M:%S"),
    }
    session.state_file.write_text(json.dumps(state, indent=2))
    mcp = {
        "mcpServers": {
            f"tilo-{session.name}": {
                "command": "python3",
                "args": [str(pathlib.Path(__file__).resolve()), "mcp", session.name],
            }
        }
    }
    (session.root / "mcp.json").write_text(json.dumps(mcp, indent=2))
    print(f"READY {session.name}: display {env['WAYLAND_DISPLAY']}, profile {session.profile}")
    print(f"  app log      {logs / 'app.log'}")
    print(f"  MCP endpoint {pathlib.Path(__file__).resolve()} mcp {session.name}")
    if args.foreground:
        try:
            dev.wait()
        except KeyboardInterrupt:
            pass
        finally:
            stop_session(session)
    return 0


def profile_owners(session: Session) -> list:
    """Processes serving the session's runtime profile, found by the profile
    path in their environment, never by a pattern that could match us."""
    marker = f"TILO_RUNTIME_PROFILE={session.profile}".encode()
    owners = []
    for entry in pathlib.Path("/proc").iterdir():
        if not entry.name.isdigit() or int(entry.name) == os.getpid():
            continue
        try:
            environ = (entry / "environ").read_bytes()
            exe = os.readlink(entry / "exe")
        except OSError:
            continue
        if marker in environ.split(b"\0") and pathlib.Path(exe).name.startswith("tilo-runtime"):
            owners.append(int(entry.name))
    return owners


def stop_session(session: Session) -> None:
    state = session.load()
    for key in ("dev", "compositor"):
        pid = state.get(key)
        if session.alive(pid):
            try:
                os.killpg(pid, signal.SIGTERM)
            except OSError:
                pass
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline and any(session.alive(state.get(key)) for key in ("dev", "compositor")):
        time.sleep(0.2)
    for key in ("dev", "compositor"):
        pid = state.get(key)
        if session.alive(pid):
            try:
                os.killpg(pid, signal.SIGKILL)
            except OSError:
                pass
    # The runtime daemon started by the app outlives it by design; it owns
    # the profile, so stop it with the session.
    for pid in profile_owners(session):
        try:
            os.kill(pid, signal.SIGTERM)
        except OSError:
            pass
    shutil.rmtree(session.run, ignore_errors=True)
    if session.state_file.exists():
        session.state_file.unlink()


def stop(args) -> int:
    stop_session(Session(app_name(args.app)))
    print("stopped")
    return 0


def status(args) -> int:
    session = Session(app_name(args.app))
    state = session.load()
    up = session.alive(state.get("compositor")) and session.alive(state.get("dev"))
    print(json.dumps({"running": up, **state}, indent=2))
    return 0 if up else 1


def mcp(args) -> int:
    session = Session(app_name(args.app))
    if not session.state_file.exists():
        sys.exit(f"no {session.name} session; start one: {sys.argv[0]} start apps/{session.name}")
    env = session.env()
    os.execve(
        str(RUNTIME_HOST),
        [str(RUNTIME_HOST), "--profile", str(session.profile), "--connect"],
        env,
    )
    return 1


class McpClient:
    """The session's MCP endpoint as an agent uses it: newline-delimited
    JSON-RPC over the stdio of `session.py mcp`."""

    def __init__(self, session: Session):
        self.process = subprocess.Popen(
            [sys.executable, str(pathlib.Path(__file__).resolve()), "mcp", session.name],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=open(session.root / "logs" / "report-mcp.log", "w"),
            text=True,
            bufsize=1,
        )
        self.next_id = 0
        self.request("initialize", {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "agent-loop-report", "version": "1"},
        })
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def send(self, message: dict) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def request(self, method: str, params: dict) -> dict:
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params})
        assert self.process.stdout is not None
        while True:
            line = self.process.stdout.readline()
            if not line:
                sys.exit("FAIL: the MCP endpoint closed")
            message = json.loads(line)
            if message.get("id") == self.next_id:
                return message.get("result", message)

    def tool(self, name: str, arguments: dict) -> tuple:
        """The tool's JSON answer and the images it carried."""
        result = self.request("tools/call", {"name": name, "arguments": arguments})
        blocks = result.get("content", [])
        text = blocks[0].get("text", "") if blocks else ""
        try:
            value = json.loads(text)
        except json.JSONDecodeError:
            value = {"error": text}
        images = [block for block in blocks if block.get("type") == "image"]
        return value, images

    def close(self) -> None:
        self.process.terminate()


def report(args) -> int:
    """What the agent made and how, from the app's own views: its history,
    an inspection and a final look, written to target/agent-runs/."""
    import base64

    session = Session(app_name(args.app))
    if not session.state_file.exists():
        sys.exit(f"no {session.name} session is running")
    client = McpClient(session)
    apps, _ = client.tool("tilo_list_apps", {})
    peers = [app["peer_id"] for app in apps.get("apps", [])]
    if not peers:
        sys.exit("FAIL: the app is not running in the session")
    out = TARGET / "agent-runs" / f"{session.name}-{time.strftime('%Y%m%d-%H%M%S')}"
    out.mkdir(parents=True, exist_ok=True)
    page, _ = client.tool("tilo_query", {"peer_id": peers[-1]})
    written = []
    for resource in page.get("resources", []):
        detail, _ = client.tool("tilo_query", {"peer_id": peers[-1], "parent_resource": resource["resource"]})
        actions = {}
        for entry in detail.get("resources", []):
            if entry.get("resource") == resource["resource"]:
                actions = entry.get("actions", {})
        for view in ("history", "inspect", "look"):
            if view not in actions:
                continue
            value, images = client.tool("tilo_invoke", {
                "peer_id": peers[-1],
                "resource": resource["resource"],
                "action": view,
                "arguments": {"size": 1024} if view == "look" else {},
            })
            stem = f"{resource['kind']}-{view}"
            output = value.get("update", {}).get("output", {}).get("value", value)
            (out / f"{stem}.json").write_text(json.dumps(output, indent=2))
            written.append(f"{stem}.json")
            for index, image in enumerate(images):
                name = f"{stem}{'' if index == 0 else f'-{index}'}.png"
                (out / name).write_bytes(base64.b64decode(image["data"]))
                written.append(name)
    client.close()
    print(f"{out}")
    for name in written:
        print(f"  {name}")
    return 0 if written else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    begin = commands.add_parser("start", help="start an isolated session for an app")
    begin.add_argument("app", help="the app directory, e.g. apps/canvas")
    begin.add_argument("--timeout", type=float, default=90.0, help="seconds to wait for the compositor")
    begin.add_argument("--foreground", action="store_true", help="stay attached; Ctrl+C stops the session")
    begin.set_defaults(run=start)
    for name, run, text in (
        ("stop", stop, "stop the session"),
        ("status", status, "report the session"),
        ("mcp", mcp, "the MCP endpoint on stdio"),
        ("report", report, "record history, inspection and a final look"),
    ):
        command = commands.add_parser(name, help=text)
        command.add_argument("app", help="the app name or directory")
        command.set_defaults(run=run)
    args = parser.parse_args()
    return args.run(args)


if __name__ == "__main__":
    sys.exit(main())

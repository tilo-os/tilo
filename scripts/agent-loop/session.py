#!/usr/bin/env python3
"""An isolated session for testing what an agent experiences in a Tilo app.

The agent's only door is the ordinary one: `tilo-runtime-mcp` over the app's
published actions, and the app's window as a person uses it: the `computer`
tool with `app_id` clicks, drags, types and scrolls in the window, in the
window's pixels, and answers with what the window then shows. This script
only sets up and tears down what is around it:

  start APP   a headless Tilo compositor as the only display (nothing appears
              on your desktop), a private home and runtime profile, and
              `tilo-app dev APP`, which rebuilds and restarts the app on every
              source change. The runtime daemon owns the profile, so an MCP
              connection survives each restart. `--release` builds the app
              optimised (`tilo-app dev --release`), for work the debug build
              is too slow to judge; `--profile NAME` picks any Cargo profile.
  mcp APP     the MCP endpoint for that session on stdio: what an agent
              (`.cursor/mcp.json`, Claude Code, a model harness) runs.
  status APP  whether the session is up, its paths and its files folder.
  usage APP   what each MCP connection of the session cost: calls, bytes
              and estimated tokens of what the agent read, time (see
              usage.py for how tokens are estimated).
  report APP  what the agent made and how, from the app's own views (its
              `history`, `inspect` and `look` actions, where it publishes
              them), and what it cost, written to target/agent-runs/APP-TIME.
  stop APP    stop the session and remove its runtime directory.

The app's own file choosers (Open, Add picture, Export...) show
target/agent-loop/APP/files: put files there for the app to open, and find
there what it saves. `start` has the session's workspace grant the app read
and write on that folder, as the workspace's owner grants an app a folder in
its Permissions, so the chooser the shell draws offers it and nothing else.

Everything lives under target/agent-loop/APP (state, logs) and a private
directory in $XDG_RUNTIME_DIR (sockets, the runtime profile). Your real home,
settings, identity and workspaces are never read or written.

Usage:
  scripts/agent-loop/session.py start apps/canvas
  scripts/agent-loop/session.py start apps/canvas --release
  scripts/agent-loop/session.py mcp canvas
  scripts/agent-loop/session.py stop canvas
"""
import argparse
import json
import os
import pathlib
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import tomllib

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import usage as cost  # noqa: E402

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
        # The folder the app's file choosers offer.
        self.files = self.root / "files"

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
                "TILO_FS_CONTENT_PEER_CACHE_DIR": str(self.root / "system" / "peer-cache"),
                "TILO_RUNTIME_PROFILE": str(self.profile),
                "TILO_RUNTIME_BINARY": str(RUNTIME_HOST),
                "TILO_TEST_CONTROL": str(self.run / "control.sock"),
                # The agent's hands: the compositor's agent-control socket
                # (input and screenshots only) and the runtime's `computer`
                # tool driving it, as on a hosted machine an agent uses.
                "TILO_AGENT_CONTROL": str(self.run / "agent.sock"),
                "TILO_RUNTIME_COMPUTER_CONTROL": str(self.run / "agent.sock"),
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
    session.files.mkdir(parents=True, exist_ok=True)
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
                "  cargo build -p shell --bin shell --bin tilo-runtime-mcp --features agent-control && cargo build -p tilo-app-cli"
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
    # The workspace in front, which the app joins, is up once its room is.
    wait_for(display_file.with_name("workspace-id"), compositor, args.timeout, "the compositor")
    grant_files(session, app_dir)
    # The app sees the desktop a Tilo login gives its apps, so it leaves
    # History and its title to the shell's chrome as it does there, rather
    # than drawing a standalone window's bar. The compositor is not told:
    # a login's compositor exports its session to the user manager.
    env["XDG_CURRENT_DESKTOP"] = "tilo:GNOME"
    dev_command = [str(TILO_APP), "dev", str(app_dir), "--app-only"]
    if args.release:
        dev_command.append("--release")
    elif args.profile:
        dev_command += ["--profile", args.profile]
    dev = subprocess.Popen(
        dev_command,
        env=env,
        stdout=open(logs / "app.log", "wb"),
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    state = {
        "app": str(app_dir),
        "compositor": compositor.pid,
        "dev": dev.pid,
        "dev_command": dev_command[1:],
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
    print(f"  files        {session.files} (what the app's file choosers show)")
    if args.foreground:
        try:
            dev.wait()
        except KeyboardInterrupt:
            pass
        finally:
            stop_session(session)
    return 0


def grant_files(session: Session, app_dir: pathlib.Path) -> None:
    """Have the workspace in front grant the app read and write on the
    session's files folder: the shell records it as its owner granting it,
    as the Permissions of a workspace do, and the app's choosers then offer
    that folder."""
    with open(app_dir / "tilo-app.toml", "rb") as manifest:
        app_id = tomllib.load(manifest)["package"]["id"]
    request = {
        "op": "grant",
        "subject": f"app:{app_id}",
        "scope": f"fs:{session.files.resolve()}",
        "verb": "write",
        "id": "agent-loop-files",
    }
    try:
        with socket.socket(socket.AF_UNIX) as control:
            control.settimeout(30)
            control.connect(str(session.run / "control.sock"))
            control.sendall(json.dumps(request).encode() + b"\n")
            answer = control.makefile().readline()
    except OSError as error:
        answer = str(error)
    try:
        granted = "granted" in json.loads(answer).get("answer", {})
    except (ValueError, AttributeError):
        granted = False
    if not granted:
        # The session still runs; only the app's choosers have nothing to offer.
        print(f"WARN: the workspace did not grant {app_id} its files folder: {answer}")


def profile_owners(session: Session) -> list:
    """Processes serving the session's runtime profile, found by the profile
    path in their environment, never by a pattern that could match us."""
    marker = f"TILO_RUNTIME_PROFILE={session.profile}".encode()
    owners = []
    for entry in pathlib.Path("/proc").iterdir():
        if not entry.name.isdigit() or int(entry.name) == os.getpid():
            continue
        # The daemon makes itself non-dumpable, so only its command line is
        # readable; its environment and executable link are not.
        try:
            argv = (entry / "cmdline").read_bytes().split(b"\0")
        except OSError:
            continue
        try:
            environ = (entry / "environ").read_bytes().split(b"\0")
        except OSError:
            environ = []
        named = [b"--profile", str(session.profile).encode()]
        serves = marker in environ or any(argv[i : i + 2] == named for i in range(len(argv)))
        program = pathlib.Path(argv[0].decode(errors="replace")).name if argv else ""
        if serves and program.startswith("tilo-runtime"):
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
    print(json.dumps({"running": up, **state, "files": str(session.files)}, indent=2))
    return 0 if up else 1


def mcp(args) -> int:
    session = Session(app_name(args.app))
    if not session.state_file.exists():
        sys.exit(f"no {session.name} session; start one: {sys.argv[0]} start apps/{session.name}")
    env = session.env()
    command = [str(RUNTIME_HOST), "--profile", str(session.profile), "--connect"]
    if args.no_usage:
        os.execve(str(RUNTIME_HOST), command, env)
    return relay(command, env, usage_dir(session) / f"{time.strftime('%Y%m%d-%H%M%S')}-{os.getpid()}.jsonl")


def usage_dir(session: Session) -> pathlib.Path:
    return session.root / "logs" / "usage"


def relay(command: list, env: dict, log_path: pathlib.Path) -> int:
    """The endpoint on stdio, unchanged, with one line per answered request
    appended to `log_path`: what the agent read and how long it waited."""
    log_path.parent.mkdir(parents=True, exist_ok=True)
    child = subprocess.Popen(command, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    asked = {}
    lock = threading.Lock()

    def forward_requests() -> None:
        assert child.stdin is not None
        for line in sys.stdin.buffer:
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                message = None
            if isinstance(message, dict) and "id" in message and "method" in message:
                params = message.get("params") or {}
                with lock:
                    asked[json.dumps(message["id"])] = (message["method"], params, time.time())
            try:
                child.stdin.write(line)
                child.stdin.flush()
            except BrokenPipeError:
                break
        try:
            child.stdin.close()
        except BrokenPipeError:
            pass

    threading.Thread(target=forward_requests, daemon=True).start()
    assert child.stdout is not None
    with open(log_path, "a") as log:
        for line in child.stdout:
            log_answer(log, line, asked, lock)
            sys.stdout.buffer.write(line)
            sys.stdout.buffer.flush()
    return child.wait()


def log_answer(log, line: bytes, asked: dict, lock: threading.Lock) -> None:
    """Log one answer before the client reads it: a client that stops
    right after its last answer stops the relay too."""
    try:
        message = json.loads(line)
    except json.JSONDecodeError:
        return
    with lock:
        request = asked.pop(json.dumps(message.get("id")), None) if isinstance(message, dict) else None
    if request is None:
        return
    method, params, started = request
    record = {"method": method, "start": round(started, 3), "ms": round((time.time() - started) * 1000, 1)}
    if method == "tools/call":
        tool = params.get("name", "")
        record["tool"] = tool
        record["what"], record["actions"] = cost.what(tool, params.get("arguments") or {})
    record.update(cost.measure(message.get("result", message.get("error", {}))))
    record["wire_bytes"] = len(line)
    log.write(json.dumps(record) + "\n")
    log.flush()


def usage_logs(session: Session) -> list:
    return sorted(usage_dir(session).glob("*.jsonl"))


def usage(args) -> int:
    """Each MCP connection's cost, newest last; --last 1 is the latest."""
    session = Session(app_name(args.app))
    logs = usage_logs(session)[-args.last:]
    if not logs:
        sys.exit(f"no MCP connection of the {session.name} session is logged yet")
    for path in logs:
        records = cost.read_log(path)
        summary = cost.summarise(records)
        if args.json:
            print(json.dumps({"connection": path.stem, **summary}))
            continue
        print(f"{path.stem}: {cost.format_summary(summary)}")
        if args.calls:
            for record in records:
                images = " ".join(f"{i['width']}x{i['height']} {i['mime']} {i['bytes']:,}B" for i in record.get("images", []))
                print(f"    {record.get('what', record['method']):<40} {record['text_bytes']:>8,}B text  ~{record['tokens']:>6,} tok  {images}")
    return 0


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
    connections = [
        {"connection": path.stem, **cost.summarise(cost.read_log(path))} for path in usage_logs(session)
    ]
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
        actions = {}
        query = {"peer_id": peers[-1], "parent_resource": resource["resource"]}
        # A resource's actions can span pages; read on to the last one.
        while True:
            detail, _ = client.tool("tilo_query", query)
            for entry in detail.get("resources", []):
                if entry.get("resource") == resource["resource"]:
                    actions.update(entry.get("actions", {}))
            if detail.get("exhausted", True) or not detail.get("next_cursor"):
                break
            query = {**query, "cursor": detail["next_cursor"]}
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
    if connections:
        (out / "usage.json").write_text(json.dumps(connections, indent=2))
        written.append("usage.json")
    print(f"{out}")
    for connection in connections:
        print(f"  {connection['connection']}: {cost.format_summary(connection)}")
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
    build = begin.add_mutually_exclusive_group()
    build.add_argument(
        "--release",
        action="store_true",
        help="build the app optimised (tilo-app dev --release: the thin-app profile)",
    )
    build.add_argument("--profile", metavar="NAME", help="build the app in this Cargo profile [default: dev]")
    begin.set_defaults(run=start)
    for name, run, text in (
        ("stop", stop, "stop the session"),
        ("status", status, "report the session"),
        ("report", report, "record history, inspection, a final look and the session's cost"),
    ):
        command = commands.add_parser(name, help=text)
        command.add_argument("app", help="the app name or directory")
        command.set_defaults(run=run)
    endpoint = commands.add_parser("mcp", help="the MCP endpoint on stdio")
    endpoint.add_argument("app", help="the app name or directory")
    endpoint.add_argument("--no-usage", action="store_true", help="do not log what each call cost")
    endpoint.set_defaults(run=mcp)
    spent = commands.add_parser("usage", help="what each MCP connection cost")
    spent.add_argument("app", help="the app name or directory")
    spent.add_argument("--last", type=int, default=1, help="how many connections, newest last")
    spent.add_argument("--calls", action="store_true", help="list every call")
    spent.add_argument("--json", action="store_true", help="one JSON summary per connection")
    spent.set_defaults(run=usage)
    args = parser.parse_args()
    return args.run(args)


if __name__ == "__main__":
    sys.exit(main())

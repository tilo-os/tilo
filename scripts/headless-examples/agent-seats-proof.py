#!/usr/bin/env python3
"""Two agents drive one hosted machine's pixels at once without colliding.

A hosted machine is one person's computer that several of their agents share
over MCP. Every MCP session is one agent run, and the compositor gives every
agent its own seat and its own strip: screenshots and coordinates are that
strip's, a window the agent opens lands in it, and the keys it holds never
reach another agent's window.

What this checks, against a real headless compositor built with
`--features agent-control`, a real terminal app in each agent's strip and two
concurrent MCP sessions speaking the canonical `computer` tools:

  1. Each agent works in its own strip: its window lands there, and its
     screenshot is that strip alone, not the other agent's.
  2. While alpha holds Shift (one `hold_key` action, four seconds), beta types
     `exit` into its own terminal. Beta's shell exits, so the text arrived in
     lowercase and in beta's terminal; alpha's shell keeps running.
  3. Alpha's Shift is still alpha's: `test LOUD = LOUD && exit`, with LOUD
     typed as Shift chords, ends alpha's shell.
  4. Ending an MCP session releases that agent's seat; its strip stays while
     it holds a window.

The frames (each agent's strip, and the whole screen) are written to the
output directory.

Usage:
  cargo build -p shell --bin shell --features agent-control
  cargo build -p terminal --bin terminal
  cargo build -p tilo-runtime --bin tilo-runtime
  ./agent-seats-proof.py [OUTDIR]
"""
import base64
import json
import os
import pathlib
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

REPO = pathlib.Path(__file__).resolve().parents[2]
# The nightly ignored-test lane points these at the binaries Cargo built for the run
# (the shell with `--features agent-control`).
SHELL = pathlib.Path(os.environ.get("TILO_SHELL_BINARY", REPO / "target" / "debug" / "shell"))
TERMINAL = pathlib.Path(os.environ.get("TILO_TERMINAL_BINARY", REPO / "target" / "debug" / "terminal"))
PROTOCOL = "2025-06-18"
# What a shell types to title its terminal (OSC 2) with the quoted text after it.
TITLE = "printf '\\033]2;%s\\007' "

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Mcp:
    """One MCP session over the compositor's streamable HTTP endpoint."""

    def __init__(self, url: str, name: str, token: str):
        self.url, self.name, self.sid, self.n = url, name, None, 0
        self.token = token
        self.call("initialize", {
            "protocolVersion": PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": name, "version": "1"},
        })

    def call(self, method: str, params: dict) -> dict:
        self.n += 1
        body = json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method,
                           "params": params}).encode()
        request = urllib.request.Request(self.url, body, {
            "content-type": "application/json",
            "accept": "application/json, text/event-stream",
            "authorization": f"Bearer {self.token}",
        })
        if self.sid:
            request.add_header("mcp-session-id", self.sid)
        with urllib.request.urlopen(request, timeout=120) as response:
            self.sid = self.sid or response.headers.get("mcp-session-id")
            return json.loads(response.read())

    def computer(self, actions: list[dict]) -> tuple[list[dict], bytes | None]:
        result = self.call("tools/call", {
            "name": "computer_batch", "arguments": {"actions": actions},
        })["result"]
        texts, image = [], None
        for block in result["content"]:
            if block["type"] == "text":
                texts.append(json.loads(block["text"].split("] ", 1)[1]))
            else:
                image = base64.b64decode(block["data"])
        if result.get("isError"):
            raise RuntimeError(f"{self.name}: {result['content']}")
        return texts, image

    def end(self) -> None:
        request = urllib.request.Request(
            self.url, method="DELETE", headers={
                "mcp-session-id": self.sid, "authorization": f"Bearer {self.token}"})
        urllib.request.urlopen(request, timeout=10)


def control(sock: str, command: dict) -> dict:
    with socket.socket(socket.AF_UNIX) as s:
        s.connect(sock)
        s.sendall((json.dumps(command) + "\n").encode())
        data = b""
        while not data.endswith(b"\n"):
            chunk = s.recv(1 << 20)
            if not chunk:
                break
            data += chunk
    return json.loads(data)


def strips(test_sock: str, titles: bool = False) -> tuple[list[tuple], int | None]:
    """Every strip as `(id, name, tab count)`, or `(id, name, tab titles)`."""
    tree = control(test_sock, {"op": "state_dump"})["state"]["workspace_tree"]
    out = []
    for workspace in tree["workspaces"]:
        for strip in workspace["strips"]:
            tabs = [tab.get("title") for group in strip["window_groups"] for tab in group["tabs"]]
            out.append((strip["id"], strip["name"], tabs if titles else len(tabs)))
    active = next(iter(tree.get("active_strip", {}).values()), None)
    return out, active


def descendants(pid: int) -> list[str]:
    """Names of every live process below `pid`."""
    children: dict[int, list[int]] = {}
    names: dict[int, str] = {}
    for entry in pathlib.Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            stat = (entry / "stat").read_text()
        except OSError:
            continue
        name = stat[stat.index("(") + 1:stat.rindex(")")]
        fields = stat[stat.rindex(")") + 2:].split()
        if fields[0] == "Z":
            continue
        names[int(entry.name)] = name
        children.setdefault(int(fields[1]), []).append(int(entry.name))
    out, todo = [], list(children.get(pid, []))
    while todo:
        child = todo.pop()
        out.append(names.get(child, "?"))
        todo.extend(children.get(child, []))
    return out


def shell_running(terminal: subprocess.Popen) -> bool:
    """Whether the terminal's guest shell is alive; the app outlives it."""
    return "sh" in descendants(terminal.pid)


def wait_for(condition, timeout: float = 15.0) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if condition():
            return True
        time.sleep(0.3)
    return condition()


def png_size(data: bytes) -> tuple[int, int]:
    return int.from_bytes(data[16:20], "big"), int.from_bytes(data[20:24], "big")


def main() -> int:
    for binary in (SHELL, TERMINAL):
        if not binary.exists():
            print(f"missing {binary}; see the usage in this file", file=sys.stderr)
            return 2
    out = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "target/agent-seats-proof")
    out.mkdir(parents=True, exist_ok=True)
    root = tempfile.mkdtemp(prefix="agent-seats-")
    run = os.path.join(root, "run")
    os.makedirs(run, mode=0o700)
    agent_sock = os.path.join(run, "agent.sock")
    test_sock = os.path.join(run, "test.sock")
    port = free_port()
    env = {
        **os.environ,
        "HOME": os.path.join(root, "home"),
        "XDG_RUNTIME_DIR": run,
        "XDG_DATA_HOME": os.path.join(root, "data"),
        "XDG_CACHE_HOME": os.path.join(root, "cache"),
        "XDG_STATE_HOME": os.path.join(root, "state"),
        "XDG_CONFIG_HOME": os.path.join(root, "config"),
        "TILO_STATE_PATH": os.path.join(root, "config", "state.json"),
        "TILO_PEER_KEY_PATH": os.path.join(root, "keys", "peer-key"),
        "TILO_HEADLESS_SIZE": "1280x800",
        "LC_ALL": "en_US.UTF-8", "LANG": "en_US.UTF-8", "LANGUAGE": "en_US",
    }
    env.pop("DISPLAY", None)
    env.pop("WAYLAND_DISPLAY", None)
    # Loopback is reachable by other accounts too; protect this test endpoint.
    token = os.urandom(32).hex()
    token_file = pathlib.Path(root) / "mcp-token"
    token_file.touch(mode=0o600)
    token_file.write_text(token)
    compositor_env = {
        **env,
        "TILO_AGENT_CONTROL": agent_sock,
        "TILO_TEST_CONTROL": test_sock,
        "TILO_RUNTIME_MCP_HTTP": f"127.0.0.1:{port}",
        "TILO_RUNTIME_MCP_HTTP_TOKEN_FILE": str(token_file),
    }
    log = open(out / "shell.log", "w")
    shell = subprocess.Popen([str(SHELL), "--backend", "headless"], env=compositor_env,
                             stdout=log, stderr=subprocess.STDOUT)
    terminals: list[subprocess.Popen] = []
    try:
        url = f"http://127.0.0.1:{port}/mcp"
        deadline = time.time() + 90
        while not (os.path.exists(agent_sock) and os.path.exists(test_sock)):
            if time.time() > deadline or shell.poll() is not None:
                print("the compositor did not come up; see shell.log", file=sys.stderr)
                return 1
            time.sleep(0.3)
        time.sleep(2)
        alpha, beta = Mcp(url, "agent-alpha", token), Mcp(url, "agent-beta", token)

        print("1. each agent opens a terminal, which lands in its own strip")
        for agent, name in ((alpha, "alpha"), (beta, "beta")):
            before, _ = strips(test_sock)
            # Any pixel action seats the agent: its strip takes focus, so the
            # window that maps next lands in it.
            agent.computer([{"action": "screenshot"}])
            terminal_env = {**env, "WAYLAND_DISPLAY": "wayland-1"}
            terminal_env["SHELL"] = "/bin/sh"
            terminals.append(subprocess.Popen(
                [str(TERMINAL)], env=terminal_env,
                stdout=open(out / f"terminal-{name}.log", "w"), stderr=subprocess.STDOUT))
            deadline = time.time() + 60
            while time.time() < deadline:
                now, active = strips(test_sock)
                if sum(t for _, _, t in now) > sum(t for _, _, t in before):
                    break
                time.sleep(0.5)
            time.sleep(3)
        layout, active = strips(test_sock)
        print("     strips:", layout)
        check("two strips, one terminal each",
              len([s for s in layout if s[2] == 1]) == 2, str(layout))

        texts_a, frame_a = alpha.computer([{"action": "screenshot", "format": "png"}])
        texts_b, frame_b = beta.computer([{"action": "screenshot", "format": "png"}])
        full = out / "full-before.png"
        control(test_sock, {"op": "snapshot", "path": str(full.resolve())})
        (out / "alpha-before.png").write_bytes(frame_a)
        (out / "beta-before.png").write_bytes(frame_b)
        size_a, size_b = png_size(frame_a), png_size(frame_b)
        check("each screenshot is its own strip, not the screen",
              size_a[1] < 800 and size_b[1] < 800 and size_a[0] == 1280,
              f"alpha {size_a}, beta {size_b}")
        check("the two agents see different pixels", frame_a != frame_b)

        print("2. alpha holds Shift while beta types `exit` into its own terminal")
        # `exit` only ends beta's shell if it arrives in beta's terminal in
        # lowercase: with alpha's Shift on it, it is `EXIT`, no command at all.
        errors: list[str] = []

        def hold() -> None:
            try:
                alpha.computer([{"action": "hold_key", "text": "shift", "duration": 4,
                                 "no_screenshot": True}])
            except Exception as error:  # noqa: BLE001 - reported below
                errors.append(f"alpha: {error}")

        def type_lower() -> None:
            try:
                time.sleep(1)
                beta.computer([
                    {"action": "type", "text": "exit", "no_screenshot": True},
                    {"action": "key", "text": "Return", "no_screenshot": True},
                ])
            except Exception as error:  # noqa: BLE001 - reported below
                errors.append(f"beta: {error}")

        workers = [threading.Thread(target=hold), threading.Thread(target=type_lower)]
        started = time.time()
        for worker in workers:
            worker.start()
        for worker in workers:
            worker.join()
        check("both agents' actions succeeded concurrently", not errors, "; ".join(errors))
        check("the hold overlapped beta's typing", time.time() - started >= 4)
        _, frame_a = alpha.computer([{"action": "wait", "duration": 2},
                                     {"action": "screenshot", "format": "png"}])
        (out / "alpha-during.png").write_bytes(frame_a)
        control(test_sock, {"op": "snapshot",
                            "path": str((out / "full-during.png").resolve())})
        alpha_terminal, beta_terminal = terminals
        check("beta's lowercase `exit` ended beta's shell",
              wait_for(lambda: not shell_running(beta_terminal)))
        check("alpha's shell is still running: beta's keys never reached it",
              shell_running(alpha_terminal))

        print("3. alpha's own Shift still works for alpha")
        # Only a real uppercase LOUD from alpha's Shift chords passes the test.
        alpha.computer([
            {"action": "type", "text": "test ", "no_screenshot": True},
            {"action": "key", "text": "shift+l", "no_screenshot": True},
            {"action": "key", "text": "shift+o", "no_screenshot": True},
            {"action": "key", "text": "shift+u", "no_screenshot": True},
            {"action": "key", "text": "shift+d", "no_screenshot": True},
            {"action": "type", "text": " = LOUD && exit", "no_screenshot": True},
            {"action": "key", "text": "Return", "no_screenshot": True},
        ])
        check("alpha's Shift chords came out uppercase in alpha's shell",
              wait_for(lambda: not shell_running(alpha_terminal)))

        print("4. ending a session releases its agent; its strip keeps its window")
        beta.end()
        time.sleep(1)
        layout_after, _ = strips(test_sock)
        check("beta's strip and window remain after beta left", layout_after == layout,
              str(layout_after))
        alpha.end()
    finally:
        for process in terminals:
            process.terminate()
        shell.send_signal(signal.SIGTERM)
        try:
            shell.wait(timeout=10)
        except subprocess.TimeoutExpired:
            shell.kill()
        for process in terminals:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
        # The terminals' runtime hosts daemonise; they are ours by profile.
        for entry in pathlib.Path("/proc").iterdir():
            try:
                if entry.name.isdigit() and root.encode() in (entry / "cmdline").read_bytes():
                    os.kill(int(entry.name), signal.SIGTERM)
            except OSError:
                pass
    print(f"frames in {out}")
    if failures:
        print(f"FAILED: {', '.join(failures)}")
        return 1
    print("all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())

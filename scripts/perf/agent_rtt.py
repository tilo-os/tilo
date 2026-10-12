#!/usr/bin/env python3
"""Agent round trip: what an agent waits for on the control socket and MCP.

An agent drives Tilo through typed actions: it lists them, reads the state
and invokes one by address. This times those calls on the path an agent
uses, from the client's side of the socket: the request is written, the
compositor's event loop parses it, resolves the address through the same
capability gate as the palette, runs the action and writes the answer, and
the clock stops when the whole answer line has arrived. Parsing the answer
in Python is not counted.

`actions_invoke` answers once the action is admitted, which for an app
action means accepted, not done. `wait_ms` holds the answer until the
invocation settles, so a "complete" sample here is one round trip whose
receipt is terminal; it is checked on every sample, never assumed. A shell
action runs before its answer is written, so for it accept and complete
coincide; an app action (`--app`) shows the difference.

Workloads, each `--warmup` calls then `--samples` timed calls on one
persistent connection:

  agent.list.local               actions_enumerate, query ""
  agent.state_dump.local         state_dump
  agent.invoke.local.noop.accept actions_invoke of the no-op, no wait
  agent.invoke.local.noop        the same with wait_ms: a terminal receipt
  agent.invoke.local.throughput  microseconds per completed no-op while
                                 `--clients` connections invoke at once,
                                 one sample per second of `--duration`
  agent.invoke.mcp.noop          the no-op as an MCP tools/call through
                                 scripts/tilo-mcp-server.py
  agent.invoke.local.app.accept  with --app PROVIDER:ID only
  agent.invoke.local.app         the same, complete (wait_ms)

The no-op is `actions/navigation.axis.disable`: a shell action that needs
no consent, clears a flag that is already clear and draws nothing new.

Either point it at a running desktop (`--socket`; on a real session that
is `$XDG_RUNTIME_DIR/tilo-session/admin-control.sock`, which serves these
ops to the device admin) or let it start a headless compositor through
`scripts/headless-examples/spawn-headless.sh` (`--spawn SHELL`). A spawned
compositor is a release build only with `--features dev-overrides`, which
serves the test-control socket; that socket runs the same dispatch and
capability gate as the admin socket, without the admin socket's per-connection
peer check. The compositor, its profile and its socket are removed on exit,
whatever happens.

Records go to `--out` through tools/perf/record.py (`--env-json` from
`record.py env`), or to stdout as JSON lines.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Callable, Iterator

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools" / "perf"))
import record  # noqa: E402

SPAWN = REPO / "scripts" / "headless-examples" / "spawn-headless.sh"
MCP_SERVER = REPO / "scripts" / "tilo-mcp-server.py"
NOOP = ("actions", "navigation.axis.disable")
TERMINAL = frozenset({"succeeded", "failed", "cancelled"})
# Generous: the answer leaves as soon as the invocation settles.
WAIT_MS = 5000
MCP_PROTOCOL = "2025-06-18"


class BenchError(RuntimeError):
    """A workload could not be measured; the run must fail, not skip it."""


class Connection:
    """One persistent connection speaking the control socket's JSON lines."""

    def __init__(self, path: str, timeout: float = 30.0):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        self.sock.connect(path)
        self.buffer = b""

    def close(self) -> None:
        self.sock.close()

    def round_trip(self, line: bytes) -> tuple[int, bytes]:
        """Send one request line; return (nanoseconds, answer line)."""
        start = time.perf_counter_ns()
        self.sock.sendall(line)
        while b"\n" not in self.buffer:
            chunk = self.sock.recv(1 << 20)
            if not chunk:
                raise BenchError("the compositor closed the connection mid-answer")
            self.buffer += chunk
        elapsed = time.perf_counter_ns() - start
        answer, _, self.buffer = self.buffer.partition(b"\n")
        return elapsed, answer


def request(payload: dict[str, Any]) -> bytes:
    return json.dumps(payload, separators=(",", ":")).encode() + b"\n"


def answer_of(line: bytes, what: str) -> Any:
    """The `answer` of a reply, or a BenchError naming the compositor's refusal."""
    try:
        reply = json.loads(line)
    except json.JSONDecodeError as error:
        raise BenchError(f"{what}: the answer is not JSON: {error}") from error
    if "err" in reply:
        err = reply["err"]
        raise BenchError(f"{what}: refused with {err.get('code')}: {err.get('message')}")
    if "answer" in reply:
        return reply["answer"]
    if "ok" in reply:
        return reply["ok"]
    return reply


def receipt_state(answer: Any) -> str | None:
    """The receipt state an `actions_invoke` answer carries, if any."""
    if not isinstance(answer, dict):
        return None
    receipt = answer.get("receipt")
    if not isinstance(receipt, dict):
        return None
    update = receipt.get("update")
    return update.get("state") if isinstance(update, dict) else None


def invoke_payload(address: tuple[str, str], wait: bool) -> dict[str, Any]:
    payload: dict[str, Any] = {"op": "actions_invoke", "provider": address[0], "id": address[1]}
    if wait:
        payload["wait_ms"] = WAIT_MS
    return payload


def check_complete(answer: Any, what: str) -> None:
    state = receipt_state(answer)
    if state not in TERMINAL:
        raise BenchError(f"{what}: wait_ms answered before the invocation settled (state {state!r})")
    if state != "succeeded":
        raise BenchError(f"{what}: the invocation ended {state}: {json.dumps(answer)[:400]}")


def check_admitted(answer: Any, what: str) -> None:
    state = receipt_state(answer)
    if state is None:
        raise BenchError(f"{what}: the answer carries no receipt: {json.dumps(answer)[:400]}")
    if state in TERMINAL and state != "succeeded":
        raise BenchError(f"{what}: the invocation ended {state}: {json.dumps(answer)[:400]}")


def measure(conn: Connection, line: bytes, warmup: int, samples: int,
            check: Callable[[bytes], None]) -> list[float]:
    """Milliseconds per round trip; every answer, warm-up included, is checked."""
    for _ in range(warmup):
        _, answer = conn.round_trip(line)
        check(answer)
    timings = []
    for _ in range(samples):
        elapsed, answer = conn.round_trip(line)
        check(answer)
        timings.append(elapsed / 1e6)
    return timings


def throughput(socket_path: str, address: tuple[str, str], clients: int,
               duration: float) -> tuple[list[float], dict[str, Any]]:
    """Microseconds per completed call in each whole second, all clients together.

    Every client invokes the no-op with wait_ms on its own connection,
    back to back, for `duration` seconds.
    """
    if clients < 1 or duration < 1:
        raise BenchError("throughput needs at least one client and one second")
    line = request(invoke_payload(address, wait=True))
    seconds = int(duration)
    per_second = [0] * seconds
    latencies: list[float] = []
    errors: list[str] = []
    lock = threading.Lock()
    start_gate = threading.Barrier(clients + 1)

    def client() -> None:
        try:
            conn = Connection(socket_path)
        except OSError as error:
            errors.append(f"connect: {error}")
            start_gate.abort()
            return
        own: list[tuple[float, float]] = []
        try:
            start_gate.wait()
            begin = start_ns[0]
            end = begin + seconds * 1_000_000_000
            while True:
                elapsed, answer = conn.round_trip(line)
                done = time.perf_counter_ns()
                if done > end:
                    break
                check_complete(answer_of(answer, "throughput"), "throughput")
                own.append(((done - begin) / 1e9, elapsed / 1e6))
        except (BenchError, OSError, threading.BrokenBarrierError) as error:
            errors.append(str(error))
        finally:
            conn.close()
        with lock:
            for at, latency in own:
                per_second[min(int(at), seconds - 1)] += 1
                latencies.append(latency)

    start_ns = [0]
    threads = [threading.Thread(target=client, daemon=True) for _ in range(clients)]
    for thread in threads:
        thread.start()
    try:
        start_ns[0] = time.perf_counter_ns()
        start_gate.wait()
    except threading.BrokenBarrierError:
        pass
    for thread in threads:
        thread.join(timeout=duration + 60)
    if errors:
        raise BenchError(f"throughput: {errors[0]}")
    if any(count == 0 for count in per_second):
        raise BenchError(f"throughput: a second completed no call: {per_second}")
    samples = [1e6 / count for count in per_second]
    ordered = sorted(latencies)
    meta = {
        "clients": clients, "duration_s": seconds, "calls": len(latencies),
        "calls_per_s": round(len(latencies) / seconds, 1),
        "loaded_p50_ms": round(record.nearest_rank(ordered, 50), 4),
        "loaded_p99_ms": round(record.nearest_rank(ordered, 99), 4),
    }
    return samples, meta


class McpClient:
    """The MCP stdio server, started over `socket_path`, spoken to as a model would."""

    def __init__(self, socket_path: str):
        self.process = subprocess.Popen(
            [sys.executable, str(MCP_SERVER), "--socket", socket_path],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            text=True, bufsize=1)
        self.next_id = 0

    def close(self) -> None:
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        for stream in (self.process.stdin, self.process.stdout):
            if stream is not None:
                stream.close()

    def send(self, method: str, params: dict[str, Any] | None = None) -> None:
        payload: dict[str, Any] = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            payload["params"] = params
        self._write(payload)

    def _write(self, payload: dict[str, Any]) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps(payload) + "\n")
        self.process.stdin.flush()

    def call(self, method: str, params: dict[str, Any]) -> tuple[int, dict[str, Any]]:
        """One request; (nanoseconds until its reply line, reply)."""
        assert self.process.stdout is not None
        self.next_id += 1
        want = self.next_id
        start = time.perf_counter_ns()
        self._write({"jsonrpc": "2.0", "id": want, "method": method, "params": params})
        while True:
            line = self.process.stdout.readline()
            if not line:
                raise BenchError("the MCP server exited")
            elapsed = time.perf_counter_ns() - start
            try:
                reply = json.loads(line)
            except json.JSONDecodeError:
                continue
            if reply.get("id") == want:
                return elapsed, reply


def mcp_noop(socket_path: str, address: tuple[str, str], warmup: int,
             samples: int) -> list[float]:
    client = McpClient(socket_path)
    try:
        _, reply = client.call("initialize", {
            "protocolVersion": MCP_PROTOCOL, "capabilities": {},
            "clientInfo": {"name": "agent-rtt", "version": "1"}})
        if "result" not in reply:
            raise BenchError(f"MCP initialize failed: {reply}")
        client.send("notifications/initialized")
        params = {"name": "tilo_invoke_action",
                  "arguments": {"provider": address[0], "id": address[1]}}
        timings = []
        for index in range(warmup + samples):
            elapsed, reply = client.call("tools/call", params)
            result = reply.get("result") or {}
            if "error" in reply or result.get("isError"):
                raise BenchError(f"MCP tools/call failed: {json.dumps(reply)[:400]}")
            if index >= warmup:
                timings.append(elapsed / 1e6)
        return timings
    finally:
        client.close()


@contextlib.contextmanager
def spawned(shell: Path, log: Path, timeout: int,
            extra_env: dict[str, str] | None = None) -> Iterator[str]:
    """A headless compositor through spawn-headless.sh, removed on exit.

    The script is the one way this repository starts a headless desktop: it
    isolates the profile and removes it, the socket and the compositor when
    it is told to stop. It runs in its own process group, which is killed
    as a whole if it does not stop by itself. `extra_env` reaches the
    compositor (spawn-headless.sh passes its environment on), for example
    `TILO_HEADLESS_PROFILE` and what a seeded profile restores from.
    """
    if not shell.is_file() or not os.access(shell, os.X_OK):
        raise BenchError(f"no compositor at {shell}")
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if not runtime or not os.path.isabs(runtime):
        raise BenchError("XDG_RUNTIME_DIR must name a private directory for the socket")
    directory = Path(tempfile.mkdtemp(prefix="agent-rtt-", dir=runtime))
    socket_path = str(directory / "control.sock")
    env = {**os.environ, **(extra_env or {}), "TILO_HEADLESS_BINARY": str(shell.resolve()),
           "TILO_HEADLESS_TIMEOUT": str(timeout)}
    log.write_bytes(b"")
    # Appending, as drain() does with the compositor's stdout.
    with log.open("ab") as stderr:
        process = subprocess.Popen(["bash", str(SPAWN), socket_path], env=env,
                                   stdout=subprocess.PIPE, stderr=stderr,
                                   start_new_session=True)
    try:
        assert process.stdout is not None
        # The compositor shares this stdout: whatever it prints before the
        # socket is ready (its log, with RUST_LOG) goes to the log.
        line = process.stdout.readline().decode(errors="replace").strip()
        while line and not line.startswith("READY "):
            with log.open("ab") as sink:
                sink.write(line.encode() + b"\n")
            line = process.stdout.readline().decode(errors="replace").strip()
        if line != f"READY {socket_path}":
            raise BenchError(f"the compositor did not start ({line or 'no READY line'}); see {log}")
        # The compositor inherits this pipe as its stdout: keep draining it
        # into the log so a chatty compositor never blocks on a full pipe.
        threading.Thread(target=drain, args=(process.stdout, log), daemon=True).start()
        yield socket_path
    finally:
        stop_group(process)
        with contextlib.suppress(OSError):
            os.rmdir(directory)


def drain(stream: Any, log: Path) -> None:
    with log.open("ab") as sink:
        for chunk in iter(lambda: stream.read1(65536), b""):
            sink.write(chunk)


def stop_group(process: subprocess.Popen) -> None:
    """Stop spawn-headless.sh (it stops the compositor), then its whole group."""
    if process.poll() is None:
        process.send_signal(signal.SIGTERM)
        try:
            process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            pass
    with contextlib.suppress(ProcessLookupError):
        os.killpg(process.pid, signal.SIGKILL)
    with contextlib.suppress(subprocess.TimeoutExpired):
        process.wait(timeout=10)
    deadline = time.monotonic() + 10
    while group_alive(process.pid):
        if time.monotonic() > deadline:
            raise BenchError(f"processes of the spawned compositor (group {process.pid}) outlived it")
        time.sleep(0.1)


def group_alive(pgid: int, proc: Path = Path("/proc")) -> bool:
    """Whether a process of group `pgid` still runs (a zombie does not)."""
    if not proc.is_dir():
        try:
            os.killpg(pgid, 0)
        except ProcessLookupError:
            return False
        return True
    for stat in proc.glob("[0-9]*/stat"):
        try:
            text = stat.read_text()
        except OSError:
            continue
        # pid (comm) state ppid pgrp ...; comm may hold spaces and parentheses.
        fields = text[text.rfind(")") + 2:].split()
        if len(fields) > 2 and fields[0] != "Z" and int(fields[2]) == pgid:
            return True
    return False


def parse_address(text: str) -> tuple[str, str]:
    provider, separator, ident = text.partition(":")
    if not separator or not provider or not ident:
        raise argparse.ArgumentTypeError(f"{text!r} must be PROVIDER:ID")
    return provider, ident


def run(args: argparse.Namespace, socket_path: str) -> list[tuple[str, str, list[float], dict[str, Any]]]:
    """Every selected workload: (KPI, unit, samples, meta)."""
    results: list[tuple[str, str, list[float], dict[str, Any]]] = []
    noop = args.noop
    common = {"transport": args.transport, "address": f"{noop[0]}/{noop[1]}",
              "warmup": args.warmup}
    conn = Connection(socket_path)
    try:
        last: dict[str, Any] = {}

        def listing(line: bytes) -> None:
            answer = answer_of(line, "actions_enumerate")
            if not isinstance(answer, dict) or not answer.get("actions"):
                raise BenchError("actions_enumerate listed no action")
            last.update(rows=len(answer["actions"]), total=answer.get("total"),
                        response_bytes=len(line))

        results.append(("agent.list.local", "ms", measure(
            conn, request({"op": "actions_enumerate", "query": ""}), args.warmup, args.samples,
            listing), {**common, "address": "", **last}))
        last.clear()

        def dump(line: bytes) -> None:
            answer_of(line, "state_dump")
            last["response_bytes"] = len(line)

        results.append(("agent.state_dump.local", "ms", measure(
            conn, request({"op": "state_dump"}), args.warmup, args.samples, dump),
            {**common, "address": "", **last}))

        accepted = request(invoke_payload(noop, wait=False))
        results.append(("agent.invoke.local.noop.accept", "ms", measure(
            conn, accepted, args.warmup, args.samples,
            lambda line: check_admitted(answer_of(line, "no-op"), "no-op")), common))
        complete = request(invoke_payload(noop, wait=True))
        results.append(("agent.invoke.local.noop", "ms", measure(
            conn, complete, args.warmup, args.samples,
            lambda line: check_complete(answer_of(line, "no-op"), "no-op")),
            {**common, "wait_ms": WAIT_MS}))

        if args.app:
            app = {**common, "address": f"{args.app[0]}/{args.app[1]}"}
            results.append(("agent.invoke.local.app.accept", "ms", measure(
                conn, request(invoke_payload(args.app, wait=False)), args.warmup, args.samples,
                lambda line: check_admitted(answer_of(line, "app"), "app")), app))
            results.append(("agent.invoke.local.app", "ms", measure(
                conn, request(invoke_payload(args.app, wait=True)), args.warmup, args.samples,
                lambda line: check_complete(answer_of(line, "app"), "app")),
                {**app, "wait_ms": WAIT_MS}))
    finally:
        conn.close()

    if args.clients:
        samples, meta = throughput(socket_path, noop, args.clients, args.duration)
        results.append(("agent.invoke.local.throughput", "us", samples, {**common, **meta}))
    if args.mcp:
        results.append(("agent.invoke.mcp.noop", "ms", mcp_noop(
            socket_path, noop, args.warmup, args.samples),
            {**common, "transport": f"MCP stdio ({MCP_SERVER.name}) over {args.transport}"}))
    return results


def emit(results: list[tuple[str, str, list[float], dict[str, Any]]],
         env: dict[str, Any] | None, out: Path | None) -> list[record.Record]:
    records = []
    for kpi, unit, samples, meta in results:
        if env is None:
            ordered = sorted(samples)
            print(json.dumps({"kpi": kpi, "unit": unit, "n": len(ordered),
                              "p50": record.nearest_rank(ordered, 50),
                              "p95": record.nearest_rank(ordered, 95),
                              "p99": record.nearest_rank(ordered, 99),
                              "max": ordered[-1], "meta": meta}))
            continue
        item = record.from_samples(kpi, unit, samples, **{**env, "meta": {**env.get("meta", {}), **meta}})
        records.append(item)
        if out is None:
            print(item.to_json())
        else:
            record.write(out, item)
    return records


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    target = parser.add_mutually_exclusive_group(required=True)
    target.add_argument("--socket", help="a running desktop's control socket")
    target.add_argument("--spawn", type=Path, metavar="SHELL",
                        help="start this compositor headless and measure it")
    parser.add_argument("--samples", type=int, default=1000)
    parser.add_argument("--warmup", type=int, default=100)
    parser.add_argument("--noop", type=parse_address, default=NOOP,
                        help="the no-op action, PROVIDER:ID (default %(default)s)")
    parser.add_argument("--app", type=parse_address,
                        help="also time this app action, PROVIDER:ID, accept and complete")
    parser.add_argument("--clients", type=int, default=8,
                        help="concurrent clients for the throughput run; 0 skips it")
    parser.add_argument("--duration", type=float, default=10.0,
                        help="seconds of the throughput run")
    parser.add_argument("--mcp", action=argparse.BooleanOptionalAction, default=True,
                        help="also time the no-op through the MCP server")
    parser.add_argument("--transport", default=None,
                        help="how the socket is reached, recorded in each record's meta")
    parser.add_argument("--env-json", type=Path,
                        help="record.py env output; without it, summaries are printed instead")
    parser.add_argument("--out", type=Path, help="append records here (default stdout)")
    parser.add_argument("--log", type=Path, help="the spawned compositor's stderr")
    parser.add_argument("--startup-timeout", type=int, default=120)
    args = parser.parse_args(argv)
    if args.samples < 1 or args.warmup < 0:
        parser.error("--samples must be positive and --warmup not negative")
    if args.transport is None:
        args.transport = ("test-control socket, headless compositor" if args.spawn
                          else "control socket")
    env = record.load_env(args.env_json) if args.env_json else None
    try:
        if args.spawn:
            log = args.log or Path(tempfile.gettempdir()) / "agent-rtt-compositor.log"
            with spawned(args.spawn, log, args.startup_timeout) as socket_path:
                results = run(args, socket_path)
        else:
            results = run(args, args.socket)
    except (BenchError, OSError) as error:
        print(f"agent_rtt: {error}", file=sys.stderr)
        return 1
    emit(results, env, args.out)
    return 0


if __name__ == "__main__":
    sys.exit(main())

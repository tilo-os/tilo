#!/usr/bin/env python3
"""Agent round trip: what an agent waits for on the control socket.

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
  agent.invoke.local.app.accept  with --app PROVIDER:ID only
  agent.invoke.local.app         the same, complete (wait_ms)

The no-op is `so.tilo.session/desktop:chrome␟reopen-closed` (`tilo_session::
desktop_surface::ChromeEffect::ReopenClosed`): a session service action that
needs no consent, and with nothing closed reopens nothing and changes nothing.

Either point it at a running desktop (`--socket`; on a real session that
is `$XDG_RUNTIME_DIR/tilo-session/admin-control.sock`, which serves these
ops to the device admin) or let it start a headless compositor through
`real_apps.ScriptedSession` (`--spawn SHELL`). A spawned compositor is a
release build only with `--features dev-overrides`, which
serves the test-control socket; that socket runs the same dispatch and
capability gate as the admin socket, without the admin socket's per-connection
peer check. It starts on a desk: a first pass mints the throwaway identity a
development login opens its desk with, so the no-op is the desk's own action
and not setup's. The compositor, its profile and its socket are removed on
exit, whatever happens.

Records go to `--out` through tools/perf/record.py (`--env-json` from
`record.py env`), or to stdout as JSON lines.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import shutil
import socket
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Callable, Iterator

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools" / "perf"))
sys.path.insert(0, str(REPO / "scripts" / "headless-examples"))
import record  # noqa: E402
import real_apps  # noqa: E402

NOOP = ("so.tilo.session", "desktop:chrome\u001freopen-closed")
TERMINAL = frozenset({"succeeded", "failed", "cancelled"})
# Generous: the answer leaves as soon as the invocation settles.
WAIT_MS = 5000


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


@contextlib.contextmanager
def desk_session(shell: Path, log: Path, timeout: int) -> Iterator[tuple[str, Path]]:
    """A headless session with a desk of its own: (control socket, runtime directory).

    A session that holds no identity opens setup instead of a desk, so a first
    pass mints the throwaway identity a development login opens its desk with
    and exits; the pass that is measured starts on it. Both run the binary this
    run built (`real_apps.ScriptedSession`), on one scratch profile, and the
    profile, the runtime directory and the session are removed on exit.
    """
    cache = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache")
    cache.mkdir(parents=True, exist_ok=True)
    profile = Path(tempfile.mkdtemp(prefix="desk-session.", dir=cache))
    identity_log = log.with_name(log.stem + ".identity.log")
    env = {"RUST_LOG": os.environ.get("RUST_LOG", "info")}
    try:
        # A first pass opens setup instead of a desk when the profile holds no
        # identity: it mints the throwaway one, which the measured pass then
        # finds, so the no-op is the desk's own action and not setup's.
        try:
            with real_apps.ScriptedSession(shell, profile=profile, timeout=timeout,
                                           env=env, log=identity_log) as session:
                connection = Connection(str(session.control_socket), timeout=60)
                try:
                    minted = answer_of(
                        connection.round_trip(request({"op": "provision_test_identity"}))[1],
                        "provision the test identity")
                finally:
                    connection.close()
        except real_apps.Failed as error:
            raise BenchError(f"{error} (see {identity_log})") from error
        if not isinstance(minted, dict) or minted.get("throwaway") is not True:
            raise BenchError(f"no throwaway identity for the measured session: "
                             f"{json.dumps(minted)[:400]} (see {identity_log})")
        with real_apps.ScriptedSession(shell, profile=profile, timeout=timeout,
                                       env=env, log=log) as session:
            yield str(session.control_socket), session.runtime_dir
    except real_apps.Failed as error:
        raise BenchError(f"{error} (see {log})") from error
    finally:
        shutil.rmtree(profile, ignore_errors=True)


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
            with desk_session(args.spawn, log, args.startup_timeout) as (socket_path, _runtime):
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

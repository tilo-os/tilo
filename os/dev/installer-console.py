#!/usr/bin/env python3
"""Drive a VM's installer or greeter through its console channel.

The installer and the greeter answer the tilo-control action protocol
(actions_enumerate / actions_invoke / state_dump; crates/session/console-control)
on the virtio-serial port os/dev/vm.sh attaches, whose host end is the Unix
socket `vm.sh console` prints. Windows WHPX uses a loopback `tcp://host:port`
socket instead. `tilo-control --socket <path> ...` speaks the
same protocol; this stdlib-only client exists for release lanes that do not
build the compositor's CLI, and adds what a long boot needs: waiting, a `seq`
field so an answer to an abandoned request is never mistaken for the current
one, and secrets taken from the environment so they never reach argv.

  installer-console.py --socket S wait APP [--timeout N] [--save FILE]
  installer-console.py --socket S wait-step STEP [--timeout N] [--save FILE]
  installer-console.py --socket S state [--save FILE]
  installer-console.py --socket S check APP [/json/pointer] [--timeout N]
  installer-console.py --socket S invoke ACTION@RESOURCE [ARGS_JSON]
                         [--secret NAME=ENVVAR ...] [--provider P]
  installer-console.py --socket S wait-install [--timeout N] [--save FILE]
  installer-console.py field FILE /json/pointer

`check` asks once whether APP is answering and, given a pointer, whether that
state field is `true` (the greeter's `/accepts_input`, for instance).

Exit status: 0 success, 1 refused/failed or a false `check` (reason on
stderr), 2 timeout or unreachable, 64 usage.
"""
from __future__ import annotations

import argparse
import itertools
import json
import math
import os
import socket
import sys
import time
from urllib.parse import urlsplit

_SEQ = itertools.count(1)
MAX_REPLY_BYTES = 1024 * 1024


class Unreachable(Exception):
    pass


class Refused(Exception):
    def __init__(self, code: str, message: str):
        super().__init__(f"{code}: {message}")
        self.code = code


def request(path: str, payload: dict, timeout: float = 15.0) -> dict:
    seq = f"cycle-{os.getpid()}-{next(_SEQ)}"
    payload = dict(payload, seq=seq)
    if not math.isfinite(timeout) or timeout <= 0:
        raise Unreachable("request timeout must be positive and finite")
    deadline = time.monotonic() + timeout
    sock = None
    try:
        if path.startswith("tcp://"):
            endpoint = urlsplit(path)
            if endpoint.hostname not in {"127.0.0.1", "localhost", "::1"} or endpoint.port is None:
                raise Unreachable("TCP console must use a loopback host and port")
            sock = socket.create_connection((endpoint.hostname, endpoint.port), timeout)
        else:
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            sock.settimeout(timeout)
            sock.connect(path)
        sock.settimeout(max(0.001, deadline - time.monotonic()))
        sock.sendall(json.dumps(payload).encode() + b"\n")
        buffer = b""
        while time.monotonic() < deadline:
            sock.settimeout(max(0.001, deadline - time.monotonic()))
            chunk = sock.recv(65536)
            if not chunk:
                break
            buffer += chunk
            while b"\n" in buffer:
                line, _, buffer = buffer.partition(b"\n")
                if len(line) > MAX_REPLY_BYTES:
                    raise Unreachable("console response is too large")
                try:
                    reply = json.loads(line)
                except ValueError:
                    continue
                if isinstance(reply, dict) and reply.get("seq") == seq:
                    return reply
            if len(buffer) > MAX_REPLY_BYTES:
                raise Unreachable("console response is too large")
    except (OSError, ValueError) as error:
        raise Unreachable(str(error)) from error
    finally:
        if sock is not None:
            sock.close()
    raise Unreachable("no answer to this request")


def unwrap(reply: dict, key: str) -> object:
    if key in reply:
        return reply[key]
    err = reply.get("err") or {}
    if not isinstance(err, dict):
        raise Unreachable("invalid console error response")
    raise Refused(err.get("code", "unknown"), err.get("message", ""))


def state(path: str, timeout: float = 15.0) -> dict:
    current = unwrap(request(path, {"op": "state_dump"}, timeout), "state")
    if not isinstance(current, dict):
        raise Unreachable("invalid console state response")
    return current


def save(value: object, target: str | None) -> None:
    if target:
        with open(target, "w", encoding="utf-8") as handle:
            json.dump(value, handle, indent=2, sort_keys=True)
            handle.write("\n")


def poll(path: str, accept, timeout: float, interval: float = 5.0) -> dict:
    if not math.isfinite(timeout) or timeout <= 0:
        raise TimeoutError("poll timeout must be positive and finite")
    deadline = time.monotonic() + timeout
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError
        try:
            current = state(path, min(15.0, remaining))
            if accept(current):
                return current
        except (Unreachable, Refused):
            pass
        if time.monotonic() >= deadline:
            raise TimeoutError
        time.sleep(min(interval, max(0, deadline - time.monotonic())))


def pointer(document: object, path: str) -> object:
    value = document
    for part in [p for p in path.split("/") if p]:
        part = part.replace("~1", "/").replace("~0", "~")
        if isinstance(value, list):
            value = value[int(part)] if part.isdigit() and int(part) < len(value) else None
        elif isinstance(value, dict):
            value = value.get(part)
        else:
            return None
    return value


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--socket", help="console socket (vm.sh console)")
    sub = parser.add_subparsers(dest="command", required=True)
    wait = sub.add_parser("wait")
    wait.add_argument("app")
    wait.add_argument("--timeout", type=float, default=600)
    wait.add_argument("--save")
    step = sub.add_parser("wait-step")
    step.add_argument("step")
    step.add_argument("--timeout", type=float, default=120)
    step.add_argument("--save")
    show = sub.add_parser("state")
    show.add_argument("--save")
    check = sub.add_parser("check")
    check.add_argument("app")
    check.add_argument("pointer", nargs="?")
    check.add_argument("--timeout", type=float, default=15)
    invoke = sub.add_parser("invoke")
    invoke.add_argument("id")
    invoke.add_argument("args", nargs="?", default="{}")
    invoke.add_argument("--secret", action="append", default=[],
                        help="NAME=ENVVAR: argument NAME from environment variable ENVVAR")
    invoke.add_argument("--provider", default="installer")
    install = sub.add_parser("wait-install")
    install.add_argument("--timeout", type=float, default=3600)
    install.add_argument("--save")
    field = sub.add_parser("field")
    field.add_argument("file")
    field.add_argument("pointer")
    options = parser.parse_args(argv)

    if options.command == "field":
        with open(options.file, encoding="utf-8") as handle:
            value = pointer(json.load(handle), options.pointer)
        print("" if value is None else value if isinstance(value, str) else json.dumps(value))
        return 0
    if not options.socket:
        parser.error("--socket is required")
    path = options.socket
    try:
        if options.command == "state":
            current = state(path)
            save(current, options.save)
            print(json.dumps(current))
        elif options.command == "check":
            current = state(path, options.timeout)
            if current.get("app") != options.app or (
                options.pointer is not None and pointer(current, options.pointer) is not True
            ):
                print(json.dumps(current), file=sys.stderr)
                return 1
        elif options.command == "wait":
            save(poll(path, lambda s: s.get("app") == options.app, options.timeout), options.save)
        elif options.command == "wait-step":
            save(poll(path, lambda s: s.get("step") == options.step, options.timeout, 2), options.save)
        elif options.command == "invoke":
            arguments = json.loads(options.args)
            if not isinstance(arguments, dict):
                parser.error("ARGS_JSON must be an object")
            for binding in options.secret:
                name, _, variable = binding.partition("=")
                if not name or variable not in os.environ:
                    parser.error(f"--secret {name}=... names an unset variable")
                arguments[name] = os.environ[variable]
            answer = unwrap(request(path, {"op": "actions_invoke", "provider": options.provider,
                                           "id": options.id, "args": arguments}, 60), "answer")
            print(json.dumps({"id": options.id, "step": (answer or {}).get("step")}))
        elif options.command == "wait-install":
            started = time.monotonic()
            final = poll(path, lambda s: (s.get("install") or {}).get("state") in ("done", "failed"),
                         options.timeout)
            save(final, options.save)
            print(json.dumps({"seconds": round(time.monotonic() - started)}))
            if final["install"]["state"] != "done":
                for line in final["install"].get("log_tail", [])[-10:]:
                    print(line, file=sys.stderr)
                return 1
    except Refused as refusal:
        print(f"installer-console: {refusal}", file=sys.stderr)
        return 1
    except (Unreachable, TimeoutError) as error:
        print(f"installer-console: {error or 'timed out'}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

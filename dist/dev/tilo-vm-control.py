#!/usr/bin/env python3
"""Send one deterministic command to a Tilo developer control socket."""

from __future__ import annotations

import argparse
import json
import socket
import sys
import time


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("socket")
    parser.add_argument("request", help="one JSON object")
    parser.add_argument("--wait-target", metavar="NAME")
    args = parser.parse_args()
    request = json.loads(args.request)
    if not isinstance(request, dict):
        parser.error("request must be a JSON object")

    def transact(payload: dict[str, object]) -> object:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(5)
            client.connect(args.socket)
            client.sendall(json.dumps(payload, separators=(",", ":")).encode() + b"\n")
            response = bytearray()
            while not response.endswith(b"\n"):
                chunk = client.recv(65536)
                if not chunk:
                    break
                response.extend(chunk)
        if not response:
            raise RuntimeError("control socket closed without a response")
        return json.loads(response)

    try:
        decoded = transact(request)
        if args.wait_target and "err" not in decoded:
            deadline = time.monotonic() + 5
            while True:
                listed = transact({"op": "list_targets"})
                targets = listed.get("targets", []) if isinstance(listed, dict) else []
                if any(item.get("name") == args.wait_target for item in targets):
                    break
                if time.monotonic() >= deadline:
                    print(
                        f"target did not become visible: {args.wait_target}",
                        file=sys.stderr,
                    )
                    return 1
                time.sleep(0.05)
    except (RuntimeError, FileNotFoundError, ConnectionResetError) as error:
        print(error, file=sys.stderr)
        return 1
    print(json.dumps(decoded, sort_keys=True))
    return 1 if "err" in decoded else 0


if __name__ == "__main__":
    raise SystemExit(main())

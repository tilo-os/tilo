#!/usr/bin/env python3
"""Run one real feedback review through the model gateway, as the desktop session does.

The gateway picks the device-local model that declares trace review, so this
needs the model started by tilo-model-manager and its profile written by
Settings. It checks the gateway's validated answer, not a mocked one.

    dist/local-models/qwen3-4b-awq/smoke-review.py --socket "$XDG_RUNTIME_DIR/tilo/services/model-gateway.sock"
"""

import argparse
import json
import socket
import struct
import sys

TRACE = {
    "steps": [
        {"kind": "opened", "outcome": "done", "app": "Mail", "action": "Open",
         "resource_kind": "mail.draft", "texts": []},
        {"kind": "edited", "outcome": "done", "app": "Mail", "action": "Message",
         "resource_kind": "mail.draft", "texts": [
             {"node": "r:mail/draft/to", "sensitivity": "personal", "kind": "name",
              "origin": "semantic", "text": "Jana Novák"},
             {"node": "r:mail/draft/body", "sensitivity": "public", "origin": "semantic",
              "text": "Hi Petr, the invoice for Dr. Alvarez at 14 Rue Oberkampf is attached. Call me on 0171 2345678."},
         ]},
        {"kind": "invoked", "outcome": "failed", "app": "Mail", "action": "Send",
         "resource_kind": "mail.draft", "texts": []},
        {"kind": "appeared", "outcome": "done", "app": "Mail", "action": "Couldn't send",
         "resource_kind": "mail.draft", "texts": []},
    ]
}


def write_frame(stream, value):
    body = json.dumps(value, separators=(",", ":")).encode()
    stream.sendall(struct.pack("<I", len(body)) + body)


def read_frame(stream):
    def exact(size):
        data = bytearray()
        while len(data) < size:
            part = stream.recv(size - len(data))
            if not part:
                raise RuntimeError("gateway closed the connection")
            data.extend(part)
        return bytes(data)

    (length,) = struct.unpack("<I", exact(4))
    return json.loads(exact(length))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--socket", required=True)
    args = parser.parse_args()
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(180)
        stream.connect(args.socket)
        # The desktop session names itself; this socket is outside every sandbox.
        write_frame(stream, {"protocol": 2, "principal": "session"})
        write_frame(stream, {"version": 4, "verb": "review_trace", "trace": TRACE})
        response = read_frame(stream)
    if not response.get("ok"):
        raise RuntimeError(response.get("error", "gateway rejected the review"))
    answer = json.loads(response["output"])
    if answer["model"] is None:
        raise RuntimeError("no device-local model declares trace review; start one first")
    print(json.dumps(answer, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    try:
        main()
    except (KeyError, ValueError, OSError, RuntimeError) as error:
        print(f"review smoke failed: {error}", file=sys.stderr)
        sys.exit(1)

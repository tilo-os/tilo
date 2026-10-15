#!/usr/bin/env python3
"""Prove the native gateway consumes systemd's listener, including queued clients.

Run through `cargo test -p tilo-model-gateway --features server --test socket_activation`.
The Cargo integration target supplies the production gateway executable.
"""

import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import tempfile


def read_exact(stream, size):
    result = b""
    while len(result) < size:
        part = stream.recv(size - len(result))
        if not part:
            raise AssertionError("gateway closed before answering the queued request")
        result += part
    return result


def main():
    binary = Path(sys.argv[1]).resolve(strict=True)
    with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as directory:
        path = str(Path(directory) / "gateway.sock")
        with socket.socket(socket.AF_UNIX) as listener, socket.socket(socket.AF_UNIX) as client:
            listener.bind(path)
            listener.listen(16)
            inode = os.stat(path).st_ino
            client.settimeout(10)
            # A client queued before exec cannot be served by rebinding the path.
            client.connect(path)
            for value in [
                {"protocol": 1, "app_id": "so.tilo.agent"},
                {"version": 3, "verb": "cancellation_supported"},
            ]:
                body = json.dumps(value).encode()
                client.sendall(struct.pack("<I", len(body)) + body)
            child = subprocess.Popen(
                [
                    "bash", "-c",
                    'fd=$1; shift; exec 3<&"$fd"; '
                    'export LISTEN_PID=$$ LISTEN_FDS=1; exec "$@"',
                    "activation-test", str(listener.fileno()), str(binary),
                    "serve", "--socket", path,
                ],
                pass_fds=(listener.fileno(),),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            try:
                listener.close()
                size = struct.unpack("<I", read_exact(client, 4))[0]
                assert size <= 4096, "unbounded activation response"
                response = json.loads(read_exact(client, size))
                assert response == {"ok": True, "output": "cancel-v1"}, response
                assert os.stat(path).st_ino == inode, "gateway replaced the activation socket"
                print("ok    production gateway serves queued clients without rebinding")
            finally:
                child.terminate()
                try:
                    child.communicate(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.communicate()


if __name__ == "__main__":
    main()

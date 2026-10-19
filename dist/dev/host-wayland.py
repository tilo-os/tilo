#!/usr/bin/env python3
"""Run a command as a client of the host's Wayland display, connected here.

Usage: dist/dev/host-wayland.py SOCKET COMMAND [ARGS...]

A Tilo desktop admits a Wayland client it did not launch only after reading
the client's user and mount namespaces from /proc/<pid>/ns, to tell a host
process from a sandboxed app. The kernel hides those entries for a process
that made itself non-dumpable, and a nested Tilo shell does exactly that as
soon as it loads its device key, before its window backend connects: the
host then cannot tell it from a sandboxed app and drops the connection
("Connection reset by peer").

So this process, an ordinary dumpable host process, connects to SOCKET
itself and hands the connected socket to COMMAND as WAYLAND_SOCKET, the
standard way to give a Wayland client a ready connection. The host checks
this process, which stays alive as COMMAND's parent until it exits, and
forwards the signals that stop it.
"""

import os
import signal
import socket
import subprocess
import sys


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    path, command = sys.argv[1], sys.argv[2:]
    connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        connection.connect(path)
    except OSError as error:
        print(f"host-wayland: cannot connect to {path}: {error}", file=sys.stderr)
        return 1
    fd = connection.fileno()
    os.set_inheritable(fd, True)
    env = dict(os.environ)
    env.pop("WAYLAND_DISPLAY", None)
    env["WAYLAND_SOCKET"] = str(fd)
    child = subprocess.Popen(command, env=env, pass_fds=(fd,))
    connection.close()

    def forward(signum, _frame):
        child.send_signal(signum)

    for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(signum, forward)
    status = child.wait()
    return 128 - status if status < 0 else status


if __name__ == "__main__":
    sys.exit(main())

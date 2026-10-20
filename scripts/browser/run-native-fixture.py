#!/usr/bin/env python3
"""Reap only an explicit native fixture's descendants before its profile is removed.

Chromium flushes its profile after the fixture exits and closes its pipes. Linux
subreaper admission makes those processes our children, so the enclosing ordinary
test-environment wrapper cannot race their profile writes during cleanup.
"""
import ctypes
import os
import resource
import signal
import subprocess
import sys
import time


def main(arguments):
    if not arguments:
        raise ValueError("an explicit native fixture command is required")
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0):  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), "could not supervise fixture descendants")
    # Fixtures crash renderers on purpose; dumping a CEF process image can
    # outlast the fixture's bound before the crash is reported.
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    result = subprocess.run(arguments, check=False)
    deadline = time.monotonic() + 40
    while True:
        try:
            child, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return result.returncode
        if child:
            continue
        if time.monotonic() >= deadline:
            # These are exclusively children reparented from our fixture.
            # Killing a parent may adopt more children; keep cleanup bounded
            # and reap without blocking on any still-running descendant.
            cleanup_deadline = time.monotonic() + 2
            while time.monotonic() < cleanup_deadline:
                with open(f"/proc/self/task/{os.getpid()}/children", encoding="ascii") as record:
                    children = record.read().split()
                for owned in children:
                    try:
                        os.kill(int(owned), signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                try:
                    reaped, _ = os.waitpid(-1, os.WNOHANG)
                except ChildProcessError:
                    break
                if not reaped:
                    time.sleep(0.02)
            raise TimeoutError("native fixture descendants did not shut down")
        time.sleep(0.02)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

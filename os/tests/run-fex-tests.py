#!/usr/bin/env python3
"""Run upstream FEX tests with an explicitly managed, ready server.

Use inside a dedicated test container with source and rootfs mounted read-only.
The upstream auto-started daemon inherits output pipes, which can keep the
first test's subprocess.communicate() waiting after the guest has exited.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import select
import subprocess
import sys


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build-dir", required=True, type=Path)
    parser.add_argument("--rootfs", required=True, type=Path)
    parser.add_argument("ctest_args", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    build = args.build_dir.resolve(strict=True)
    rootfs = args.rootfs.resolve(strict=True)
    server_exe = build / "Bin/FEXServer"
    if not rootfs.is_dir() or not server_exe.is_file():
        parser.error("a rootfs directory and built Bin/FEXServer are required")

    environment = os.environ.copy()
    environment["FEX_ROOTFS"] = str(rootfs)
    log_path = build / "Testing/Temporary/FEXServer.log"
    log_path.parent.mkdir(parents=True, exist_ok=True)
    extra = args.ctest_args
    if extra[:1] == ["--"]:
        extra = extra[1:]

    read_fd, write_fd = os.pipe()
    try:
        with log_path.open("w") as log:
            server = subprocess.Popen(
                [str(server_exe), "--foreground", "--persistent", "300", "--wait_pipe", str(write_fd)],
                env=environment,
                pass_fds=(write_fd,),
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=subprocess.STDOUT,
            )
            os.close(write_fd)
            write_fd = -1
            try:
                if not select.select([read_fd], [], [], 20)[0]:
                    raise RuntimeError(f"FEXServer readiness timed out; see {log_path}")
                if os.read(read_fd, 8) or server.poll() is not None:
                    raise RuntimeError(f"FEXServer failed during startup; see {log_path}")
                os.close(read_fd)
                read_fd = -1
                return subprocess.call(
                    ["ctest", "--test-dir", str(build), "--output-on-failure",
                     "--parallel", "2", "--timeout", "302", *extra],
                    env=environment,
                )
            finally:
                server.terminate()
                try:
                    server.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()
    finally:
        for descriptor in (read_fd, write_fd):
            if descriptor != -1:
                os.close(descriptor)


if __name__ == "__main__":
    sys.exit(main())

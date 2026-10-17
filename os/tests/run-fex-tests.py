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
import signal
import subprocess
import sys


def run_ctest(command, environment, server, log_path):
    """Stop the suite if its server dies instead of reporting client failures."""
    # CTest otherwise exits successfully when a filter or missing registration
    # selects no tests, which cannot establish upstream FEX compatibility.
    tests = subprocess.Popen([*command, "--no-tests=error"], env=environment, start_new_session=True)
    try:
        while server.poll() is None:
            try:
                result = tests.wait(timeout=1)
            except subprocess.TimeoutExpired:
                continue
            if server.poll() is None:
                return result
            break
        raise RuntimeError(f"FEXServer exited during validation; see {log_path}")
    finally:
        if tests.poll() is None:
            try:
                os.killpg(tests.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                tests.wait(timeout=10)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(tests.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                tests.wait()


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
    # FEX falls back to '/' when HOME does not exist. A fresh build directory
    # must work without an earlier test run having created this state.
    home = build / "home"
    home.mkdir(mode=0o700, parents=True, exist_ok=True)
    environment["HOME"] = str(home)
    environment["XDG_CONFIG_HOME"] = str(home / ".config")
    environment["XDG_DATA_HOME"] = str(home / ".local/share")
    environment["XDG_CACHE_HOME"] = str(home / ".cache")
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
                return run_ctest(
                    ["ctest", "--test-dir", str(build), "--output-on-failure",
                     "--parallel", "2", "--timeout", "302", *extra],
                    environment, server, log_path,
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

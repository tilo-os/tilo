#!/usr/bin/env python3
"""Read back only the pairing harness's identified QEMU process; never signal it."""
import argparse
import json
import os
from pathlib import Path
import sys
import time

PROC_ROOT = Path("/proc")
QEMU_NAMES = {"qemu-system-x86_64", "qemu-system-aarch64", "qemu-kvm"}


def process(pid):
    if not (PROC_ROOT / "self" / "stat").read_text():
        raise ValueError("cannot observe the QEMU process namespace")
    directory = PROC_ROOT / str(pid)
    try:
        stat = (directory / "stat").read_text()
    except FileNotFoundError:
        return None
    fields = stat.rsplit(")", 1)[1].split()
    if len(fields) < 20 or int(stat.split(" ", 1)[0]) != pid:
        raise ValueError("cannot observe owned QEMU kernel identity")
    return {"pid": pid, "start_time": int(fields[19]), "state": fields[0]}


def snapshot(pidfile):
    pidfile = Path(pidfile).absolute()
    pid = int(pidfile.read_text().strip())
    if pid <= 1:
        raise ValueError("invalid owned QEMU pid")
    observed = process(pid)
    if observed is None or observed["state"] in {"Z", "X"}:
        raise ValueError("owned QEMU is not running after boot")
    directory = PROC_ROOT / str(pid)
    args = (directory / "cmdline").read_bytes().split(b"\0")
    args = [os.fsdecode(value) for value in args if value]
    executable = Path(os.readlink(directory / "exe")).name
    if not args or Path(args[0]).name not in QEMU_NAMES or executable not in QEMU_NAMES:
        raise ValueError("pid file does not identify a QEMU process")
    try:
        owned_path = Path(args[args.index("-pidfile") + 1]).absolute()
    except (ValueError, IndexError) as error:
        raise ValueError("QEMU does not name its owned pid file") from error
    if owned_path != pidfile:
        raise ValueError("QEMU belongs to another runtime")
    observed["pidfile"] = str(pidfile)
    return observed


def alive(identity):
    current = process(identity["pid"])
    return current is not None and current["start_time"] == identity["start_time"] and current["state"] not in {"Z", "X"}


def before_stop(identity):
    pidfile = Path(identity["pidfile"])
    try:
        current_pid = int(pidfile.read_text().strip())
    except FileNotFoundError:
        current_pid = None
    if current_pid is not None and current_pid != identity["pid"]:
        raise ValueError("owned QEMU pid file changed; normal stop refused")
    if not alive(identity):
        # Do not let a stale pid file make the normal stop signal a reused PID.
        current = process(identity["pid"])
        if current is not None and current["start_time"] != identity["start_time"]:
            raise ValueError("owned QEMU pid was reused; normal stop refused")
        return {**identity, "state": "already-exited"}
    observed = snapshot(identity["pidfile"])
    if observed["pid"] != identity["pid"] or observed["start_time"] != identity["start_time"]:
        raise ValueError("owned QEMU identity changed; normal stop refused")
    return {**identity, "state": "verified-before-stop"}


def after_stop(identity):
    deadline = time.monotonic() + 5
    while alive(identity):
        if time.monotonic() >= deadline:
            raise ValueError("owned QEMU is still alive after normal stop")
        time.sleep(0.1)
    return {**identity, "state": "owned-process-exited"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("snapshot", "before-stop", "after-stop"))
    parser.add_argument("path")
    options = parser.parse_args()
    if options.phase == "snapshot":
        result = snapshot(options.path)
    else:
        identity = json.loads(Path(options.path).read_text())
        if set(identity) != {"pid", "start_time", "state", "pidfile"}:
            raise ValueError("invalid owned QEMU identity receipt")
        if type(identity["pid"]) is not int or identity["pid"] <= 1:
            raise ValueError("invalid owned QEMU pid receipt")
        if type(identity["start_time"]) is not int or identity["start_time"] < 0:
            raise ValueError("invalid owned QEMU start-time receipt")
        if not isinstance(identity["pidfile"], str) or not Path(identity["pidfile"]).is_absolute():
            raise ValueError("invalid owned QEMU runtime receipt")
        result = before_stop(identity) if options.phase == "before-stop" else after_stop(identity)
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, IndexError, KeyError) as error:
        print(f"nearby VM cleanup cannot be verified: {error}", file=sys.stderr)
        raise SystemExit(1) from error

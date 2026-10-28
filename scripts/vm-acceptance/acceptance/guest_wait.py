"""Waits that run inside the guest and return the moment their event happens, so the harness
spends one `tilo-vm` round trip on a wait instead of one per poll.

    python3 - exit <app> <seconds>
        until no process belongs to <app> (its launcher's process tree, or a cgroup of its
        unit below the launcher's scope: core.parse_processes' rule), waiting on pidfds
    python3 - line <file> <from-line> <seconds> <regex>...
        until a line of <file> from line <from-line> on matches one of the regexes, woken by
        inotify where the file's writes reach it and reading again every 0.1 s where not

Exit status 0 when the event happened, 1 when the wait ran out, with what is left on stdout.
The harness sends this file's text as the script (`script()`); it runs in the guest's python3.
"""
import ctypes
import os
import re
import select
import sys
import time
from pathlib import Path

_APP_UNIT = re.compile(r"/tilo-apps-\d+\.scope/app-((?:[^/-]|\\x2d)+)-\d+(?:/|$)")


def script() -> str:
    return Path(__file__).read_text(encoding="utf-8")


def _cgroup_app(path: str):
    found = _APP_UNIT.search(path or "")
    return re.sub(r"\\x([0-9a-fA-F]{2})", lambda m: chr(int(m[1], 16)), found[1]) if found else None


def _table() -> dict:
    """{pid: (ppid, the app a launcher process launches or None, cgroup path)}."""
    table = {}
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            with open(f"/proc/{name}/stat", "rb") as f:
                stat = f.read()
            end = stat.rindex(b")")
            comm = stat[stat.index(b"(") + 1:end].decode(errors="replace")
            state, ppid = stat[end + 2:].split()[:2]
            if state == b"Z":
                continue  # exited, not yet reaped: core.processes leaves it out too
            ppid = int(ppid)
            launches = None
            if comm == "tilo-launcher":
                with open(f"/proc/{name}/cmdline", "rb") as f:
                    argv = f.read().split(b"\0")
                if b"--app-id" in argv and argv.index(b"--app-id") + 1 < len(argv):
                    launches = argv[argv.index(b"--app-id") + 1].decode(errors="replace")
            with open(f"/proc/{name}/cgroup") as f:
                cgroup = next((line[3:].strip() for line in f if line.startswith("0::")), "")
        except (OSError, ValueError):
            continue
        table[int(name)] = (ppid, launches, cgroup)
    return table


def app_pids(app: str) -> list[int]:
    table = _table()
    pids = []
    for pid, (_, _, cgroup) in table.items():
        cur, seen = pid, set()
        while cur in table and table[cur][1] is None and cur not in seen:
            seen.add(cur)
            cur = table[cur][0]
        owner = (table[cur][1] if cur in table else None) or _cgroup_app(cgroup)
        if owner == app:
            pids.append(pid)
    return pids


def wait_exit(app: str, seconds: float) -> int:
    deadline = time.monotonic() + seconds
    while True:
        pids = app_pids(app)
        if not pids:
            return 0
        left = deadline - time.monotonic()
        if left <= 0:
            print(" ".join(map(str, pids)))
            return 1
        poll, fds = select.poll(), []
        for pid in pids:
            try:
                fd = os.pidfd_open(pid)
            except ProcessLookupError:
                continue
            fds.append(fd)
            poll.register(fd, select.POLLIN)
        try:
            # A pidfd turns readable when its process exits; then the app's processes are
            # read again (one may have started another on its way out).
            if fds:
                poll.poll(left * 1000)
        finally:
            for fd in fds:
                os.close(fd)


IN_MODIFY, IN_ATTRIB, IN_MOVE_SELF, IN_DELETE_SELF = 0x2, 0x4, 0x800, 0x400


def wait_line(path: str, first: int, seconds: float, patterns: list[str]) -> int:
    deadline = time.monotonic() + seconds
    libc = ctypes.CDLL(None, use_errno=True)
    fd = libc.inotify_init1(os.O_NONBLOCK | os.O_CLOEXEC)
    if fd < 0 or libc.inotify_add_watch(fd, os.path.realpath(path).encode(),
                                        IN_MODIFY | IN_ATTRIB | IN_MOVE_SELF | IN_DELETE_SELF) < 0:
        print("no inotify watch on the file", file=sys.stderr)
        return 2
    wanted = [re.compile(p.encode()) for p in patterns]
    number, partial = 0, b""
    with open(path, "rb") as f:
        while True:
            chunk = f.read()
            if chunk:
                lines = (partial + chunk).split(b"\n")
                partial = lines.pop()
                for line in lines:
                    number += 1
                    # Coloured log lines carry escapes between a field's name and value.
                    plain = re.sub(rb"\x1b\[[0-9;]*m", b"", line)
                    if number >= first and any(w.search(plain) for w in wanted):
                        print(line.decode(errors="replace")[:400])
                        return 0
                continue
            left = deadline - time.monotonic()
            if left <= 0:
                return 1
            # The session writes its log through the slot's own mount of the home, and
            # inotify sees no write made through another mount of an overlay: a wakeup on
            # an event where there is one, else after a tenth of a second, to read again.
            select.select([fd], [], [], min(left, 0.1))
            try:
                os.read(fd, 4096)
            except BlockingIOError:
                pass


if __name__ == "__main__":
    what = sys.argv[1]
    if what == "exit":
        sys.exit(wait_exit(sys.argv[2], float(sys.argv[3])))
    if what == "line":
        sys.exit(wait_line(sys.argv[2], int(sys.argv[3]), float(sys.argv[4]), sys.argv[5:]))
    sys.exit(f"unknown wait {what!r}")

#!/usr/bin/env python3
"""Keep Chromium's descendants owned until they have all stopped writing."""
import ctypes
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


stopping = False


def stop(_number, _frame):
    global stopping
    stopping = True


def children():
    result = []
    for entry in Path('/proc').iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            stat = (entry / 'stat').read_text()
        except (FileNotFoundError, ProcessLookupError):
            continue  # Another process exited; waitpid still proves our children.
        fields = stat[stat.rindex(')') + 2:].split()
        if int(fields[1]) == os.getpid():
            result.append((int(entry.name), fields[19]))
    return result


def reap():
    while True:
        try:
            pid, _status = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return True
        if pid == 0:
            return False


def quiesce():
    started = time.monotonic()
    sent = set()
    while not reap():
        elapsed = time.monotonic() - started
        if elapsed > 5:
            raise RuntimeError('owned browser descendants did not exit; profile must remain')
        action = signal.SIGKILL if elapsed > 2 else signal.SIGTERM
        for pid, birth in children():
            key = (pid, birth, action)
            if key in sent:
                continue
            try:
                descriptor = os.pidfd_open(pid)
            except ProcessLookupError:
                continue
            try:
                # Bind the snapshot to the opened process before signalling: a
                # PID reused after discovery must never target another job.
                stat = Path(f'/proc/{pid}/stat').read_text()
                fields = stat[stat.rindex(')') + 2:].split()
                if int(fields[1]) != os.getpid() or fields[19] != birth:
                    continue
                signal.pidfd_send_signal(descriptor, action)
            except (FileNotFoundError, ProcessLookupError):
                pass
            finally:
                os.close(descriptor)
            sent.add(key)
        time.sleep(0.01)


def main():
    if len(sys.argv) < 2:
        raise ValueError('supply the owned browser executable and arguments')
    # Orphaned grandchildren return to this supervisor, including a utility
    # process which outlives Chromium's root or starts a separate session.
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), 'cannot own browser descendants')
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    browser = subprocess.Popen(sys.argv[1:])
    while not stopping and browser.poll() is None:
        time.sleep(0.02)
    status = browser.returncode
    quiesce()
    browser.wait()  # Record the root's completion after waitpid reaped it.
    if stopping or status == 0:
        return 0
    # The browser ended on its own and badly; say how for whoever reads why.
    print(f'owned browser exited on its own with status {status}', file=sys.stderr, flush=True)
    return 1


if __name__ == '__main__':
    sys.exit(main())

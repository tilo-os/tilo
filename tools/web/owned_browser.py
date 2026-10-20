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


def owned_fields(pid, birth):
    stat = Path(f'/proc/{pid}/stat').read_text()
    fields = stat[stat.rindex(')') + 2:].split()
    if int(fields[1]) == os.getpid() and fields[19] == birth:
        return fields
    return None


def describe_children(sent):
    states = []
    for pid, birth in children():
        try:
            fields = owned_fields(pid, birth)
        except (FileNotFoundError, ProcessLookupError):
            continue
        if fields is not None:
            signals = ','.join(action.name for action in (signal.SIGTERM, signal.SIGKILL)
                               if (pid, birth, action) in sent) or 'none'
            states.append(f'pid={pid} birth={birth} state={fields[0]} sent={signals}')
    return '; '.join(states) or 'no visible owned children (waitpid not yet proven)'


def quiesce():
    started = time.monotonic()
    kill_deadline = None
    sent = set()
    while not reap():
        elapsed = time.monotonic() - started
        action = signal.SIGKILL if kill_deadline is not None or elapsed > 2 else signal.SIGTERM
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
                if owned_fields(pid, birth) is None:
                    continue
                signal.pidfd_send_signal(descriptor, action)
            except (FileNotFoundError, ProcessLookupError):
                pass
            finally:
                os.close(descriptor)
            sent.add(key)
        if action == signal.SIGKILL:
            if kill_deadline is None:
                # A starved supervisor may first run again after the TERM
                # budget has elapsed. Always attempt KILL before starting its
                # bounded reaping window; never renew it for adopted children.
                kill_deadline = time.monotonic() + 3
                print(f'owned browser KILL phase: {describe_children(sent)}',
                      file=sys.stderr, flush=True)
            elif time.monotonic() >= kill_deadline:
                if reap():
                    return
                raise RuntimeError('owned browser descendants did not exit; profile must remain; '
                                   + describe_children(sent))
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

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
# How long the browser's descendants get to close after TERM before KILL.
# Fixtures that always honour TERM raise it so a loaded host cannot turn
# their graceful close into an escalation.
TERM_GRACE = float(os.environ.get('TILO_OWNED_BROWSER_TERM_GRACE_SECS', '2'))


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


def describe_threads(pid):
    """Each live thread of `pid` and where in the kernel it waits: a killed
    process stays an unreapable zombie until every thread has exited, so a
    thread stuck in uninterruptible sleep (D) names what holds it."""
    threads = []
    try:
        tasks = sorted(Path(f'/proc/{pid}/task').iterdir(), key=lambda entry: int(entry.name))
    except (FileNotFoundError, ProcessLookupError, NotADirectoryError):
        return ''
    for task in tasks:
        def read(name):
            try:
                return (task / name).read_text().strip()
            except (OSError, ValueError):
                return ''
        stat = read('stat')
        if not stat:
            continue
        state = stat[stat.rindex(')') + 2:].split()[0]
        detail = f'{task.name}:{state}'
        wchan = read('wchan')
        if wchan and wchan != '0':
            detail += f'@{wchan}'
        stack = read('stack')
        if stack and state == 'D':
            frames = [line.split(' ', 1)[-1].split('+', 1)[0] for line in stack.splitlines()][:6]
            detail += '<' + '<'.join(frames)
        threads.append(detail)
    if not threads:
        return ''
    described = ' threads=' + ','.join(threads)
    if any(':D' in thread for thread in threads):
        # What its threads may be closing: an exiting thread in D is usually
        # releasing one of these.
        targets = {}
        try:
            for fd in Path(f'/proc/{pid}/fd').iterdir():
                try:
                    target = os.readlink(fd)
                except OSError:
                    continue
                kind = target.split(':', 1)[0] if ':' in target and not target.startswith('/') else target
                targets[kind] = targets.get(kind, 0) + 1
        except OSError:
            pass
        if targets:
            described += ' fds=' + ','.join(f'{kind}x{count}' for kind, count in sorted(targets.items())[:40])
    return described


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
            states.append(f'pid={pid} birth={birth} state={fields[0]} sent={signals}{describe_threads(pid)}')
    return '; '.join(states) or 'no visible owned children (waitpid not yet proven)'


def quiesce():
    started = time.monotonic()
    kill_deadline = None
    sent = set()
    while not reap():
        elapsed = time.monotonic() - started
        action = signal.SIGKILL if kill_deadline is not None or elapsed > TERM_GRACE else signal.SIGTERM
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
    code = main()
    # Interpreter shutdown puts the default TERM and INT actions back. A close
    # request landing then (the caller sends TERM after a grace period, which
    # a loaded host can make overlap a clean exit) would end the supervisor by
    # that signal after it proved every descendant reaped, and its caller
    # reads an exit by signal as a failed shutdown. Every descendant is
    # already reaped here, so the request has nothing left to stop.
    signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
    sys.exit(code)

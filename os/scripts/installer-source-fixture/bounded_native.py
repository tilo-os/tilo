"""Bounded owned children; unknown retirement keeps the actual Child alive."""
import os
import selectors
import signal
import subprocess
import time

BOUND = 1024 * 1024
_OUTSTANDING = {}


def retain_unknown(process):
    _OUTSTANDING[id(process)] = process


def assert_children_retired():
    if _OUTSTANDING:
        raise RuntimeError('owned child retirement unknown; preserve native fixture namespace')


def observe_exit(process):
    """Observe without reaping: the leader PID stays reserved for group cleanup."""
    if process.returncode is not None:
        raise RuntimeError('owned child was already reaped outside its observer')
    return os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)


def retire_child(process, timeout=5):
    if process.returncode is not None:
        return
    try:
        # ECHILD means ownership is unknown. Never signal its numeric group then.
        observe_exit(process)
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=timeout)
    except BaseException:
        retain_unknown(process)
        raise RuntimeError('owned child retirement unknown; preserve native fixture namespace')


def bounded(argv, *, timeout=1800, pass_fds=(), limit=BOUND, test_manifest=None, stdin=None):
    if timeout <= 0 or not 0 < limit <= 16 * BOUND:
        raise ValueError('positive bounded command limits required')
    environment = {'PATH': '/usr/bin', 'LC_ALL': 'C', 'LANG': 'C'}
    if test_manifest is not None:
        environment['TILO_CLEAN_NATIVE_FIXTURE'] = test_manifest
    process = None
    selector = None
    try:
        process = subprocess.Popen(argv, env=environment,
                                   stdin=subprocess.DEVNULL if stdin is None else stdin,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   start_new_session=True, pass_fds=pass_fds)
        selector = selectors.DefaultSelector()
        for index, pipe in enumerate((process.stdout, process.stderr)):
            os.set_blocking(pipe.fileno(), False)
            selector.register(pipe, selectors.EVENT_READ, index)
        output = [bytearray(), bytearray()]
        deadline = time.monotonic() + timeout
        observed = None
        while selector.get_map() or observed is None:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise RuntimeError('native qualification command deadline exceeded')
            for key, _ in selector.select(min(.05, remaining)):
                part = os.read(key.fileobj.fileno(), min(8192, limit + 1))
                if not part:
                    selector.unregister(key.fileobj)
                else:
                    output[key.data].extend(part)
                    if sum(map(len, output)) > limit:
                        raise RuntimeError('native qualification command output exceeded bound')
            if observed is None:
                observed = observe_exit(process)
        status = observed.si_status if observed.si_code == os.CLD_EXITED else -observed.si_status
        return status, bytes(output[0]), bytes(output[1])
    finally:
        # Setup failures also enter this custody path. No unbounded wait, and
        # nothing signals a leader after wait/reap made its PID reusable.
        try:
            if process is not None:
                retire_child(process)
        finally:
            if selector is not None:
                selector.close()
            if process is not None:
                for pipe in (process.stdout, process.stderr):
                    if pipe is not None:
                        pipe.close()

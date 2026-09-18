"""Make a spawned compositor die when the script that spawned it does.

Every harness here starts a compositor, and each one costs the better
part of a core while it lives. They are started with `start_new_session=True`
so a `--keep` run can outlive the script — which also means a script that is
killed, times out, or panics leaves them running indefinitely and can
consume most of a machine.

`atexit` and signal handlers do not fix it: they do not run on `SIGKILL`,
and that is exactly how a hung harness is usually stopped.

So the guarantee has to come from the kernel. Two mechanisms, together:

- **`PR_SET_PDEATHSIG`** in the child, so the kernel sends it `SIGKILL` the
  moment its parent dies, whatever killed the parent. This is what covers
  the `SIGKILL`-the-script case that nothing in userspace can.
- **A tracked-pid sweep** on normal exit, which also catches the case where
  the child has outlived an *intermediate* parent and been reparented.

Usage — replace `start_new_session=True` with `preexec_fn=die_with_parent()`:

    proc = subprocess.Popen(argv, env=env, preexec_fn=die_with_parent())
    track(proc)                       # optional: also swept at exit

A harness that wants survivors (`--keep`) passes
`die_with_parent(enabled=False)` and says so on the command line.
"""
from __future__ import annotations

import atexit
import ctypes
import os
import signal
import subprocess

# prctl(2): send the calling process this signal when its parent dies.
_PR_SET_PDEATHSIG = 1

_tracked: list[subprocess.Popen] = []


def die_with_parent(enabled: bool = True):
    """A `preexec_fn` that asks the kernel to kill this child with its parent.

    Returns `None` when disabled, which `Popen` accepts as "no preexec_fn",
    so a caller can pass the result through unconditionally.
    """
    if not enabled:
        return None

    def _preexec() -> None:
        # Runs in the forked child, before exec. Anything raised here
        # becomes a failed spawn, so failure is swallowed: losing the
        # guarantee is better than losing the run, and the exit sweep
        # below still covers the ordinary paths.
        try:
            libc = ctypes.CDLL("libc.so.6", use_errno=True)
            libc.prctl(_PR_SET_PDEATHSIG, signal.SIGKILL, 0, 0, 0)
        except Exception:
            pass
        # A parent that already died between fork and prctl would leave
        # this child orphaned with the signal never delivered. Re-check.
        if os.getppid() == 1:
            os._exit(0)

    return _preexec


def track(proc: subprocess.Popen) -> subprocess.Popen:
    """Also kill `proc` from an `atexit` sweep. Returns it, so it chains."""
    _tracked.append(proc)
    return proc


def _sweep() -> None:
    for proc in _tracked:
        if proc.poll() is not None:
            continue
        try:
            proc.kill()
        except Exception:
            pass


atexit.register(_sweep)

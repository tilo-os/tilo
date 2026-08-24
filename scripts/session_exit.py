"""Say how a process ended, in words, rather than leaving it to be inferred.

A rig that loses its control socket sees the same three exceptions —
`FileNotFoundError`, `ConnectionRefusedError`, `ConnectionResetError` —
whether the session was closed on purpose or died. It reported them
identically, and "the take ended because: ConnectionResetError" is
consistent with somebody clicking the close button and with the compositor
crashing. Nothing in the run distinguished them.

That gap cost real time. Applications were seen aborting at the same second
as a session ending, and the ordering was read off the timestamps as "the
applications died first, so something took the compositor down with them".
The compositor had in fact exited cleanly; there was no compositor crash to
find. A single line saying so would have settled it in the run that
produced the evidence, instead of in an investigation afterwards.

So: capture the exit status, and say which of the three things happened —
a clean exit, a signal, or a crash that left a core.

Usage, from any rig:

    import session_exit
    # BEFORE teardown: after stop() every status is the rig's own SIGTERM.
    ends = [(s.name, session_exit.how_it_ended(s.proc)) for s in sessions]
    ...
    for name, how in ends:
        print(f"  {name}: compositor {how}")

`how_it_ended` never raises and never blocks: a rig's own reporting must not
be the thing that fails.
"""
from __future__ import annotations

import shutil
import signal
import subprocess

__all__ = ["how_it_ended", "left_a_core"]


def left_a_core(pid: int) -> bool | None:
    """Whether a core was recorded for `pid`.

    `True` / `False` when `coredumpctl` could answer, `None` when it is
    absent or unwilling — which is not the same as "no core", and is
    reported as the uncertainty it is.
    """
    if not shutil.which("coredumpctl"):
        return None
    try:
        done = subprocess.run(
            ["coredumpctl", "info", str(pid)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            timeout=10,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    # 1 is "no such coredump"; anything else unexpected is not an answer.
    if done.returncode == 0:
        return True
    return False if done.returncode == 1 else None


def _signal_name(number: int) -> str:
    try:
        return signal.Signals(number).name
    except ValueError:
        return f"signal {number}"


def how_it_ended(proc: "subprocess.Popen | None") -> str:
    """One sentence naming how `proc` ended, for a run's own report.

    Does not wait: the answer is about the state at the moment of asking,
    which is the point — asked before teardown it says whether the process
    was already gone, and asked after it says how teardown ended it.
    """
    if proc is None:
        return "was never started"

    status = proc.poll()
    if status is None:
        return f"is still running (pid {proc.pid})"

    if status >= 0:
        if status == 0:
            return "exited cleanly (status 0)"
        return f"exited with status {status} — it failed, it did not crash"

    # Negative means a signal. This is the case the report exists for.
    number = -status
    name = _signal_name(number)
    deliberate = number in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP)
    core = left_a_core(proc.pid)

    if core is True:
        return (f"CRASHED: killed by {name} ({number}) and left a core — "
                f"`coredumpctl info {proc.pid}`")
    if deliberate and core is False:
        return (f"was stopped by {name} ({number}), no core — "
                f"that is a deliberate shutdown, not a crash")
    if core is False:
        return f"was killed by {name} ({number}), no core recorded"
    return (f"was killed by {name} ({number}); whether it left a core is "
            f"unknown (coredumpctl gave no answer)")

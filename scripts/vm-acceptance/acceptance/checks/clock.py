"""A clock that comes up behind the device certificate.

K01 starts the session and the filesystem daemon with the VM's clock set a year back, before
any certificate this VM holds was issued, as a real-time clock that lost its count leaves a
machine. The identity is intact, so the desktop must not offer first-use setup or a restore:
it says that it waits for the clock. fs stays up waiting instead of exiting into a restart
loop. When the clock steps back to the real time, as network time does, fs serves the
workspace and the session starts its network and broker without anyone acting.

It moves the VM's clock and restarts the session, so it runs after the checks that judge what
the run opened, and puts the clock and chronyd back however it ends.
"""

from __future__ import annotations

import shlex

from ..core import CheckFailed, check, expect
from ..vm import wait_until

AREA = "basics"
YEAR_S = 366 * 24 * 3600
FS_UNIT = "fs.service"


def _fs(ctx) -> dict:
    out = ctx.vm.root_exec(f"systemctl show {FS_UNIT} -p ActiveState -p SubState -p NRestarts "
                           f"-p ExecMainStartTimestampMonotonic", timeout=60)
    return dict(line.split("=", 1) for line in out.splitlines() if "=" in line)


def _fs_said(ctx, since: str, text: str) -> bool:
    out = ctx.vm.root_exec(f"journalctl -u {FS_UNIT} --since {shlex.quote(since)} -o cat "
                           f"--no-pager | grep -F -c {shlex.quote(text)} || true", timeout=60)
    return out.strip().splitlines()[-1:] != ["0"]


def _real_now(ctx, real: int, behind: int) -> int:
    """The real time now: the VM's clock reading, moved by however far it was set back,
    when it still reads the setback time."""
    vm_now = int(ctx.vm.root_exec("date +%s").split()[0])
    return vm_now if vm_now >= real else real + (vm_now - behind)


@check("K01", AREA, "A clock behind the device certificate: no setup or restore; "
                    "fs and the workspace come up when the clock steps")
def clock_behind_the_certificate(ctx, res):
    real = int(ctx.vm.root_exec("date +%s").split()[0])
    behind = real - YEAR_S
    marker = f"@{behind}"
    ctx.vm.root_exec("systemctl stop chronyd.service || true", timeout=60)
    try:
        ctx.vm.root_exec(f"date -s @{behind} >/dev/null && systemctl restart {FS_UNIT}", timeout=120)
        ctx.vm.restart_session(timeout=180)
        # The shell's state dump carries the desktop's own fields under `chrome`.
        state = ctx.vm.state()["chrome"]
        res.metrics["identity_while_behind"] = state.get("identity")
        expect(not state["input_capture"]["onboarding"],
               "the desktop shows first-use setup for an identity it has")
        expect(not state["identity"]["recovery"], "the desktop offers to restore an intact identity")
        expect(state["identity"]["waits_for_clock"],
               "the desktop does not say that it waits for the clock")

        # fs waits: up, not restarting every few seconds.
        first = _fs(ctx)
        ctx.settle(12)
        waiting = _fs(ctx)
        res.metrics["fs_while_behind"] = waiting
        expect(waiting.get("ActiveState") == "active",
               f"fs is {waiting.get('ActiveState')}/{waiting.get('SubState')} while the clock is behind")
        expect(waiting.get("NRestarts") == first.get("NRestarts"),
               "fs restarts in a loop while the clock is behind")
        expect(_fs_said(ctx, marker, "waiting for the clock"), "fs does not say it waits for the clock")

        # The clock steps to the real time, as network time steps it; nobody touches
        # the desktop.
        stepped_to = _real_now(ctx, real, behind)
        ctx.vm.root_exec(f"date -s @{stepped_to} >/dev/null", timeout=60)
        stepped = ctx.wait_state(
            lambda s: not s["chrome"]["identity"]["waits_for_clock"]
            and s["chrome"]["collab"]["available"],
            90, "the session's network and broker after the clock stepped")["chrome"]
        res.metrics["identity_after_step"] = stepped.get("identity")
        expect(not stepped["input_capture"]["onboarding"], "setup appeared after the clock stepped")
        if not wait_until(lambda: _fs_said(ctx, f"@{stepped_to}", "Enforcing"), 60, interval=2):
            raise CheckFailed("fs did not start serving after the clock stepped")
        after = _fs(ctx)
        res.metrics["fs_after_step"] = after
        expect(after.get("ActiveState") == "active" and after.get("NRestarts") == first.get("NRestarts"),
               "fs did not come up in the process that waited")
    finally:
        restore = _real_now(ctx, real, behind)
        ctx.vm.root_exec(f"date -s @{restore} >/dev/null; systemctl start chronyd.service || true",
                         timeout=60)

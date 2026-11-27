"""A clock that comes up behind the device certificate.

K01 starts the session and the filesystem daemon with the VM's clock set a year back, before
any certificate this VM holds was issued, as a real-time clock that lost its count leaves a
machine. The identity is intact, so the desktop must not offer first-use setup, and it
answers with its displays. fs stays up waiting instead of exiting into a restart loop. When
the clock steps back to the real time, as network time does, fs serves the workspace and the
session lists its apps without anyone acting.

It moves the VM's clock and restarts the session, so it runs after the checks that judge what
the run opened, and puts the clock and chronyd back however it ends.

fs.service serves the device's own identity (/var/lib/tilo/device/certs) and does not
start without it (its ConditionPathExists). A tilo-vm account's session keeps a test identity
of its own under TILO_IDENTITY_ROOT instead, so on such a VM there is no fs to wait: the
check says so in its numbers and judges the desktop's half alone.
"""

from __future__ import annotations

import shlex
import time

from .. import core
from ..core import CheckFailed, check, expect
from ..vm import wait_until

AREA = "basics"
YEAR_S = 366 * 24 * 3600
FS_UNIT = "fs.service"
DEVICE_CERTS = "/var/lib/tilo/device/certs"
# fs is watched for a fixed window while the clock is behind: a restart loop shows as
# restarts within it.
FS_WATCH_S = 12


def _fs(ctx) -> dict:
    out = ctx.vm.root_exec(f"systemctl show {FS_UNIT} -p ActiveState -p SubState -p NRestarts "
                           f"-p ExecMainStartTimestampMonotonic", timeout=60)
    return dict(line.split("=", 1) for line in out.splitlines() if "=" in line)


def _fs_serves_this_device(ctx) -> bool:
    """Whether fs.service has the device identity it serves (see the module's doc)."""
    return ctx.vm.root_exec(f"ls {DEVICE_CERTS}/*.bin >/dev/null 2>&1 && echo yes || echo no", timeout=60).split()[-1:] == ["yes"]


def _fs_said(ctx, since: str, text: str) -> bool:
    out = ctx.vm.root_exec(f"journalctl -u {FS_UNIT} --since {shlex.quote(since)} -o cat "
                           f"--no-pager | grep -F -c {shlex.quote(text)} || true", timeout=60)
    return out.strip().splitlines()[-1:] != ["0"]


def _real_now(ctx, real: int, behind: int) -> int:
    """The real time now: the VM's clock reading, moved by however far it was set back,
    when it still reads the setback time."""
    vm_now = int(ctx.vm.root_exec("date +%s").split()[0])
    return vm_now if vm_now >= real else real + (vm_now - behind)


@check("K01", AREA, "A clock behind the device certificate: no first-use setup; "
                    "fs and the workspace come up when the clock steps")
def clock_behind_the_certificate(ctx, res):
    real = int(ctx.vm.root_exec("date +%s").split()[0])
    behind = real - YEAR_S
    marker = f"@{behind}"
    ctx.vm.root_exec("systemctl stop chronyd.service || true", timeout=60)
    try:
        ctx.vm.root_exec(f"date -s @{behind} >/dev/null && systemctl restart {FS_UNIT}", timeout=120)
        ctx.vm.restart_session(timeout=180)
        # The desktop a person sees: its own controls, none of first-use setup for an identity
        # it has.
        setup = [t["name"] for t in ctx.vm.targets() if t["name"].startswith("onboarding")]
        res.metrics["setup_targets"] = setup
        expect(not setup, f"the desktop shows first-use setup for an identity it has: {setup[:3]}")
        expect(ctx.state()["chrome"].get("session_locked") is False,
               "the desktop came up locked with an identity it has")

        # fs waits: up, not restarting every few seconds.
        with_fs = _fs_serves_this_device(ctx)
        if not with_fs:
            # The desktop's half still applies and is judged; the fs half is not seen, and
            # the detail says so on a pass as on a failure.
            why = (f"fs part skipped: no device certificate at {DEVICE_CERTS}, so fs.service's "
                   "condition never starts it; this account's identity is its own")
            res.metrics["fs"] = why
            res.detail = why
        first = _fs(ctx)
        if with_fs:
            time.sleep(FS_WATCH_S)
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
        ctx.vm.wait_catalogue(90)
        stepped = ctx.wait_state(lambda s: len(core.display_rects(s)) > 0, 90,
                                 "the session answering after the clock stepped")
        res.metrics["displays_after_step"] = len(core.display_rects(stepped))
        setup = [t["name"] for t in ctx.vm.targets() if t["name"].startswith("onboarding")]
        expect(not setup, f"setup appeared after the clock stepped: {setup[:3]}")
        if not with_fs:
            return
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

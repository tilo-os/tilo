"""The slot's session unit, and a restart of it nobody asked for.

The slot's desktop runs as the system unit `tilo-vm-session@<slot>.service`
(crates/dev/tilo-vmd/data/tilo-vm-session@.service): its main process is
`tilo-session`, the wrapper that runs the compositor and exits when it does.
`Restart=always` starts a fresh session two seconds later, with every window gone.
Nothing else in the run would notice: the user journal P03 reads does not hold a
system unit, and a check that was opening an app only sees its app not arrive.

systemd counts those restarts in the unit's `NRestarts` and stamps each exit of the
main process in `ExecMainExitTimestamp`; a requested restart (`tilo-vm slot restart`,
the harness's own after a check left the session locked) does not count up, may reset
the counter, and is known to the harness (`TiloVm.requested_restarts`). `run_check`
reads both after every check and fails the check during which the session ended on its
own, with the dump below beside it.
"""

from __future__ import annotations

import re
import shlex

from .vm import VmError

# What `systemctl show` says about how the main process last ended.
SHOW_FIELDS = ("NRestarts", "Result", "ExecMainCode", "ExecMainStatus", "MainPID",
               "ExecMainStartTimestamp", "ExecMainExitTimestamp")


def unit(slot: str) -> str:
    return f"tilo-vm-session@{slot}.service"


def parse_show(text: str) -> dict[str, str]:
    """`systemctl show -p A -p B` output as {A: value, B: value}."""
    out = {}
    for line in text.splitlines():
        key, sep, value = line.partition("=")
        if sep and key.strip() in SHOW_FIELDS:
            out[key.strip()] = value.strip()
    return out


def show(ctx) -> dict[str, str] | None:
    """The slot session unit's `SHOW_FIELDS`, read as root over ssh so a session that is
    down or restarting still answers. None when it cannot be read."""
    fields = " ".join(f"-p {name}" for name in SHOW_FIELDS)
    try:
        text = ctx.vm.root_exec(f"systemctl show {fields} {shlex.quote(unit(ctx.vm.slot))}", timeout=60)
    except (VmError, AttributeError):
        return None
    parsed = parse_show(text)
    return parsed if parsed.get("NRestarts", "").isdigit() else None


def restarted(before: int | None, after: int | None) -> bool:
    """Whether the unit restarted on its own between two `NRestarts` readings. A requested
    restart resets the counter, so a lower reading that is not zero counts too."""
    if before is None or after is None:
        return False
    return after > before or (after < before and after > 0)


def ended_on_its_own(before: dict[str, str] | None, after: dict[str, str] | None, asked: bool,
                     pending: bool) -> tuple[bool, bool]:
    """(whether the session's main process ended on its own between two `show` readings,
    whether `Restart=` is still to count that ending). `NRestarts` growing is an ending.
    So is a new exit timestamp while the counter has not moved: the reading fell in the two
    seconds before `Restart=` starts the next session (once it runs, systemd clears the exit
    fields), and the next reading's one restart is then that same ending (`pending`).
    `asked`: the harness restarted the session itself in between, which ends the main
    process too but never counts up."""
    if before is None or after is None:
        return False, pending
    grew = int(after["NRestarts"]) - int(before["NRestarts"])
    exited = after.get("ExecMainExitTimestamp", "")
    new_exit = bool(exited) and exited != before.get("ExecMainExitTimestamp", "")
    if restarted(int(before["NRestarts"]), int(after["NRestarts"])):
        return not (pending and grew == 1 and not new_exit), False
    if new_exit and not asked:
        return True, True
    return False, pending and grew == 0


def how_it_ended(fields: dict[str, str], journal: str = "") -> str:
    """How the session's main process last ended, in systemd's words: its exit fields, or
    once the next session runs (systemd clears them), the last "Main process exited" line
    of `journal`. tilo-session exits with the compositor's status, 128 + a signal's number
    when a signal ended it (139: SIGSEGV)."""
    code, status = fields.get("ExecMainCode", "0"), fields.get("ExecMainStatus", "?")
    # ExecMainCode is CLD_*: 1 exited (status is the exit code), 2 killed, 3 dumped core
    # (status is the signal).
    kind = {"1": "exited with status", "2": "was killed by signal", "3": "dumped core on signal"}.get(code)
    if kind:
        return f"tilo-session {kind} {status} at {fields.get('ExecMainExitTimestamp') or '?'}"
    found = MAIN_EXITED.findall(journal)
    if found:
        code, status = found[-1]
        return f"tilo-session ended (systemd: code={code}, status={status})"
    return "tilo-session ended; systemd no longer holds how (the next session already runs)"


def dump_script(slot: str) -> str:
    """A root shell script that prints what the slot session's last run left behind: the
    unit's state and systemd's lines on it, the compositor's flight records (the run that
    ended is `previous.log` once the next has started, `latest.log` before; its end record
    names the exit status), its panic reports, and the kernel's (a `segfault at` line names
    the faulting library), coredump's and the user manager's view of the same seconds."""
    u = shlex.quote(unit(slot))
    ident = shlex.quote(f"tilo-session-{slot}")
    noise = r"tilo-sandbox-exec\|MESA\|tilo::launch_latency"
    return (
        f"u={u}; user=$(systemctl show -p User --value $u); uid=$(id -u \"$user\" 2>/dev/null); "
        "home=$(getent passwd \"$user\" | cut -d: -f6); logs=\"$home/.local/state/tilo/session-logs\"; "
        f"echo '=== unit'; systemctl show $u {' '.join(f'-p {f}' for f in SHOW_FIELDS)} 2>&1; "
        "echo '=== systemd on the unit'; journalctl -b -u $u --no-pager -o short-precise 2>/dev/null "
        f"| grep -av '{noise}' | tail -n 60 | cut -c1-600; "
        "echo '=== flight records'; ls -la \"$logs\" 2>&1; "
        "grep -ah 'tilo-flight-record-v1' \"$logs/previous.log\" \"$logs/latest.log\" 2>/dev/null; "
        # Read before the next session starts, the run that ended is still latest.log.
        "for f in previous.log latest.log; do "
        "echo \"=== $f: panics, signals and errors\"; sed 's/\\x1b\\[[0-9;]*m//g' \"$logs/$f\" 2>/dev/null "
        "| grep -aiE -B2 -A12 'panicked|panic|abort|SIG[A-Z]+|fatal|crash log|thread .* overflow|out of memory' "
        f"| grep -av '{noise}' | tail -n 120 | cut -c1-600; "
        "echo \"=== $f: its last lines\"; sed 's/\\x1b\\[[0-9;]*m//g' \"$logs/$f\" 2>/dev/null "
        f"| grep -av '{noise}' | tail -n 100 | cut -c1-600; done; "
        "echo '=== panic reports'; ls -lat \"$home/.cache/tilo/crashes\" 2>&1 | head -n 10; "
        "for f in $(ls -t \"$home/.cache/tilo/crashes\"/*.log 2>/dev/null | head -n 2); do "
        "echo \"--- $f\"; head -n 120 \"$f\" | cut -c1-400; done; "
        f"echo '=== the session output in the journal'; journalctl -b -t {ident} --no-pager -o short-precise "
        "2>/dev/null | grep -aiE 'panick|abort|signal|segfault|fatal|crash|flight-record|restart|exit|killed' "
        f"| grep -av '{noise}' | tail -n 80 | cut -c1-600; "
        "echo '=== kernel'; journalctl -b -k --no-pager -o short-precise 2>/dev/null "
        "| grep -aiE 'segfault|traps:|general protection|oom|killed process|out of memory|blocked for more' "
        "| tail -n 40 | cut -c1-400; "
        "echo \"=== coredumps (core_pattern $(cat /proc/sys/kernel/core_pattern), "
        "suid_dumpable $(cat /proc/sys/fs/suid_dumpable))\"; coredumpctl --no-pager list 2>/dev/null | tail -n 8; "
        "coredumpctl --no-pager info -1 2>/dev/null | head -n 250 | cut -c1-400; "
        "echo '=== system journal'; journalctl -b --no-pager -o short-precise 2>/dev/null "
        "| grep -aE 'tilo-vm-session|systemd-coredump|oom|logind.*(Removed|Session)|tilo-vmd' "
        "| tail -n 60 | cut -c1-400; "
        "echo '=== user manager'; journalctl -b _UID=$uid --no-pager -o short-precise 2>/dev/null "
        "| grep -aiE 'graphical-session|tilo-graphical|stopp|fail|killed|signal|dumped' | tail -n 60 | cut -c1-400; true"
    )


MAIN_EXITED = re.compile(r"Main process exited, code=(\w+), status=(\S+)")


def dump(ctx, label: str, fields: dict[str, str] | None) -> tuple[str, str]:
    """Write the dump as `data/<label>-session-restart.log`. Returns its name and how the
    main process ended: from `fields` while systemd still holds the exit, else systemd's
    last "Main process exited" line in the dump (the next session's start clears it)."""
    name = f"{label}-session-restart.log"
    try:
        text = ctx.vm.root_exec(dump_script(ctx.vm.slot), timeout=180)
    except (VmError, AttributeError) as exc:
        text = f"the session dump could not be read: {exc}"
    ctx.write(name, text)
    return f"data/{name}", how_it_ended(fields or {}, text)

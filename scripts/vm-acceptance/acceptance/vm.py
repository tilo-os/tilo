"""The suite's only door into the VM: the `tilo-vm` CLI.

Everything goes through `tilo-vm control` (the compositor's control socket:
semantic ops first), `tilo-vm input` (uinput devices on the slot's own seat),
`tilo-vm shot` and `tilo-vm slot exec` (what the slot's session sees). Nothing
here uses raw ssh or copies files into the VM.
"""

from __future__ import annotations

import base64
import json
import os
import shlex
import subprocess
import time
from pathlib import Path

# `actions list` pages: rows asked for per page (the desktop's most), and how often a
# listing that changed under the cursor is read again from its start.
LISTING_PAGE = 2000
LISTING_ATTEMPTS = 3


class VmError(RuntimeError):
    """A tilo-vm command failed; carries its output for the report."""

    def reason(self) -> str:
        """The innermost `error ...` line, without tilo-vm's hints and receipt."""
        lines = [l.strip() for l in str(self).splitlines() if "error" in l]
        return (lines[-1] if lines else str(self).splitlines()[0])[:400]


def first_json(text: str):
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        pass
    lines = text.splitlines()
    for index, line in enumerate(lines):
        if line.lstrip().startswith(("{", "[")):
            # CLI receipts and bracket-prefixed progress lines can surround a
            # complete, pretty-printed JSON document. A log line is not a response.
            try:
                return json.JSONDecoder().raw_decode("\n".join(lines[index:]).lstrip())[0]
            except json.JSONDecodeError:
                # Only a complete bracketed progress line may be skipped. Once a
                # document begins, never mistake one of its nested values for it.
                stripped = line.strip()
                if (stripped.startswith("[") and stripped.endswith("]")
                        and stripped[1:2].isalpha()
                        and not any(char in stripped[1:-1] for char in '[]{}"')):
                    continue
                break
    raise VmError(f"no JSON in output: {text[:400]!r}")


def command_failure(stdout: str, stderr: str) -> str:
    """Prefer the CLI's structured error over build progress on stderr."""
    decoder = json.JSONDecoder()
    for stream in (stdout, stderr):
        offset = 0
        for line in stream.splitlines(keepends=True):
            if line.lstrip().startswith("{"):
                try:
                    value, _ = decoder.raw_decode(stream[offset:].lstrip())
                except json.JSONDecodeError:
                    pass
                else:
                    if isinstance(value, dict) and isinstance(value.get("error"), str):
                        return value["error"].strip()
            offset += len(line)
    return (stderr or stdout).strip()


class TiloVm:
    def __init__(self, binary: str, vm: str, slot: str, log: Path):
        self.binary = binary
        self.vm = vm
        self.slot = slot
        self.log = log

    def run(self, *args: str, timeout: float = 120, check: bool = True,
            slot: bool = True, cwd: Path | None = None) -> str:
        env = dict(os.environ, TILO_VM=self.vm)
        if slot:
            env["TILO_VM_SLOT"] = self.slot
        cmd = [self.binary, *args]
        started = time.monotonic()
        try:
            proc = subprocess.run(cmd, env=env, capture_output=True, text=True,
                                  timeout=timeout, cwd=cwd)
        except subprocess.TimeoutExpired as exc:
            self._log(cmd, None, time.monotonic() - started, "", str(exc))
            raise VmError(f"timed out after {timeout}s: {shlex.join(args)}") from exc
        self._log(cmd, proc.returncode, time.monotonic() - started,
                  proc.stdout, proc.stderr)
        if check and proc.returncode != 0:
            said = command_failure(proc.stdout, proc.stderr)
            if len(said) > 1200:
                said = said[:300] + " […] " + said[-900:]
            raise VmError(f"`tilo-vm {shlex.join(args)[:200]}` exited {proc.returncode}: {said}")
        return proc.stdout

    def _log(self, cmd, code, elapsed, out, err):
        with self.log.open("a") as f:
            f.write(f"$ {shlex.join(cmd)}  # exit={code} {elapsed:.2f}s\n")
            for chunk in (out, err):
                if chunk:
                    # A failure explains itself at the end (a build's error after
                    # its progress lines), so a long output keeps its head and tail.
                    if len(chunk) > 8000:
                        chunk = chunk[:2000] + "\n[…]\n" + chunk[-6000:]
                    f.write(chunk.rstrip() + "\n")

    # -- control socket, semantic half --------------------------------------

    def control_json(self, *args: str, timeout: float = 60):
        return first_json(self.run("control", *args, timeout=timeout))

    def raw(self, op: dict, timeout: float = 60):
        return self.control_json("raw", json.dumps(op), timeout=timeout)

    def state(self) -> dict:
        return self.control_json("state-dump")

    def actions(self, query: str = "", *, programmatic: bool = False) -> list[dict]:
        """Every row the desktop lists for `query`, page after page. A page holds 200 rows
        unless asked for more, and an open app's own actions can push the window closers
        past it: a caller reading one page saw a window with no close action and left it
        open (A15). A listing that changes while paged is read again from its start."""
        for _ in range(LISTING_ATTEMPTS):
            rows, cursor = [], None
            while True:
                args = ["actions", "list", *([query] if query else []),
                        *(["--programmatic"] if programmatic else []), "--json",
                        "--limit", str(LISTING_PAGE)]
                try:
                    page = self.control_json(*args, *(["--cursor", cursor] if cursor else []))
                except VmError as exc:
                    if "stale_cursor" in str(exc):
                        break
                    raise
                rows += page["actions"]
                if not page.get("truncated"):
                    return rows
                cursor = page.get("next_cursor")
                if not cursor:
                    raise VmError("the action listing is truncated but names no next page")
        raise VmError(f"the action listing changed on each of {LISTING_ATTEMPTS} reads while it was paged")

    def invoke(self, provider: str, action_id: str, args: dict | None = None) -> dict:
        cmd = ["actions", "invoke", provider, action_id]
        if args:
            cmd += ["--args-json", json.dumps(args)]
        return self.control_json(*cmd)

    def targets(self) -> list[dict]:
        return self.raw({"op": "list_targets"})["targets"]

    def foreign_frames(self) -> list[dict]:
        return self.raw({"op": "foreign_frames"})["foreign_frames"]["frames"]

    # -- control socket, input half (synthetic) -----------------------------

    def control(self, *args: str) -> str:
        return self.run("control", *args)

    # -- real input on the slot's seat (uinput through libinput) ------------

    def key(self, *combos: str) -> None:
        self.run("input", "key", *combos)

    def input(self, *args: str) -> str:
        return self.run("input", *args)

    # -- pictures and the slot's view of the system -------------------------

    def shot(self, path: Path) -> Path:
        path.parent.mkdir(parents=True, exist_ok=True)
        try:
            self.run("shot", "-o", str(path), timeout=90)
        except VmError:
            # A software-rendered session under load can miss the 15 s snapshot deadline once.
            time.sleep(2)
            self.run("shot", "-o", str(path), timeout=90)
        return path

    def snapshot_once(self, path: Path) -> Path:
        """One `tilo-vm shot`, no retry: for a caller that expects it to be refused."""
        path.parent.mkdir(parents=True, exist_ok=True)
        self.run("shot", "-o", str(path), timeout=90)
        return path

    # -- the slot's session ---------------------------------------------------

    def restart_session(self, timeout: float = 60) -> None:
        """Restart the slot's session on its current generation and wait until its control
        socket answers again: a fresh compositor, unlocked, with no windows open. Counted in
        `requested_restarts`, so the session-restart guard knows this ending was asked for."""
        self.requested_restarts = getattr(self, "requested_restarts", 0) + 1
        self.run("slot", "restart", self.slot, timeout=300, slot=False)

        def answering():
            try:
                return self.state()
            except VmError:
                return None

        if wait_until(answering, timeout, interval=1.0) is None:
            raise VmError(f"the restarted session's control socket did not answer within {timeout:.0f}s")

    def exec(self, script: str, timeout: float = 120, check: bool = True) -> str:
        """Run a POSIX sh script as the slot's user, in its session's view."""
        return self.run("slot", "exec", "--", "sh", "-c", script,
                        timeout=timeout, check=check)

    def root_exec(self, script: str, timeout: float = 120) -> str:
        """Run a POSIX sh script as root in the VM: reads of the compositor's /proc entries
        (it is not dumpable, so slot exec cannot), or a check's setup of this throwaway VM."""
        return self.vm_cmd("ssh", "--", "sh -c " + shlex.quote(script), timeout=timeout)

    def slot_uid(self) -> int:
        if not hasattr(self, "_uid"):
            self._uid = int(self.exec("id -u").split()[0])
        return self._uid

    def write_home_file(self, rel: str, data: bytes) -> None:
        """Write a file under the slot user's home, as that user's apps do."""
        blob = base64.b64encode(data).decode()
        q = shlex.quote(rel)
        self.exec(f'cd "$HOME" && mkdir -p "$(dirname {q})" && '
                  f'echo {blob} | base64 -d > {q}.acc-tmp && mv -f {q}.acc-tmp {q}')

    def vm_cmd(self, *args: str, timeout: float = 900, check: bool = True) -> str:
        return self.run("vm", *args, timeout=timeout, check=check, slot=False)


def wait_until(predicate, timeout: float, interval: float = 0.5):
    """Poll `predicate` until it is truthy and return its value, else None."""
    deadline = time.monotonic() + timeout
    while True:
        value = predicate()
        if value:
            return value
        if time.monotonic() >= deadline:
            return None
        time.sleep(interval)

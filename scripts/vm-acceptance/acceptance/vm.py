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


class VmError(RuntimeError):
    """A tilo-vm command failed; carries its output for the report."""

    def reason(self) -> str:
        """The innermost `error ...` line, without tilo-vm's hints and receipt."""
        lines = [l.strip() for l in str(self).splitlines() if "error" in l]
        return (lines[-1] if lines else str(self).splitlines()[0])[:400]


def first_json(text: str):
    for line in text.splitlines():
        line = line.strip()
        if line.startswith(("{", "[")):
            return json.loads(line)
    raise VmError(f"no JSON in output: {text[:400]!r}")


class TiloVm:
    def __init__(self, binary: str, vm: str, slot: str, log: Path):
        self.binary = binary
        self.vm = vm
        self.slot = slot
        self.log = log

    def run(self, *args: str, timeout: float = 120, check: bool = True,
            slot: bool = True) -> str:
        env = dict(os.environ, TILO_VM=self.vm)
        if slot:
            env["TILO_VM_SLOT"] = self.slot
        cmd = [self.binary, *args]
        started = time.monotonic()
        try:
            proc = subprocess.run(cmd, env=env, capture_output=True, text=True,
                                  timeout=timeout)
        except subprocess.TimeoutExpired as exc:
            self._log(cmd, None, time.monotonic() - started, "", str(exc))
            raise VmError(f"timed out after {timeout}s: {shlex.join(args)}") from exc
        self._log(cmd, proc.returncode, time.monotonic() - started,
                  proc.stdout, proc.stderr)
        if check and proc.returncode != 0:
            raise VmError(f"`tilo-vm {shlex.join(args)[:200]}` exited {proc.returncode}: "
                          f"{(proc.stderr or proc.stdout).strip()[:600]}")
        return proc.stdout

    def _log(self, cmd, code, elapsed, out, err):
        with self.log.open("a") as f:
            f.write(f"$ {shlex.join(cmd)}  # exit={code} {elapsed:.2f}s\n")
            for chunk in (out, err):
                if chunk:
                    f.write(chunk[:4000].rstrip() + "\n")

    # -- control socket, semantic half --------------------------------------

    def control_json(self, *args: str, timeout: float = 60):
        return first_json(self.run("control", *args, timeout=timeout))

    def raw(self, op: dict, timeout: float = 60):
        return self.control_json("raw", json.dumps(op), timeout=timeout)

    def state(self) -> dict:
        return self.control_json("state-dump")

    def actions(self, query: str = "") -> list[dict]:
        args = ["actions", "list", *([query] if query else []), "--json"]
        return self.control_json(*args)["actions"]

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

    def exec(self, script: str, timeout: float = 120, check: bool = True) -> str:
        """Run a POSIX sh script as the slot's user, in its session's view."""
        return self.run("slot", "exec", "--", "sh", "-c", script,
                        timeout=timeout, check=check)

    def root_exec(self, script: str, timeout: float = 120) -> str:
        """Run a read-only POSIX sh script as root in the VM. The compositor is not dumpable
        and /proc is hidepid=invisible, so only root sees it; slot exec cannot."""
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

"""The suite's only way into the VM: the `tilo-vm` CLI.

Everything goes through `tilo-vm control` (the compositor's control socket:
semantic ops first), `tilo-vm input` (uinput devices on the account's own seat),
`tilo-vm shot` and `tilo-vm account exec` (what the account's session sees). Nothing
here uses raw ssh or copies files into the VM.
"""

from __future__ import annotations

import base64
import json
import os
import shlex
import subprocess
import sys
import time
from pathlib import Path

# The action model's frames, built as the headless harness builds them.
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "headless-examples"))
import real_apps  # noqa: E402

# How often a listing that changed under its cursor is read again from its start.
LISTING_ATTEMPTS = 6
# How the checks name the workspace's open of an app, and an app's own action, as providers.
APPLICATIONS = "applications"
APP_ACTIONS = "app-actions"
# Between an action's resource and its name in the ids the checks read.
ID_SEP = "\x1f"
# Base64 characters per write_home_file command: a multiple of 4, so each piece decodes alone.
WRITE_CHUNK = 64 * 1024


# What the tool says of a VM or an account another of its commands still holds, and how long
# each further try waits first: a little over seven seconds in all.
BUSY = "is busy with another tilo-vm command"
BUSY_WAITS = (0.5, 1.0, 2.0, 4.0)


class VmError(RuntimeError):
    """A tilo-vm command failed; carries its output for the report."""

    def reason(self) -> str:
        """The innermost `error ...` line, without tilo-vm's hints and receipt."""
        lines = [l.strip() for l in str(self).splitlines() if "error" in l]
        return (lines[-1] if lines else str(self).splitlines()[0])[:400]


def row_of(row: dict) -> dict | None:
    """A catalogue row as the checks read it, or None for a row of a resource alone. `provider`
    and `id` name it: the workspace's open of an app is `applications` and the app's id, an
    app's own action `app-actions` and `provider␟resource␟action`, any other `resource␟action`
    of its provider. Beside them `application`, `resource` (the row's), `operation`, `title`,
    `subtitle`, `params`, `contract`, `semantic_identity` and `target`, what it invokes."""
    action = row.get("action")
    if not action:
        return None
    at = action["address"]
    resource = row.get("resource") or {}
    provider = row["provider"]
    described = {
        "provider": provider, "id": f"{at['resource']}{ID_SEP}{at['action']}", "kind": "actions",
        "operation_kind": action.get("kind"), "title": action.get("action_label"),
        "subtitle": resource.get("label"), "params": action.get("params") or [],
        "contract": action.get("contract"), "application": row.get("app_id"), "record": "",
        "resource": resource.get("id"), "parent_resource": row.get("parent_resource"),
        "semantic_identity": action.get("semantic_identity"), "operation": at["action"],
        "target": {"provider": provider, "address": at},
    }
    if (at["action"] == "open" and at["resource"].startswith("app.")
            and provider.startswith("workspace:")):
        described.update(provider=APPLICATIONS, id=row.get("app_id"), kind="apps",
                         title=resource.get("label"))
    elif provider.startswith(("application:", "launch:")):
        described.update(provider=APP_ACTIONS,
                         id=f"{provider}{ID_SEP}{at['resource']}{ID_SEP}{at['action']}")
    return described


def listed(ask, query: str = "") -> list[dict]:
    """Every action row the session lists for `query`, read as `row_of` reads it. `ask` sends
    one action frame to the session's control socket and answers the frame that came back. A
    listing that changed while it was paged is read again from its start."""
    for _ in range(LISTING_ATTEMPTS):
        try:
            return [described for row in real_apps.catalogue(ask, query)
                    if (described := row_of(row)) is not None]
        except real_apps.Refused as exc:
            if exc.code != "stale_revision":
                raise VmError(f"the action listing was refused: {exc}") from exc
    raise VmError(f"the action listing changed on each of {LISTING_ATTEMPTS} reads while it was paged")


def update_of(invocation: str, status) -> dict:
    """An invocation's receipt status as the update the checks read: `state`, and the `output`
    or `error` it ended with."""
    if status == "Accepted":
        return {"state": "accepted", "invocation": invocation}
    (name, body), = status.items()
    state = {"Completed": "succeeded", "Failed": "failed", "Cancelled": "cancelled"}[name]
    return {"state": state, "invocation": invocation, **(body or {})}


def invoked(ask, provider: str, action_id: str, args: dict | None = None, *,
            confirmed: bool = False, rows=None) -> dict:
    """Invoke the action the checks name `provider` and `action_id` (see `row_of`), answered
    once it is admitted: `{"receipt": {"invocation", "update"}}`. A refusal raises, with its
    code. `rows` lists what the session offers, for the open of an app by its id."""
    if provider == APPLICATIONS:
        found = next((row for row in (rows() if rows else listed(ask, action_id))
                      if row["provider"] == APPLICATIONS and row["id"] == action_id), None)
        if found is None:
            raise VmError(f"unknown_action: the session offers no app {action_id}")
        target = found["target"]
        provider, (resource, action) = target["provider"], (target["address"]["resource"],
                                                             target["address"]["action"])
    elif provider == APP_ACTIONS:
        provider, resource, action = action_id.split(ID_SEP, 2)
    else:
        resource, _, action = action_id.rpartition(ID_SEP)
    frame = real_apps.invocation(provider, resource, action, args, confirmed=confirmed)
    try:
        status = real_apps.receipt(ask(frame))
    except real_apps.Refused as exc:
        raise VmError(f"{exc.code}: {provider} {resource}#{action} was refused: {exc.message}") from exc
    invocation = frame["message"]["Invoke"]["invocation"]
    return {"receipt": {"invocation": invocation, "update": update_of(invocation, status)}}


def observed(ask, invocation: str, wait_ms: int = 0) -> dict:
    """Where `invocation` stands now, as `update_of` reads it; `wait_ms` holds the answer until
    it moves on."""
    message = {"correlation": 1, "invocation": invocation}
    if wait_ms:
        message["wait_ms"] = wait_ms
    answer = ask(real_apps.frame({"Observe": message}))["message"]["Update"]
    if answer.get("state") is None:
        raise VmError(f"unknown_invocation: the session follows no invocation {invocation}")
    return answer["state"]["update"]


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
    def __init__(self, binary: str, vm: str, account: str, log: Path):
        self.binary = binary
        self.vm = vm
        self.account = account
        self.log = log
        # Why the account session could not be restarted, once that happened (`restart_session`).
        self.restart_failed: str | None = None

    def run(self, *args: str, timeout: float = 120, check: bool = True,
            account: bool = True, cwd: Path | None = None) -> str:
        env = dict(os.environ, TILO_VM=self.vm)
        if account:
            env["TILO_VM_ACCOUNT"] = self.account
        cmd = [self.binary, *args]
        # The tool holds a VM or an account for the length of one command and refuses
        # the next while it still does, saying to wait and retry: so a refusal
        # of that kind is waited out a few times before it is the answer.
        for wait in (*BUSY_WAITS, None):
            started = time.monotonic()
            try:
                # Account output may be cut at a byte count (R09 reads the session
                # log through `cut -c1-400`), splitting a multi-byte character;
                # one bad byte must not lose the whole check.
                proc = subprocess.run(cmd, env=env, capture_output=True, text=True,
                                      errors="replace", timeout=timeout, cwd=cwd)
            except subprocess.TimeoutExpired as exc:
                self._log(cmd, None, time.monotonic() - started, "", str(exc))
                raise VmError(f"timed out after {timeout}s: {shlex.join(args)}") from exc
            except OSError as exc:
                self._log(cmd, None, time.monotonic() - started, "", str(exc))
                raise VmError(f"could not run {shlex.join(cmd)}: {exc}") from exc
            self._log(cmd, proc.returncode, time.monotonic() - started,
                      proc.stdout, proc.stderr)
            if proc.returncode == 0 or wait is None or BUSY not in proc.stdout + proc.stderr:
                break
            time.sleep(wait)
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
                    # Successful build progress keeps its head and tail. Failed
                    # builds need the entire stream: a long exception command
                    # can push the underlying error out of both retained ends.
                    if code == 0 and len(chunk) > 8000:
                        chunk = chunk[:2000] + "\n[…]\n" + chunk[-6000:]
                    f.write(chunk.rstrip() + "\n")

    # -- control socket, semantic half --------------------------------------

    def control_json(self, *args: str, timeout: float = 60):
        return first_json(self.run("control", *args, timeout=timeout))

    def raw(self, op: dict, timeout: float = 60):
        return self.control_json("raw", json.dumps(op), timeout=timeout)

    def state(self) -> dict:
        return self.control_json("state-dump")

    def search(self) -> dict:
        """What the shell's search shows now: `query`, `rows` (each its title, subtitle, kind
        and facts) and whether it still waits for sources (`searching`)."""
        return self.raw({"op": "search_rows"})["answer"]

    def actions(self, query: str = "", *, programmatic: bool = False) -> list[dict]:
        """Every action row the desktop lists for `query`, page after page (see `row_of`): an
        open app's own actions can push the window closers past one page, and a caller reading
        one page saw a window with no close action and left it open (A15). The listing is the
        one agents read, so `programmatic` rows are always in it."""
        del programmatic
        return listed(self.raw, query)

    def invoke(self, provider: str, action_id: str, args: dict | None = None, *,
               confirmed: bool = False) -> dict:
        """Invoke an action the listing names (see `invoked`)."""
        return invoked(self.raw, provider, action_id, args, confirmed=confirmed)

    def observe(self, invocation: str, wait_ms: int = 0) -> dict:
        """Where an invocation this desktop admitted stands (see `observed`)."""
        return observed(self.raw, invocation, wait_ms)

    def targets(self) -> list[dict]:
        return self.raw({"op": "list_targets"})["targets"]

    def windows(self) -> list[dict]:
        """The shell's open windows: each with `window` (the mapped window's id), `app_id`, `rect`
        (`[x, y, width, height]`, null while no window of the tab is mapped) and `frames`, how many
        frames that window's client committed."""
        return self.raw({"op": "windows"})["answer"]["windows"]

    # -- control socket, input half (synthetic) -----------------------------

    def control(self, *args: str) -> str:
        return self.run("control", *args)

    # -- real input on the account's seat (uinput through libinput) ------------

    def key(self, *combos: str) -> None:
        self.run("input", "key", *combos)

    def input(self, *args: str) -> str:
        return self.run("input", *args)

    # -- pictures and the account's view of the system -------------------------

    def shot(self, path: Path) -> Path:
        path.parent.mkdir(parents=True, exist_ok=True)
        try:
            self.run("shot", "-o", str(path), timeout=90)
        except VmError:
            # A software-rendered session under load can miss the 15 s snapshot deadline once:
            # take it again once the compositor answers its control socket.
            self.wait_answering(30)
            self.run("shot", "-o", str(path), timeout=90)
        return path

    def snapshot_once(self, path: Path) -> Path:
        """One `tilo-vm shot`, no retry: for a caller that expects it to be refused."""
        path.parent.mkdir(parents=True, exist_ok=True)
        self.run("shot", "-o", str(path), timeout=90)
        return path

    # -- the account's session ---------------------------------------------------

    def restart_session(self, timeout: float = 60) -> None:
        """Restart the account's session on its current generation and wait until its control
        socket answers again: a fresh compositor, unlocked, with no windows open. Counted in
        `requested_restarts`, so the session-restart guard knows this ending was asked for."""
        if getattr(self, "restart_failed", None):
            # A session that one restart could not bring back gets no second one: each would
            # wait out its bounds again, and every check after it with them.
            raise VmError(f"not restarted again: {self.restart_failed}")
        self.requested_restarts = getattr(self, "requested_restarts", 0) + 1
        try:
            self.run("account", "restart", self.account, timeout=300, account=False)
            self.wait_answering(timeout)
        except VmError as exc:
            self.restart_failed = f"restarting the account session failed: {exc.reason()}"
            raise

    def wait_answering(self, timeout: float = 60) -> dict:
        """The state dump, once the account's compositor answers its control socket with at
        least one display."""
        def answering():
            try:
                st = self.state()
            except VmError:
                return None
            outputs = (st.get("workspace_tree") or {}).get("outputs") or {}
            return st if outputs.get("displays") else None

        st = wait_until(answering, timeout, interval=0.5)
        if st is None:
            raise VmError(f"the session's control socket did not answer with a display within {timeout:.0f}s")
        return st

    def wait_catalogue(self, timeout: float = 60) -> None:
        """Once the session answers, until its app catalogue lists Terminal, which every
        image carries: a fresh session publishes the catalogue a few seconds after it
        starts."""
        self.wait_answering(timeout)

        def listed():
            try:
                return any(a["provider"] == "applications" and a["id"] == "so.tilo.terminal"
                           for a in self.actions("Terminal"))
            except VmError:
                return False

        if wait_until(listed, timeout, interval=0.5) is None:
            raise VmError(f"the session listed no applications within {timeout:.0f}s")

    def exec(self, script: str, timeout: float = 120, check: bool = True) -> str:
        """Run a POSIX sh script as the account's user, in its session's view."""
        return self.run("account", "exec", "--", "sh", "-c", script,
                        timeout=timeout, check=check)

    def root_exec(self, script: str, timeout: float = 120, check: bool = True) -> str:
        """Run a POSIX sh script as root in the VM: reads of the compositor's /proc entries
        (it is not dumpable, so account exec cannot), or a check's setup of this throwaway VM."""
        return self.vm_cmd("ssh", "--", "sh -c " + shlex.quote(script), timeout=timeout, check=check)

    def account_uid(self) -> int:
        if not hasattr(self, "_uid"):
            self._uid = int(self.exec("id -u").split()[0])
        return self._uid

    def write_home_file(self, rel: str, data: bytes) -> None:
        """Write a file under the account user's home, as that user's apps do."""
        blob = base64.b64encode(data).decode()
        q = shlex.quote(rel)
        # The script travels as one argument, which the kernel caps at 128 KiB: a larger
        # file (the AT-SPI client) goes over in pieces.
        chunks = [blob[i:i + WRITE_CHUNK] for i in range(0, len(blob), WRITE_CHUNK)] or [""]
        for n, chunk in enumerate(chunks):
            first, last = n == 0, n == len(chunks) - 1
            self.exec(f'cd "$HOME" && ' + (f'mkdir -p "$(dirname {q})" && ' if first else '')
                      + f'echo {chunk} | base64 -d {">" if first else ">>"} {q}.acc-tmp'
                      + (f' && mv -f {q}.acc-tmp {q}' if last else ''))

    def vm_cmd(self, *args: str, timeout: float = 900, check: bool = True) -> str:
        return self.run("vm", *args, timeout=timeout, check=check, account=False)


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

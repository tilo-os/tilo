#!/usr/bin/env python3
"""An agent on one of the person's Tilo boxes steers another of their boxes over MCP.

    scripts/e2e-remote/run.py --work DIR --out DIR [--hold SECONDS]

What a person does, on real booted boxes (tools/ci/e2e-remote.sh makes the host):

1. Box A is created from this tree's dev disk and its person made the way a device's first
   login makes one (`tilo-vm slot create --identity device`). A is then branched into box B
   (`tilo-vm vm branch`, which boots B once beside A to have it taken in), which comes up as
   a new device of the same person. Box C is another person's. The boxes meet only on the
   network tilo-vm gives the VMs of one host (`tilolan0`, 198.18.0.0/16).
2. On A, an agent adds A's MCP server carried to B (`tilo-runtime --connect --device B`);
   the scripted client (guest/agent.py, clientInfo `reviewer`) runs exactly that command.
3. A's own server lists B online (`devices#list`).
4. With B's "Accept agents from my other devices" off, the agent's calls are refused on B,
   saying which setting would let it in, with nobody asked and nothing registered there. With
   it on, the agent works on B; B's person's Stop refuses its calls from then on, saying so.
5. With the setting on (turned on through its own action), the agent is let in on B without a
   question: it lists B's apps, opens Notes and makes a note there, registered in B's shell with
   A's device beside its name (its whole key and its label as the server caps it) and with no
   environment. Its step `terminal#run`, which asks first, is still put to B's person on the
   run's sheet, who allows it once, and the command then runs.
6. With B's second setting "Let them act without asking" on as well, a new connection of the
   agent (a session of its own, under another key) does the same work, `terminal#run` and all,
   with no sheet ever up on B. That the step is asked with the second setting off is step 5,
   in the same run, just before.
7. Two agents on two machines work in one Browser tab on B at once: B's person opens the
   Browser from the shell's apps, the agent on A, through B's server, sends its tab to a form B
   serves on its loopback, and an agent on B itself joins it. Each answers its part of the
   form with `fill-page-form` in the same moment; B's second setting is off again, and B's
   person allows each agent on B's sheet.
   B's person then reads, through the desktop's own actions, who set each field
   (`read-page-fields`) and what was done in the tab, in order (`read-page-record`): both
   agents, each under its own name.
8. Three machines work in that one tab: A is branched once more into D, a third device of the
   person, and the agents on A and on D, both through B's server, and the agent on B each answer
   a part of the form in the same moment. B's person reads three names in the tab's record and
   which of them set each field. The agent on D names B by its device id, which a server takes
   as well as a name: D does not list B (step 10).
9. In B's tab, each agent that came from another machine is recorded with its device beside
   its name: the device B's server verified it came from, by its whole key and its name, in the
   tab's record and in who set each field. B's own agent is recorded with none. B's shell registers
   each of them the same way: an agent from another machine with that machine's key and label,
   B's own with neither, and none of them with an environment.
10. The person's third device lists the second among their devices (`devices#list`), as the
    first does, so that `--device <name>` can name it there.
11. A person at another machine watches that tab: B's person shares the Browser's window with
    the workspace, naming its tab, and A's shell holds the window's picture in a tab of its own,
    as a window that runs on B, whatever it lists as its own view of that window. A picture of
    each screen is kept.
12. The same the other way round: A, the machine that hosts the workspace, opens a Browser of
    its own on the form, served on A's loopback as B serves it, and A's person shares that
    window. B's shell holds it as a window that runs on A and shows the form's text.
13. C, another person's device, is refused at B's server.

Every step is a result in report.json / report.md under --out; any failure fails the run.
"""

from __future__ import annotations

import argparse
import os
import base64
import json
import re
import shlex
import sys
import threading
import time
import traceback
import unicodedata
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "vm-acceptance"))

from acceptance.vm import (Invoked, TiloVm, VmError, address, completed, first_json,  # noqa: E402
                           wait_until)

GUEST_AGENT = HERE / "guest" / "agent.py"
GUEST_DIR = ".cache/e2e-remote"
MARK = "E2E-MCP "
SLOT = "me"
# What the boxes' sessions log of the MCP server, its remote side and the questions it asks.
SERVER_LOG = ("info,tilo_session::mcp_server=debug,tilo_session::device_agents=debug,"
            "tilo_session::questions=debug,tilo_session::agent_sessions=debug")
# Each box: guest memory and CPUs. Three fit beside each other in the micro-VM.
# A, B and D run side by side; C runs beside B once A and D have stopped. A box takes 3 GiB: with less its
# first login's onboarding does not come up (1.6 GiB boxes never showed it). A micro-VM t booked
# too small for two runs out of memory, and t runs it again with twice the room.
MEMORY_MB = 3072
CPUS = 2


def _host() -> tuple[int, int]:
    total = next(int(l.split()[1]) // 1024 for l in Path("/proc/meminfo").read_text().splitlines()
                 if l.startswith("MemTotal:"))
    return total, os.cpu_count() or 2


_TOTAL_MB, _NPROC = _host() if Path("/proc/meminfo").exists() else (0, 0)
OUTPUT = "1280x800"
# How long a step that waits on the boxes finding each other may take.
FIND_S = 180
# The MCP server sheet's choice and button, as the person reads them (en-US).
# The name a shell shows the scripted client by, then its number: its clientInfo name,
# `reviewer` (r5d40ebffe7 on B: "reviewer #3").
CLIENT_SHOWN = "reviewer"
ALLOW_ONCE = "Allow once"
ALLOW = "Allow"
DENY = "Deny"
# The choice only the server's own sheet offers: a sheet with "Allow once" and without it asks
# about one step of an agent that is in already (crates/session/session/src/step_questions.rs).
ALLOW_SESSION = "Allow for this session"
# The Browser, as the shell names the app.
BROWSER = "so.tilo.browser"
# How long the person is not there to answer B's sheet.
# The sheet's choice that lets an agent go on unasked while it works here, as its label starts.
ALWAYS_ALLOW = "Always allow"
# The form box B serves on its own loopback for the agents to answer, and where.
SITE_PORT = 8765
SITE_URL = f"http://127.0.0.1:{SITE_PORT}/"
SITE = """<!doctype html><title>Apply</title><h1>Apply for a visitor visa</h1>
<form action="/sent" method="get">
<label>Given names <input name="given"></label>
<label>Family name <input name="family"></label>
<label>Nationality <select name="nationality" id="nat"><option value="">Select a country</option>
<option>Kenya</option><option>Other</option></select></label>
<div id="other" hidden><label>Which country? <input name="other" required></label></div>
<label>Passport number <input name="passport"></label>
<fieldset><legend>Refused before?</legend>
<label><input type="radio" name="refused" value="yes"> Yes</label>
<label><input type="radio" name="refused" value="no"> No</label></fieldset>
<label>Arrival <input type="date" name="arrival"></label>
<button>Send application</button></form>
<script>document.getElementById('nat').addEventListener('change', e =>
document.getElementById('other').hidden = e.target.value !== 'Other');</script>
"""
# What the form shows whoever looks at it, on the machine that runs it and on one that watches
# its window: its heading and three of its labels.
SITE_TEXT = ("Apply for a visitor visa", "Given names", "Family name", "Passport number")
# What each of the two agents answers: the one from A the person, the one on B the journey.
ANSWERS_A = {"Given names": "Maria Elena", "Family name": "Okonkwo-Reyes",
             "Nationality": "Other", "Which country?": "Portugal"}
ANSWERS_B = {"Passport number": "P7K290XB4", "Refused before?": "No", "Arrival": "2027-02-03"}
# The same form answered anew by three: the agents on A and on D, and the one on B.
THREE_URL = SITE_URL + "?three"
THREE_A = {"Given names": "Joao Pedro", "Family name": "Almeida-Santos"}
THREE_D = {"Nationality": "Other", "Which country?": "Brazil"}
THREE_B = {"Passport number": "FX4410972", "Refused before?": "No", "Arrival": "2027-05-19"}


def capped(label: str) -> str:
    """A device's label as B's server says it beside an agent that came from that device: trimmed,
    without control characters, its first 64 characters (tilo_actions::InvokerDevice::certified).
    Empty for a device whose certificate names none."""
    return "".join(c for c in (label or "").strip() if unicodedata.category(c) != "Cc")[:64]


class Failed(Exception):
    """A step saw what it checks, and it is wrong; the steps that need it do not run."""


@dataclass
class Result:
    id: str
    title: str
    status: str = "fail"
    detail: str = ""
    facts: dict = field(default_factory=dict)
    seconds: float = 0.0


@dataclass
class Box:
    letter: str
    vm: TiloVm
    ip: str = ""
    identity: dict = field(default_factory=dict)

    @property
    def name(self) -> str:
        return self.vm.vm


@dataclass
class Party:
    """An agent working in the tab: the box it runs on, the server it comes through, what it
    answers, a value only it writes, the questions reviewed as set by it, whether it is the
    one that sends the tab to the form, and whether it comes from another device."""
    box: Box
    command: list[str]
    answers: dict
    mark: str
    reviewed: tuple
    leads: bool = False
    remote: bool = True


class Run:
    def __init__(self, args):
        self.args = args
        self.out: Path = args.out
        self.out.mkdir(parents=True, exist_ok=True)
        self.results: list[Result] = []
        # The agents from other machines as B's tab named them, with their devices.
        self.tab_actors: list[dict] = []
        # B's shell while the agents worked in its tab: each agent's machine and what was registered.
        self.tab_sessions: list[dict] = []
        # Whether the third device D was made, for the step that asks what it lists.
        self.third_made = False
        self.log = self.out / "tilo-vm.log"
        self.running: set[str] = set()
        # What E14 found of A's tab for B's window while the window was not shared, by when it
        # looked: empty for a tab that said where the window runs, else what was wrong (E17).
        self.tab_stands: dict[str, str] = {}
        self.boxes = {
            letter: Box(letter, TiloVm(args.tilo_vm, f"e2e-{letter}", SLOT, self.log))
            for letter in "abcd"
        }

    # -- bookkeeping ---------------------------------------------------------

    def step(self, rid: str, title: str, fn, *needs: str) -> bool:
        missing = [n for n in needs if not self.passed(n)]
        res = Result(rid, title)
        if missing:
            res.status, res.detail = "not-run", f"needs {', '.join(missing)}"
            self.results.append(res)
            print(f"{rid} NOT RUN  {title}: {res.detail}", flush=True)
            return False
        started = time.monotonic()
        # No step meets the idle dim or lock it is not about.
        self.keep_awake(*(box for box in self.boxes.values() if box.letter in self.running))
        try:
            fn(res)
            res.status = "pass"
        except (Failed, VmError) as exc:
            res.detail = str(exc)[:4000]
        except Exception as exc:  # noqa: BLE001 - a broken step is a failed step, with its trace
            res.detail = f"{type(exc).__name__}: {exc}\n{traceback.format_exc()[-2000:]}"
        res.seconds = round(time.monotonic() - started, 1)
        self.results.append(res)
        print(f"{rid} {res.status.upper():4} {res.seconds:6.1f}s  {title}"
              + (f"\n      {res.detail[:1500]}" if res.detail else ""), flush=True)
        # What the step saw, in short: the log is the record a failed run keeps.
        for key, value in res.facts.items():
            text = value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)
            print(f"      {key}: {text[:700]}", flush=True)
        return res.status == "pass"

    def passed(self, rid: str) -> bool:
        return any(r.id == rid and r.status == "pass" for r in self.results)

    def write(self, name: str, data) -> None:
        path = self.out / name
        path.write_text(data if isinstance(data, str) else json.dumps(data, indent=1, ensure_ascii=False))

    def report(self) -> bool:
        ok = bool(self.results) and all(r.status == "pass" for r in self.results)
        doc = {"overall": "pass" if ok else "fail", "commit": self.args.commit,
               "results": [r.__dict__ for r in self.results]}
        self.write("report.json", doc)
        lines = [f"# e2e-remote: {doc['overall'].upper()}", "",
                 f"Commit {self.args.commit}", "", "| step | result | seconds | what |",
                 "|---|---|---|---|"]
        for r in self.results:
            lines.append(f"| {r.id} | {r.status} | {r.seconds} | {r.title} |")
        for r in self.results:
            if r.detail:
                lines += ["", f"## {r.id} {r.status}", "", "```", r.detail, "```"]
        self.write("report.md", "\n".join(lines) + "\n")
        return ok

    # -- the boxes -------------------------------------------------------------

    def virsh(self, *args: str, check: bool = True) -> str:
        import subprocess
        proc = subprocess.run(["virsh", "--connect", "qemu:///session", *args],
                              capture_output=True, text=True, timeout=120)
        if check and proc.returncode != 0:
            raise Failed(f"virsh {' '.join(args)}: {proc.stderr.strip() or proc.stdout.strip()}")
        return proc.stdout

    def lan(self, box: Box) -> str:
        """`box` 's address on the network tilo-vm gives the VMs of one host: its `tilolan0`,
        198.18.<MAC>/16, set at every boot. The boxes reach each other there and nowhere else."""
        def read() -> str:
            return box.vm.root_exec("ip -o -4 addr show dev tilolan0", timeout=60, check=False)

        def addressed() -> str | None:
            said = read()
            return said if " 198.18." in said else None

        said = wait_until(addressed, 90, interval=3.0) or read()
        found = re.search(r"inet (198\.18\.\d+\.\d+)/", said)
        if not found:
            raise Failed(f"{box.name} has no address on tilolan0: {said.strip()[-400:]!r}")
        box.ip = found.group(1)
        return said.strip()[-300:]

    def identity(self, box: Box) -> dict:
        out = box.vm.exec("tilo-identity --json status", timeout=60)
        status = first_json(out)
        box.identity = status
        return status

    def wait_session(self, box: Box, timeout: float = 300) -> None:
        """The box's session as it came up with the box: its shell, with nothing restarted to
        get there, as a person who turns the box on finds it."""
        self.ready(box, timeout)

    def ready(self, box: Box, timeout: float = 120) -> None:
        """Once `box`'s session answers its control socket with its shell drawn and unlocked,
        and no first-use setup over it."""
        def answering():
            try:
                dump = box.vm.state()
                names = [t.get("name") for t in box.vm.targets()]
            except (VmError, ValueError):
                return False
            state = dump.get("state") or dump
            chrome = state.get("chrome") or {}
            return ("workspace_tree" in state and not chrome.get("session_locked")
                    and any(str(n).startswith("DesktopChrome") for n in names)
                    and not any(str(n).startswith("OnboardingView") for n in names))

        if not wait_until(answering, timeout, 1.0):
            self.what_shows(box, "not ready")
            raise Failed(f"{box.name}'s session did not show its shell within {timeout:.0f}s")

    def keep_awake(self, *boxes: Box) -> None:
        for box in boxes:
            try:
                box.vm.key("shift")
            except VmError:
                pass

    def keep_logs(self, box: Box) -> None:
        """What `box`'s session said, for a run that failed: its MCP server, its endpoint and
        its identity, without the per-frame statistics; what it shows and what it lists."""
        for name, fetch in (("targets", lambda: box.vm.targets()), ("state", lambda: box.vm.state()),
                            ("identity", lambda: box.vm.exec("tilo-identity --json status", timeout=60,
                                                             check=False))):
            try:
                self.write(f"{name}-{box.letter}.json", fetch())
            except (VmError, ValueError) as exc:
                self.write(f"{name}-{box.letter}.json", {"error": str(exc)})
        try:
            self.write(f"session-log-{box.letter}.txt", box.vm.exec(
                'tail -n 4000 "$HOME/.local/state/tilo/session-logs/latest.log" 2>&1', timeout=60,
                check=False))
        except VmError as exc:
            self.write(f"session-log-{box.letter}.txt", str(exc))
        try:
            box.vm.shot(self.out / f"shot-{box.letter}.png")
        except VmError:
            pass
        try:
            text = box.vm.run("logs", SLOT, "-n", "20000", timeout=120, check=False, slot=False)
        except VmError as exc:
            text = f"tilo-vm logs failed: {exc}"
        noise = ("FRAME-LIFECYCLE", "paint-stats:", "HEAP-MILESTONE", "DEVICE-MEMORY", "frames=")
        kept = [l for l in text.splitlines() if not any(n in l for n in noise)]
        self.write(f"logs-{box.letter}.txt", "\n".join(kept))
        # A failed run keeps no outputs: what matters of it goes to the log.
        telling = ("MCP server", "mcp_server", "device_agents", "questions", "lan:", "onboarding",
                   "identity", "personal", "ERROR", "WARN", "panicked", "refused", "branch")
        excerpt = ([l for l in kept if any(t in l for t in telling)][-200:]
                   + ["  (its last lines)"] + kept[-40:])
        print(f"--- {box.name}: {len(excerpt)} lines of its session's log ---", flush=True)
        for line in excerpt:
            print("  " + line[:400], flush=True)
        try:
            journal = box.vm.root_exec("journalctl -b --no-pager -o short-monotonic "
                                       "-u tilo-device-branch -u tilo-device-key-init -u NetworkManager "
                                       "| tail -n 400", timeout=120, check=False)
        except VmError as exc:
            journal = f"journalctl failed: {exc}"
        self.write(f"journal-{box.letter}.txt", journal)
        print(f"--- {box.name}: its boot's identity and network units ---", flush=True)
        for line in journal.splitlines()[-60:]:
            print("  " + line[:400], flush=True)

    # -- the agent ---------------------------------------------------------------

    def put_agent(self, box: Box) -> None:
        box.vm.write_home_file(f"{GUEST_DIR}/agent.py", GUEST_AGENT.read_bytes())

    def agent(self, box: Box, scenario: str, command: list[str], timeout: float = 600,
              **env: str) -> dict:
        """Run the scripted agent on `box` and read its transcript."""
        envs = {"E2E_CMD": base64.b64encode(json.dumps(command).encode()).decode(), **env}
        line = " ".join(f"{k}={shlex.quote(v)}" for k, v in envs.items())
        out = self.exec_for(box, f'{line} python3 "$HOME/{GUEST_DIR}/agent.py" {scenario}', timeout)
        for text in reversed(out.splitlines()):
            if text.startswith(MARK):
                transcript = json.loads(text[len(MARK):])
                self.write(f"agent-{box.letter}-{scenario}-{int(time.time())}.json", transcript)
                if transcript.get("client_error"):
                    raise Failed(f"the scripted agent failed: {transcript['client_error']}")
                return transcript
        raise Failed(f"the scripted agent printed no transcript: {out[-600:]!r}")

    @staticmethod
    def exec_for(box: Box, script: str, seconds: float, check: bool = True) -> str:
        """Run `script` in `box` as its person for up to `seconds`: a command there is stopped
        after a minute unless it is given longer."""
        return box.vm.run("slot", "exec", "--timeout", str(int(seconds)), "--", "sh", "-c", script,
                          timeout=seconds + 30, check=check)

    def agent_beside(self, box: Box, scenario: str, command: list[str], timeout: float = 600,
                     **env: str):
        """Run the agent on `box` in a thread; the box keeps what it said in `box_`."""
        holder: dict = {}

        def run():
            try:
                holder["transcript"] = self.agent(box, scenario, command, timeout, **env)
            except (Failed, VmError) as exc:
                holder["error"] = str(exc)

        thread = threading.Thread(target=run, daemon=True)
        thread.start()
        return thread, holder

    @staticmethod
    def finish(thread, holder: dict, timeout: float = 600) -> dict:
        thread.join(timeout)
        if thread.is_alive():
            raise Failed("the scripted agent did not finish")
        if "error" in holder:
            raise Failed(holder["error"])
        return holder["transcript"]

    # -- B's shell ------------------------------------------------------------------

    def agent_sessions(self, box: Box) -> list[dict]:
        dump = box.vm.state()
        return (dump.get("state") or dump).get("agents") or []

    def rows(self, box: Box, query: str = "") -> list[dict]:
        return box.vm.actions(query)

    def find_row(self, box: Box, action: str, query: str = "") -> dict | None:
        for row in self.rows(box, query):
            if address(row)[1] == action:
                return row
        return None

    def targets(self, box: Box) -> list[dict]:
        return box.vm.targets()

    def answer_sheet(self, box: Box, res: Result) -> None:
        """The person at `box` allows the agent once, as their clicks would: the sheet's
        "Allow once" choice, then its "Allow" button (crates/session/session/src/questions.rs,
        `admitted`)."""
        targets = self.targets(box)
        self.write(f"targets-{box.letter}-{int(time.time())}.json", targets)
        res.facts["sheet_targets"] = [t.get("name") for t in targets if "llow" in str(t.get("name"))]
        for name in (ALLOW_ONCE, ALLOW):
            box.vm.raw({"op": "click_named", "name": name})
            time.sleep(0.5)

    def set_accept(self, box: Box, on: bool) -> Invoked:
        return self.set_mcp_server(box, "set-accept-own-devices", on)

    def set_unasked(self, box: Box, on: bool) -> Invoked:
        """B's second MCP server setting, "Let them act without asking"."""
        return self.set_mcp_server(box, "set-act-without-asking", on)

    def set_mcp_server(self, box: Box, verb: str, on: bool) -> Invoked:
        row = self.find_row(box, verb, "agents") or self.find_row(box, verb)
        if row is None:
            raise Failed(f"{box.name} lists no {verb} action")
        return box.vm.invoke_row(row, {"on": on})

    def allow_step_once(self, box: Box, res: Result) -> None:
        """The person at `box` allows the step a run asks about once: the run's sheet has its
        "Allow once" as a button of its own (crates/session/session/src/questions.rs,
        `step_question`)."""
        targets = self.targets(box)
        self.write(f"targets-{box.letter}-step-{int(time.time())}.json", targets)
        res.facts.setdefault("step_sheets", []).append(
            [t.get("name") for t in targets if "llow" in str(t.get("name"))])
        try:
            box.vm.raw({"op": "click_named", "name": ALLOW_ONCE})
        except VmError as exc:
            # The sheet went down between seeing it and the click: nothing is left to allow.
            res.facts.setdefault("step_sheet_gone", []).append(str(exc)[:200])
        time.sleep(0.5)

    def stop_agent(self, box: Box, agent: str) -> Invoked:
        row = self.find_row(box, "stop-agent", "stop")
        if row:
            return box.vm.invoke_row(row, {"agent": agent})
        return box.vm.invoke("session", "session:agents", "stop-agent", {"agent": agent})

    # -- the run ----------------------------------------------------------------------

    def main(self) -> int:
        a, b, c, d = (self.boxes[letter] for letter in "abcd")
        facts: dict = {}
        print(f"boxes of {MEMORY_MB} MiB and {CPUS} CPUs each, in {_TOTAL_MB} MiB and {_NPROC} CPUs",
              flush=True)
        # Nobody sits at the boxes: every minute each running one is nudged as a hand on its
        # keyboard would, so none meets the idle lock a step is not about.
        def nudge():
            while True:
                time.sleep(60)
                for box in self.boxes.values():
                    if box.letter in self.running:
                        self.keep_awake(box)

        threading.Thread(target=nudge, daemon=True).start()
        try:
            self.step("E01", "Box A boots this tree's dev disk and its person is made by onboarding",
                      lambda res: self.make_a(a, res))
            self.step("E02", "Box A is branched into box B, which A takes in; both on tilo-vm's network",
                      lambda res: self.branch(a, b, res), "E01")
            self.step("E03", "B is a new device of A's person: its own key, certified by the same root",
                      lambda res: self.same_person(a, b, res), "E02")
            self.step("E04", "A has the MCP server an agent adds to reach B (`--connect --device B`)",
                      lambda res: self.register(a, b, res, facts), "E03")
            self.step("E05", "A's server lists B online (devices#list)",
                      lambda res: self.reachable(a, b, res), "E04")
            self.step("E06", "With B's setting off, B refuses the agent's calls, naming the setting, "
                             "with nobody asked",
                      lambda res: self.not_accepted(a, b, res, facts), "E05")
            self.step("E07", "With B's setting on, the agent works on B until B's Stop refuses it",
                      lambda res: self.accept_and_stop(a, b, res, facts), "E05")
            self.step("E08", "With only B's first setting on, the agent is let in unasked and registered "
                             "with A's device (key and capped label) and no environment; its "
                             "terminal#run is put to B's person, allowed once, and runs",
                      lambda res: self.work(a, b, res, facts), "E05")
            self.step("E08b", "With both B's settings on, a new connection (another session key) runs "
                              "the same terminal#run with no sheet ever up; E08 asked it with the "
                              "second setting off, in this run",
                      lambda res: self.work_unasked(a, b, res, facts), "E08")
            self.step("E10", "Two agents on two machines answer one form in one Browser tab on B, "
                             "and B's person reads who set each field",
                      lambda res: self.two_machines(a, b, res, facts), "E08")
            self.step("E11", "Three machines answer one form in that tab: A, a third device D of "
                             "the person, and B itself",
                      lambda res: self.three_machines(a, b, d, res, facts), "E10")
            self.step("E12", "B's tab and shell name the device of each agent that came from another "
                             "machine (key and capped label), and none for B's own agent",
                      self.devices_named, "E10")
            self.step("E13", "The person's third device lists the second among their devices "
                             "(devices#list)",
                      lambda res: self.third_lists_second(d, b, res), "E10")
            self.step("E14", "A's person watches the Browser window B shares, in a tab of its own "
                             "on A's shell",
                      lambda res: self.window_watched(a, b, res), "E10")
            self.step("E17", "While B's window is not shared, A's tab for it says where it runs",
                      lambda res: self.tab_says_where(res), "E10")
            self.step("E16", "A change B's person makes on the page B shares shows on A's shell",
                      lambda res: self.window_watched(a, b, res, change=True), "E14")
            self.step("E15", "B's person watches the Browser window A shares: the machine that "
                             "hosts the workspace shares, and its copy's shell shows the form",
                      lambda res: self.host_window_watched(a, b, res), "E03")
            self.step("E09", "A device of another person is refused at B's server",
                      lambda res: self.stranger(b, c, res), "E03")
        finally:
            ok = self.report()
            if not ok:
                # A stopped box has nothing to say: its logs were kept when it was stopped.
                for box in (a, b, c, d):
                    if box.letter in self.running:
                        self.keep_logs(box)
            if self.args.hold:
                print(f"holding the boxes for {self.args.hold}s: t vm ssh, then as {Path.home().name}: "
                      f"tilo-vm vm list", flush=True)
                time.sleep(self.args.hold)
            for box in (a, b, c, d):
                try:
                    box.vm.vm_cmd("stop", box.name, timeout=300, check=False)
                except VmError:
                    pass
        return 0 if ok else 1

    def make_a(self, a: Box, res: Result) -> None:
        a.vm.vm_cmd("create", a.name, "--image", str(self.args.work / "box-a.qcow2"), "--heads", "1",
                    "--resolution", OUTPUT, "--memory-mb", str(MEMORY_MB), "--cpus", str(CPUS),
                    timeout=900)
        a.vm.vm_cmd("start", a.name, timeout=900)
        self.running.add("a")
        self.log_server(a)
        self.make_person(a, res)
        self.ready(a, 300)
        status = self.identity(a)
        res.facts["identity"] = status
        if not (status.get("this_device") or {}).get("device"):
            raise Failed(f"A's person has no certified device after onboarding: {status}")

    def log_server(self, box: Box) -> None:
        """Have `box` 's session log its MCP server and questions at debug from its next start
        on; B, branched from A, inherits it. Read back only when a step fails."""
        box.vm.root_exec(
            f"d=/etc/systemd/system/tilo-vm-session@{SLOT}.service.d; mkdir -p $d && "
            f"printf '[Service]\\nEnvironment=RUST_LOG={SERVER_LOG}\\n' > $d/90-e2e-server-log.conf && "
            "systemctl daemon-reload", timeout=60, check=False)

    def make_person(self, box: Box, res: Result) -> None:
        """The box's person, made by its first login's onboarding. On a slow box onboarding may
        outlast tilo-vm's wait; the same create again carries on with it."""
        for attempt in range(1, 4):
            try:
                box.vm.run("slot", "create", SLOT, "--headful", "--outputs", f"{OUTPUT}@1",
                           "--identity", "device", timeout=900, slot=False)
                res.facts[f"onboarding_{box.letter}_attempts"] = attempt
                return
            except VmError as exc:
                self.what_shows(box, f"onboarding attempt {attempt}")
                if "did not finish" not in str(exc) or attempt == 3:
                    raise
                print(f"{box.name}: onboarding still going after attempt {attempt}; again", flush=True)

    def what_shows(self, box: Box, when: str) -> None:
        """What `box`'s session shows and holds now, into the log: a failed run keeps no outputs."""
        print(f"--- {box.name}, {when} ---", flush=True)
        try:
            names = [t.get("name") for t in box.vm.targets()]
        except VmError as exc:
            names = [f"unlisted: {exc.reason()}"]
        print(f"  controls: {names}", flush=True)
        try:
            rows = box.vm.actions("Terminal")
            print(f"  'Terminal' lists: {json.dumps(rows[:4])[:600]}", flush=True)
        except VmError as exc:
            print(f"  actions: {exc.reason()}", flush=True)
        try:
            dump = box.vm.state()
            state = dump.get("state") or dump
            print(f"  chrome: {json.dumps(state.get('chrome'))[:600]}", flush=True)
        except (VmError, ValueError) as exc:
            print(f"  state: {exc}", flush=True)
        try:
            said = box.vm.exec('grep -iE "setup|onboard|identity|first|lock|ceremony" '
                               '"$HOME/.local/state/tilo/session-logs/latest.log" | tail -n 40',
                               timeout=60, check=False)
            for line in said.splitlines():
                print("  " + line[:300], flush=True)
        except VmError as exc:
            print(f"  log: {exc.reason()}", flush=True)

    def branch(self, a: Box, b: Box, res: Result) -> None:
        # The person's flow: `vm branch` boots B once beside A to have it re-keyed and taken into
        # the workspaces A hosts, then stops both again.
        started = time.monotonic()
        out = a.vm.vm_cmd("branch", a.name, b.name, "--stop", timeout=2400)
        res.facts["branch_seconds"] = round(time.monotonic() - started, 1)
        res.facts["branch"] = out[-2000:]
        for box in (a, b):
            box.vm.vm_cmd("start", box.name, timeout=900)
            self.running.add(box.letter)
        for box in (a, b):
            self.wait_session(box)
            self.ready(box, 300)
        for box in (a, b):
            res.facts[f"lan_{box.letter}"] = self.lan(box)
        ping = a.vm.root_exec(f"ping -c 3 -W 2 {b.ip}", timeout=60, check=False)
        res.facts["ping"] = ping[-600:]
        if "not found" in ping:
            res.facts["ping"] = "the image has no ping; the boxes' agents prove the network"
        elif " 0% packet loss" not in ping and "0.0% packet loss" not in ping:
            raise Failed(f"A cannot reach B on their network: {ping[-600:]}")

    def same_person(self, a: Box, b: Box, res: Result) -> None:
        sa, sb = self.identity(a), self.identity(b)
        res.facts["a"], res.facts["b"] = sa, sb
        da, db = sa.get("this_device") or {}, sb.get("this_device") or {}
        if not db.get("device"):
            raise Failed(f"B holds no certificate of the person after its first boot: {sb}")
        if da.get("device") == db.get("device"):
            raise Failed(f"B kept A's device key {da.get('device')}: the branch was not re-keyed")
        ia, ib = (sa.get("identity") or {}).get("id"), (sb.get("identity") or {}).get("id")
        if not ia or ia != ib:
            raise Failed(f"A and B are not one person: {ia} and {ib}")

    def register(self, a: Box, b: Box, res: Result, facts: dict) -> None:
        """The MCP server an agent on A adds to reach B: A's own server, carried to B by
        `--device`, a command A has."""
        label = (b.identity.get("this_device") or {}).get("label") or ""
        if not label:
            raise Failed(f"B's device has no label to name it by: {b.identity}")
        command = [*self.server(a), "--device", label]
        out = a.vm.exec(f"command -v {shlex.quote(command[0])}", timeout=60)
        res.facts["said"], res.facts["command"] = out, command
        if not out.strip():
            raise Failed(f"A has no {command[0]} for an agent to run: {out!r}")
        facts["command"] = command
        self.put_agent(a)

    def server(self, a: Box) -> list[str]:
        """The command an agent adds for this desktop's own MCP server."""
        return ["tilo-runtime", "--connect"]

    def reachable(self, a: Box, b: Box, res: Result) -> None:
        key = (b.identity.get("this_device") or {}).get("device")
        thread, holder = self.agent_beside(a, "devices", self.server(a), E2E_DEVICE=key,
                                           E2E_WAIT_S=str(FIND_S))
        # A local client is asked about on A: its person allows it, as for any program of theirs.
        self.answer_when_asked(a, res, thread)
        t = self.finish(thread, holder)
        text = text_of(t.get("devices"))
        res.facts["devices"] = text
        res.facts["open_here"] = text_of(t.get("open_here")) or t.get("open_here")
        if not online(text, key):
            raise Failed(f"{a.letter.upper()}'s devices#list does not list {b.letter.upper()} ({key}) "
                         f"online after {FIND_S}s: {text!r}")

    def answer_when_asked(self, box: Box, res: Result, thread, within: float = 90) -> None:
        deadline = time.monotonic() + within
        while time.monotonic() < deadline and thread.is_alive():
            if self.sheet_targets(box):
                self.answer_sheet(box, res)
                return
            time.sleep(1)
        if thread.is_alive():
            raise Failed(f"{box.name} never asked its person about the agent within {within}s")

    def sheet_targets(self, box: Box) -> list[dict]:
        """The MCP server's sheet, when it is up on `box`: its "Allow once" choice."""
        try:
            return [t for t in self.targets(box) if t.get("name") == ALLOW_ONCE]
        except VmError:
            return []

    def not_accepted(self, a: Box, b: Box, res: Result, facts: dict) -> None:
        self.set_accept(b, False)
        t = self.agent(a, "not_accepted", facts["command"])
        res.facts["initialize"] = t.get("initialize")
        res.facts["apps"] = t.get("apps")
        apps = t.get("apps")
        if not isinstance(apps, dict) or (apps.get("result") or {}).get("isError") is not True:
            raise Failed(f"B answered the agent's call although its setting is off: {apps}")
        if "accept agents from your other devices" not in text_of(apps):
            raise Failed(f"B's refusal does not name the setting that would let the agent in: {apps}")
        if self.sheet_targets(b):
            raise Failed("B asked its person about the agent")
        if self.agent_sessions(b):
            raise Failed(f"an agent is registered on B: {self.agent_sessions(b)}")

    def accept_and_stop(self, a: Box, b: Box, res: Result, facts: dict) -> None:
        res.facts["setting"] = self.set_accept(b, True)
        ready = f"$HOME/{GUEST_DIR}/stop-ready"
        a.vm.exec(f'rm -f "{ready}"')
        thread, holder = self.agent_beside(a, "stop", facts["command"], E2E_READY=ready, E2E_WAIT_S="90")
        sessions = wait_until(lambda: self.agent_sessions(b), 60, 1.0)
        res.facts["sessions"] = sessions
        if not sessions:
            raise Failed("B registers no agent although its setting lets it in")
        agent = sessions[0]["key"]
        if not wait_until(lambda: "yes" in a.vm.exec(f'test -e "{ready}" && echo yes || echo no'), 120, 1.0):
            raise Failed("the agent never got through its first calls on B")
        res.facts["stopped"] = self.stop_agent(b, agent)
        t = self.finish(thread, holder)
        res.facts["initialize"] = t.get("initialize")
        res.facts["after_stop"] = t.get("after_stop")
        last = (t.get("after_stop") or [{}])[-1].get("reply")
        if not isinstance(last, dict) or "stopped this agent" not in json.dumps(last):
            raise Failed(f"B did not refuse the agent after its Stop: {t.get('after_stop')}")
        if self.agent_sessions(b):
            raise Failed(f"B still registers the agent after its Stop: {self.agent_sessions(b)}")
        # The person lets agents from their other devices in again for the steps that follow.
        self.set_accept(b, True)

    def unasked(self, res: Result, names: list[str]) -> None:
        """Nobody sits at B and both its settings are on, so no sheet may be up on it: neither
        the server's, which the first setting answers, nor a question about a step, which the
        second answers. One was up all the same and offered `names`: the failure says which of
        the two it was, by what it offered."""
        res.facts["sheet"] = [n for n in names if "llow" in n or n == DENY]
        if ALLOW_SESSION in names:
            raise Failed("B asked its person although its setting lets the person's devices in; "
                         f"its screen names {names}")
        raise Failed("B let the agent in unasked, then asked its person about a step of it "
                     f"although both its settings are on: {res.facts['sheet']}")

    def work(self, a: Box, b: Box, res: Result, facts: dict) -> None:
        """Only the first setting on: the agent is let in unasked, but its `terminal#run`, a step
        that asks first, is put to B's person on the run's sheet, who allows it once; the
        command then runs. Every later step it asks about is allowed once too."""
        self.no_sheet_before(b)
        res.facts["setting"] = self.set_accept(b, True)
        res.facts["unasked"] = self.set_unasked(b, False)
        ready, go, asking = (f"$HOME/{GUEST_DIR}/work-ready", f"$HOME/{GUEST_DIR}/work-go",
                             f"$HOME/{GUEST_DIR}/work-asking")
        a.vm.exec(f'rm -f "{ready}" "{go}" "{asking}"')
        title = f"From A {int(time.time())}"
        thread, holder = self.agent_beside(a, "work", facts["command"], E2E_READY=ready, E2E_GO=go,
                                           E2E_ASKING=asking, E2E_TITLE=title, E2E_WAIT_S="60",
                                           E2E_COMMAND=f"echo e2e-remote-asked-{int(time.time())}")
        sessions = wait_until(lambda: self.agent_sessions(b), 90, 1.0)
        res.facts["sessions"] = sessions
        if not sessions:
            names = [t.get("name") for t in self.targets(b)]
            raise Failed("B registers no agent although its setting lets the person's devices in; "
                         f"its screen names {names}")

        def there(path: str) -> bool:
            return "yes" in a.vm.exec(f'test -e "{path}" && echo yes || echo no')

        # The agent says it comes to `terminal#run` just before it asks for it. A sheet up before
        # then is about an earlier step (opening Notes or Terminal), which the person allows too.
        deadline = time.monotonic() + 120
        while not there(asking):
            if not thread.is_alive() or time.monotonic() > deadline:
                raise Failed("the agent never came to its terminal#run on B: "
                             f"{json.dumps(holder, ensure_ascii=False)[:600]}")
            if self.sheet_targets(b):
                res.facts.setdefault("asked_before_run", []).append(
                    [t.get("name") for t in self.targets(b)])
                self.allow_step_once(b, res)
            time.sleep(0.5)
        # The sheet goes up while the agent waits on it, before it got as far as saying it is ready.
        asked = wait_until(lambda: self.sheet_targets(b), 50, 0.5)
        res.facts["asked"] = bool(asked)
        if not asked:
            raise Failed("B never asked its person about the agent's terminal#run")
        if there(ready):
            raise Failed("the agent went on past terminal#run while B's person was being asked")
        self.allow_step_once(b, res)
        if not wait_until(lambda: there(ready), 120, 1.0):
            raise Failed("the agent never got past terminal#run after B's person allowed it")
        a.vm.exec(f'touch "{go}"')
        # Whatever else it asks about (making the note) the person allows once, until it is done.
        while thread.is_alive():
            if self.sheet_targets(b):
                self.allow_step_once(b, res)
            thread.join(1.0)
        t = self.finish(thread, holder)
        session = self.check_work(a, b, res, t, sessions, title)
        # What E08b stands on: in this run, with the first setting on and the second off, B's
        # person was asked about this step and allowed it once; and which session asked.
        facts["asked_run"] = {"session": session.get("key"), "accept_own_devices": True,
                              "act_without_asking": False, "asked": True}

    def work_unasked(self, a: Box, b: Box, res: Result, facts: dict) -> None:
        """Both settings on: a new connection of the agent does the whole work, `terminal#run`
        and all, with no sheet ever up on B. That the same step is asked with the second setting
        off is E08's proof, from this run and the same first setting, just before."""
        before = facts.get("asked_run") or {}
        res.facts["asked_with_second_off"] = before
        if not (before.get("asked") and before.get("accept_own_devices")
                and not before.get("act_without_asking") and before.get("session")):
            raise Failed(f"E08 did not show in this run that terminal#run is asked with the second "
                         f"setting off: {before}")
        self.no_sheet_before(b)
        if wait_until(lambda: not self.agent_sessions(b), 60, 1.0) is None:
            raise Failed(f"E08's agent is still registered on B: {self.agent_sessions(b)}")
        res.facts["setting"] = self.set_accept(b, True)
        res.facts["unasked"] = self.set_unasked(b, True)
        ready, go, asking = (f"$HOME/{GUEST_DIR}/work-ready", f"$HOME/{GUEST_DIR}/work-go",
                             f"$HOME/{GUEST_DIR}/work-asking")
        a.vm.exec(f'rm -f "{ready}" "{go}" "{asking}"')
        title = f"From A unasked {int(time.time())}"
        thread, holder = self.agent_beside(a, "work", facts["command"], E2E_READY=ready, E2E_GO=go,
                                           E2E_ASKING=asking, E2E_TITLE=title, E2E_WAIT_S="60",
                                           E2E_COMMAND=f"echo e2e-remote-unasked-{int(time.time())}")
        sheets: list = []

        def watch() -> None:
            if self.sheet_targets(b):
                sheets.append([t.get("name") for t in self.targets(b)])

        def sessions_now() -> list:
            watch()
            return self.agent_sessions(b)

        sessions = wait_until(sessions_now, 90, 1.0)
        res.facts["sessions"] = sessions
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline and \
                "yes" not in a.vm.exec(f'test -e "{ready}" && echo yes || echo no'):
            watch()
            if not thread.is_alive():
                break
            time.sleep(0.5)
        a.vm.exec(f'touch "{go}"')
        while thread.is_alive():
            watch()
            thread.join(0.5)
        res.facts["sheets"] = sheets
        if sheets:
            self.unasked(res, [str(name) for name in sheets[0]])
        t = self.finish(thread, holder)
        session = self.check_work(a, b, res, t, sessions, title)
        # Each MCP connection is an agent session of its own: this one is not E08's.
        res.facts["sessions_of"] = {"E08": before["session"], "E08b": session.get("key")}
        if not session.get("key") or session.get("key") == before["session"]:
            raise Failed(f"B registers E08b's connection under E08's key {before['session']!r}: {session}")

    def no_sheet_before(self, b: Box) -> None:
        # What an earlier agent asked is withdrawn once it is gone: no sheet stands on B before
        # this agent comes, so a sheet afterwards is this agent's.
        if wait_until(lambda: not self.sheet_targets(b), 60, 1.0) is None:
            names = [t.get("name") for t in self.targets(b)]
            raise Failed(f"a question about an agent that left still stands on B's shell: {names}")

    def check_work(self, a: Box, b: Box, res: Result, t: dict, sessions: list, title: str) -> dict:
        """What the agent from A did on B, and how B's shell registers it: under a name, with A's
        whole key and A's label as the server caps it beside the name, and with no environment.
        Its session."""
        for key in ("initialize", "devices", "apps", "open_notes", "run_command", "new_note",
                    "found_note"):
            res.facts[key] = t.get(key) if key == "initialize" else text_of(t.get(key)) or t.get(key)
        if not (isinstance(t.get("initialize"), dict) and "result" in t["initialize"]):
            raise Failed(f"the agent was not admitted on B: {t.get('initialize')}")
        opened = t.get("open_notes")
        if not (isinstance(opened, dict) and (opened.get("result") or {}).get("isError") is False):
            raise Failed(f"the agent's notes#open did not go through on B: {opened}")
        # The server answers a job that did not succeed (still pending, failed, cancelled) as an
        # error: only a command that ran is answered without one.
        ran = t.get("run_command")
        if not (isinstance(ran, dict) and (ran.get("result") or {}).get("isError") is False):
            raise Failed(f"the agent's terminal#run did not go through on B: {ran}")
        if not sessions:
            raise Failed("B registers no agent")
        # B's shell registers the agent under the name it shows the client by (its clientInfo name, then
        # its number) and says beside the name the device it came from, as B's server verified it:
        # A's whole key and A's label as the server caps it. The session's key names the agent
        # session, which says nothing of a device. It came in by B's server, by no environment.
        session, this = sessions[0], a.identity.get("this_device") or {}
        res.facts["session"] = session
        for field_name in ("device", "device_name", "environment", "environment_id"):
            if field_name not in session:
                raise Failed(f"B's shell lists the agent without its {field_name!r}: {session}")
        if not str(session.get("name") or "").startswith(CLIENT_SHOWN):
            raise Failed(f"B registers the agent under the name {session.get('name')!r}, not as "
                         f"{CLIENT_SHOWN!r}: {session}")
        if len(this.get("device", "")) != 64 or session.get("device") != this.get("device"):
            raise Failed(f"B registers the agent as one from the device {session.get('device')!r}, not "
                         f"from A's ({this.get('device')!r}): {session}")
        if (session.get("device_name") or "") != capped(this.get("label") or ""):
            raise Failed(f"B calls the device its agent came from {session.get('device_name')!r}, "
                         f"not {capped(this.get('label') or '')!r} as A's label reads: {session}")
        if session.get("environment") or session.get("environment_id"):
            raise Failed(f"B registers the agent from A as one of an environment: {session}")
        windows = b.vm.raw({"op": "windows"})
        res.facts["b_windows"] = windows
        if "so.tilo.notes" not in json.dumps(windows):
            raise Failed(f"Notes did not open on B: {json.dumps(windows)[:800]}")
        made, found = text_of(t.get("new_note")), text_of(t.get("found_note"))
        # An action the server could not run answers in words, and so does a search that finds
        # nothing, which names the title it looked for: neither is the note.
        if not made or made.startswith(("pending ", "failed")) or "did not open" in made:
            raise Failed(f"the agent could not make its note on B: {t.get('open_notes')} / {t.get('new_note')}")
        if title not in found or found.startswith("nothing matches"):
            raise Failed(f"the note the agent made is not found on B: {t.get('new_note')} / {t.get('found_note')}")
        return session

    # -- one Browser tab, several machines ---------------------------------------------

    def person_allows(self, box: Box, done: threading.Event, allowed: list) -> None:
        """The person at `box` for as long as the agents work: every sheet that asks about an
        agent is answered as their clicks would answer it, letting the agent go on unasked
        where the sheet offers that."""
        while not done.is_set():
            try:
                names = [str(t.get("name")) for t in self.targets(box)]
                for name in names:
                    if ("llow" in name or name == DENY) and f"shown: {name}" not in allowed:
                        allowed.append(f"shown: {name}")
                clicks: list[str] = []
                if ALLOW_ONCE in names:
                    clicks = [next((n for n in names if n.startswith(ALWAYS_ALLOW)), ALLOW_ONCE), ALLOW]
                elif ALLOW in names:
                    clicks = [ALLOW]
                for name in clicks:
                    box.vm.raw({"op": "click_named", "name": name})
                    time.sleep(0.5)
                if clicks and f"chose: {clicks[0]}" not in allowed:
                    allowed.append(f"chose: {clicks[0]}")
            except VmError:
                pass
            done.wait(1.0)

    def progress(self, box: Box) -> dict:
        """What the agent on `box` had been answered so far, each answer in short."""
        try:
            kept = json.loads(box.vm.exec(f'cat "$HOME/{GUEST_DIR}/tab-progress-{box.letter}.json"',
                                          timeout=60, check=False) or "{}")
        except (VmError, ValueError) as exc:
            return {"unread": str(exc)[:200]}
        return {key: (text_of(value) or json.dumps(value, ensure_ascii=False))[:420]
                for key, value in kept.items() if key not in ("tools_list", "command", "scenario")}

    def person_reads(self, box: Box, action: str) -> dict:
        """What `box`'s person is answered by one of the desktop's own actions on the page, as
        the desktop lists it for scripts. A read of a page is a job: its answer is the one its
        status ends on."""
        row = None
        for query in ("page", ""):
            row = next((r for r in self.rows(box, query) if address(r)[1] == action), None)
            if row is not None:
                break
        if row is None:
            raise Failed(f"{box.name} lists no {action} action")
        try:
            return completed(box.vm.invoke_row(row, {}, wait_ms=90_000).status)
        except VmError as exc:
            raise Failed(f"{action} did not answer {box.name}'s person: {str(exc)[:1200]}") from exc

    def serve_form(self, b: Box) -> None:
        """The form, on B's own loopback: the boxes' network has no way out. A command run in a
        box returns once everything it started has ended, so the server is one that runs for as
        long as the box does, beside the run."""
        asks = ('python3 -c "import urllib.request as u; '
                f"print('served', u.urlopen('{SITE_URL}', timeout=5).status)\"")

        def served() -> bool:
            try:
                return "served 200" in b.vm.exec(asks, timeout=60, check=False)
            except VmError:
                return False

        if served():
            return
        b.vm.write_home_file(f"{GUEST_DIR}/site/index.html", SITE.encode())

        def serve():
            try:
                self.exec_for(b, f'cd "$HOME/{GUEST_DIR}/site" && exec python3 -m http.server '
                                 f'{SITE_PORT} --bind 127.0.0.1 </dev/null >/dev/null 2>&1',
                              7200, check=False)
            except VmError:
                pass

        threading.Thread(target=serve, daemon=True).start()
        if not wait_until(served, 60, 2.0):
            raise Failed("B does not serve the form on its loopback")

    def in_one_tab(self, b: Box, parties: list[Party], url: str, res: Result) -> None:
        """Every party's agent answers its part of the form in B's one tab in the same moment,
        B's person allowing each on B's sheet. B's person then reads, through the desktop's own
        actions, who set each field and what was done in the tab."""
        try:
            self.serve_form(b)
            self.person_opens_browser(b, res)
            self.answered_in_one_tab(b, parties, url, res)
            self.browser_runs_on(b, [party.box for party in parties if party.box is not b], res)
        except (Failed, VmError):
            # How far each agent got and what B showed and said, for the log of a failed run.
            for party in parties:
                for key, value in self.progress(party.box).items():
                    res.facts[f"so_far_{party.box.letter}_{key}"] = value
            try:
                res.facts["shown_on_b"] = [str(t.get("name")) for t in self.targets(b)][:80]
                res.facts["windows_on_b"] = b.vm.raw({"op": "windows"})
            except VmError as exc:
                res.facts["shown_on_b"] = str(exc)[:300]
            try:
                said = b.vm.exec('grep -iE "browser|chromium|cef|engine" '
                                 '"$HOME/.local/state/tilo/session-logs/latest.log" | tail -n 40 | cut -c1-400',
                                 timeout=60, check=False)
                print(f"      B's session on the Browser:\n{said}", flush=True)
            except VmError as exc:
                print(f"      B's session log could not be read: {exc}", flush=True)
            raise

    def browser_runs_on(self, b: Box, others: list[Box], res: Result) -> None:
        """A Browser window is a window of the device whose engine holds its page: B's shell
        shows it, live, in its tab, and no other machine opened a Browser of its own in that
        tab. What each shell lists is read from its session; nothing is written anywhere."""
        def windows(box: Box) -> list:
            return (box.vm.raw({"op": "windows"}).get("answer") or {}).get("windows") or []

        live = [window for window in windows(b)
                if window.get("app_id") == BROWSER and window.get("kind") == "Foreign"
                and window.get("window") is not None]
        res.facts["browser_on_b"] = live
        if not live:
            raise Failed("B's shell holds no live Browser window after the agents worked in its "
                         f"tab: {json.dumps(windows(b))[:800]}")
        tabs = {window.get("tab") for window in live}
        for box in others:
            own = [window for window in windows(box)
                   if window.get("tab") in tabs and window.get("kind") in ("Foreign", "LocalApp")]
            res.facts[f"browser_tab_on_{box.letter}"] = [window for window in windows(box)
                                                         if window.get("tab") in tabs]
            if own:
                raise Failed(f"{box.name} opened a Browser of its own in the tab of the window "
                             f"that runs on B: {json.dumps(own)[:600]}")

    def answered_in_one_tab(self, b: Box, parties: list[Party], url: str, res: Result) -> None:
        self.serve_form(b)
        res.facts["setting"] = self.set_accept(b, True)
        # B's person is asked about each agent's steps here and answers them: the second setting,
        # which the step before these turned on, is off again.
        res.facts["unasked"] = self.set_unasked(b, False)
        done, allowed = threading.Event(), []
        person = threading.Thread(target=self.person_allows, args=(b, done, allowed), daemon=True)
        person.start()
        runs = []
        try:
            for party in parties:
                box = party.box
                self.put_agent(box)
                ready = f"$HOME/{GUEST_DIR}/tab-ready-{box.letter}"
                go = f"$HOME/{GUEST_DIR}/tab-go-{box.letter}"
                box.vm.exec(f'rm -f "{ready}" "{go}"')
                env = {"E2E_URL": url, "E2E_ANSWERS": json.dumps(party.answers), "E2E_READY": ready,
                       "E2E_GO": go, "E2E_WAIT_S": "300",
                       "E2E_PROGRESS": f"$HOME/{GUEST_DIR}/tab-progress-{box.letter}.json"}
                if party.leads:
                    env["E2E_NAVIGATE"] = "1"
                thread, held = self.agent_beside(box, "browser", party.command, timeout=900, **env)
                runs.append((party, thread, held, go))

                def reads(box=box, ready=ready) -> bool:
                    return "yes" in box.vm.exec(f'test -e "{ready}" && echo yes || echo no')

                # The first brings the form up; each of the others joins once the one before it
                # has read it.
                wait_until(lambda: not thread.is_alive() or reads(), 420, 2.0)
                if not reads():
                    raise Failed(f"the agent on {box.letter.upper()} never read the form in B's tab: "
                                 f"{json.dumps(held, ensure_ascii=False)[:600]}")
            sessions = self.agent_sessions(b)
            res.facts["sessions"] = sessions
            # Every agent is in B's tab now: how B's shell registers each of them is E12's to judge.
            self.tab_sessions.append({
                "parties": [{"who": party.box.letter.upper(), "remote": party.remote,
                             "device": (party.box.identity.get("this_device") or {}).get("device", ""),
                             "label": (party.box.identity.get("this_device") or {}).get("label", "")}
                            for party in parties],
                "sessions": sessions,
            })
            # All of them answer in the same moment.
            starters = [threading.Thread(target=lambda box=party.box, go=go: box.vm.exec(f'touch "{go}"'))
                        for party, _, _, go in runs]
            for starter in starters:
                starter.start()
            for starter in starters:
                starter.join(60)
            said = [(party, self.finish(thread, held, 900)) for party, thread, held, _ in runs]
        finally:
            done.set()
            person.join(10)
            res.facts["allowed"] = allowed
        for party, t in said:
            who = party.box.letter
            res.facts[f"page_{who}"] = t.get("page")
            for key in ("navigate", "forms", "fill", "fields", "record"):
                if key in t:
                    res.facts[f"{key}_{who}"] = text_of(t.get(key)) or t.get(key)
            res.facts[f"fill_window_{who}"] = [t.get("fill_from"), t.get("fill_until")]
            if "Answered" not in text_of(t.get("fill")):
                raise Failed(f"the agent on {who.upper()} did not answer its part: {t.get('fill')}")
        fields = self.person_reads(b, "read-page-fields")
        record = self.person_reads(b, "read-page-record")
        self.write(f"tab-fields-{len(parties)}.json", fields)
        self.write(f"tab-record-{len(parties)}.json", record)
        said_fields = json.dumps(fields, ensure_ascii=False)
        said_record = json.dumps(record, ensure_ascii=False)
        res.facts["fields_read"], res.facts["record_read"] = said_fields, said_record
        sets = {str(f.get("question")): f.get("set") or {} for f in _fields(fields)}
        who_set = {question: said.get("by") for question, said in sets.items()}
        res.facts["who_set"] = who_set
        fills = [e for e in _entries(record) if e.get("action") == "fill_form" and e.get("ok")]
        actors: dict[str, str] = {}
        res.facts["actors"] = actors
        # Every session of one MCP client is the same agent member, so two agents of one client
        # share the actor's name whichever machine each runs on. What tells them apart in the
        # tab is the device its record says beside the name: none for B's own agent.
        told: dict[str, list[str]] = {}
        res.facts["told_apart_by"] = told
        for party in parties:
            who = party.box.letter.upper()
            for value in party.answers.values():
                if len(value) > 3 and value not in said_fields:
                    raise Failed(f"the page on B does not hold {value!r}, which the agent on {who} "
                                 f"answered: {said_fields[:1500]}")
            mine = [e for e in fills if party.mark in json.dumps(e, ensure_ascii=False)]
            if len(mine) != 1:
                raise Failed(f"B's tab does not record once what the agent on {who} answered "
                             f"({party.mark!r}): {said_record[:1500]}")
            actor = str(mine[0].get("actor") or "")
            actors[who] = actor
            told[who] = [actor, str(mine[0].get("device") or "")]
            if not actor:
                raise Failed(f"B's tab records what the agent on {who} answered under no name: {mine[0]}")
            # Which machine that actor worked from is a step of its own: what the entry says of
            # a device beside the actor, and what the review says of one beside who set a field.
            this = party.box.identity.get("this_device") or {}
            self.tab_actors.append({
                "who": who, "actor": actor, "remote": party.remote,
                "device": this.get("device", ""), "label": this.get("label", ""),
                "said": {"the tab's record": [mine[0].get("device"), mine[0].get("device_name")],
                         **{f"the review of {question!r}": [sets.get(question, {}).get("device"),
                                                            sets.get(question, {}).get("device_name")]
                            for question in party.reviewed}},
            })
            for question in party.reviewed:
                if who_set.get(question) != actor:
                    raise Failed(f"{question!r} is not reviewed as set by the agent on {who} ({actor}): "
                                 f"{who_set}")
        if len({tuple(said) for said in told.values()}) != len(parties):
            raise Failed(f"B's tab does not tell the {len(parties)} agents apart by actor and "
                         f"device: {told}")

    def devices_named(self, res: Result) -> None:
        """Each agent that worked in B's tab from another machine is recorded there with its
        device beside its name, and who set each field it set is reviewed with it: the device's
        whole key, as that machine's own identity names it, and its label as B's server caps it.
        B's own agent is recorded and reviewed with no device. The record's device is the one
        the change it made was invoked with (`/invoker/device`): the tab takes no entry whose
        device is not that (apps/browser/src/page_record.rs). B's shell registers them the same way
        while they work, none of them as an agent of an environment."""
        res.facts["actors"] = self.tab_actors
        res.facts["sessions"] = self.tab_sessions
        if not self.tab_sessions:
            raise Failed("B's shell was not read while the agents worked in its tab")
        for held in self.tab_sessions:
            sessions = held["sessions"]
            for session in sessions:
                missing = [f for f in ("device", "device_name", "environment", "environment_id")
                           if f not in session]
                if missing:
                    raise Failed(f"B's shell lists an agent without {missing}: {session}")
                if session.get("environment") or session.get("environment_id"):
                    raise Failed(f"B's shell registers an agent in its tab as one of an environment: {session}")
                if not session.get("device") and session.get("device_name"):
                    raise Failed(f"B's shell names a device for an agent that came from none: {session}")
            for party in held["parties"]:
                who, device = party["who"], party["device"]
                if not party["remote"]:
                    if not [s for s in sessions if not s.get("device") and not s.get("device_name")]:
                        raise Failed(f"B's shell registers its own agent ({who}) with a device: {sessions}")
                    continue
                theirs = [s for s in sessions if len(device) == 64 and s.get("device") == device]
                if not theirs:
                    raise Failed(f"B's shell registers no agent from {who}'s device {device}: {sessions}")
                if any((s.get("device_name") or "") != capped(party["label"]) for s in theirs):
                    raise Failed(f"B's shell calls {who}'s device other than "
                                 f"{capped(party['label'])!r}: {theirs}")
        if not any(seen["remote"] for seen in self.tab_actors):
            raise Failed("B's tab recorded no agent from another machine")
        for seen in self.tab_actors:
            who, actor, device, label = seen["who"], seen["actor"], seen["device"], seen["label"]
            for where, (said, name) in seen["said"].items():
                if not seen["remote"]:
                    if said or name:
                        raise Failed(f"{where} says B's own agent {actor!r} worked from another "
                                     f"device: {said!r} ({name!r})")
                    continue
                if len(device) != 64 or said != device:
                    raise Failed(f"{where} names the agent on {who}, {actor!r}, with the device "
                                 f"{said!r}, not with its device {device}")
                if (name or "") != capped(label):
                    raise Failed(f"{where} calls the device of the agent on {who} {name!r}, not "
                                 f"{capped(label)!r} as that device's label reads")

    def person_opens_browser(self, b: Box, res: Result) -> None:
        """B's person opens the Browser from their shell's list of apps, and it is there once
        its window stands on the shell and stays: a Browser that ends as it starts shows its
        window for a moment."""
        def open_windows() -> bool:
            return BROWSER in json.dumps(b.vm.raw({"op": "windows"}))

        if open_windows():
            return
        # A session publishes its list of apps a few seconds after it starts: the invocation is
        # the probe, as it is for the person who presses the app's row.
        deadline = time.monotonic() + 45
        while True:
            try:
                res.facts["person_opened"] = b.vm.open(BROWSER)
                break
            except VmError as exc:
                if "unknown_action" not in str(exc) or time.monotonic() >= deadline:
                    raise Failed(f"{b.name}'s person cannot open the Browser from the shell's apps: "
                                 f"{str(exc)[:600]}") from exc
                time.sleep(0.5)
        if not wait_until(open_windows, 90, 2.0):
            raise Failed("the Browser B's person opened never stood on B's shell")
        time.sleep(6.0)
        if not open_windows():
            raise Failed("the Browser B's person opened did not stay on B's shell")

    def two_machines(self, a: Box, b: Box, res: Result, facts: dict) -> None:
        # B's person has the Browser open; the agent on A sends its tab to the form, and the
        # agent on B joins the tab.
        self.in_one_tab(b, [
            Party(a, facts["command"], ANSWERS_A, "Maria Elena",
                  ("Given names", "Family name", "Nationality", "Which country?"), leads=True),
            Party(b, self.server(b), ANSWERS_B, "P7K290XB4", ("Passport number", "Arrival"),
                  remote=False),
        ], SITE_URL, res)

    def third_device(self, a: Box, d: Box, res: Result) -> None:
        """D: A branched once more, so a third device of the person, on the boxes' network."""
        # Made as B was: `vm branch` boots D once beside A to have it re-keyed and taken into the
        # workspaces A hosts, then stops both again.
        started = time.monotonic()
        out = a.vm.vm_cmd("branch", a.name, d.name, "--stop", timeout=2400)
        res.facts["branch_seconds"] = round(time.monotonic() - started, 1)
        res.facts["branch"] = out[-1200:]
        self.running.discard("a")
        for box in (a, d):
            box.vm.vm_cmd("start", box.name, timeout=900)
            self.running.add(box.letter)
        for box in (a, d):
            self.wait_session(box, 600)
            self.ready(box, 300)
        for box in (a, d):
            res.facts[f"lan_{box.letter}"] = self.lan(box)
        sa, sd = self.identity(a), self.identity(d)
        res.facts["a"], res.facts["d"] = sa, sd
        key = lambda status: (status.get("this_device") or {}).get("device")  # noqa: E731
        if not key(sd) or key(sd) in (key(sa), key(self.boxes["b"].identity)):
            raise Failed(f"D is not a device of its own after its first boot: {sd}")
        ia, i_d = (sa.get("identity") or {}).get("id"), (sd.get("identity") or {}).get("id")
        if not ia or ia != i_d:
            raise Failed(f"A and D are not one person: {ia} and {i_d}")

    def three_machines(self, a: Box, b: Box, d: Box, res: Result, facts: dict) -> None:
        self.third_device(a, d, res)
        self.third_made = True
        # The first device finds B again on their network. The third does not list B, which a
        # step of its own asks: its agent comes through B's server by B's device id, which a server
        # takes as well as a name.
        self.reachable(a, b, res)
        by_id = list(facts["command"])
        by_id[by_id.index("--device") + 1] = (b.identity.get("this_device") or {}).get("device", "")
        res.facts["command_d"] = by_id
        # The agent on A sends the tab B has open to the form anew; the agents on D and on B
        # join it.
        self.in_one_tab(b, [
            Party(a, facts["command"], THREE_A, "Joao Pedro", ("Given names", "Family name"),
                  leads=True),
            Party(d, by_id, THREE_D, "Brazil", ("Nationality", "Which country?")),
            Party(b, self.server(b), THREE_B, "FX4410972", ("Passport number", "Arrival"),
                  remote=False),
        ], THREE_URL, res)

    def third_lists_second(self, d: Box, b: Box, res: Result) -> None:
        """Whether D, enrolled after B, knows B as one of the person's devices: what
        `devices#list` lists there, and so whether `--device <name>` could name B on D."""
        if not self.third_made:
            raise Failed("no third device was made")
        self.reachable(d, b, res)

    def window_watched(self, a: Box, b: Box, res: Result, change: bool = False) -> None:
        """B's person shares the Browser's window with the workspace, naming it by its tab
        ("Share a window with the workspace"), and A, their other device, holds its picture in
        the tab its shell lists the window in: a share is the workspace's, so nobody asks for it
        on A. A Browser window runs only on the device whose engine holds its page, so A's shell
        lists B's under the window's own tab as a window that runs on B, before anything is
        shared, and opens no Browser of its own for it; what B shares is what runs on B, which
        is what a person reviewing the tab from another machine has to see, and to go on seeing:
        the step judges by what the two sessions publish of the share and by what A's screen
        holds, when the window first came and again some seconds later. The Browser's window
        travels as its page, which A draws itself: A's shell names the page's text where it draws
        it, as it names every control (`tilo-control list-targets`), so the step reads the form's
        heading and labels off A's shell, and fails on a tab that shows the card of a window still
        arriving, whatever the sessions say. When the share ends the window is still B's on both
        shells. Before the share and after it, the tab A lists the window in holds no picture of
        it and says so: that the window runs on B, on A's screen, and that it is not shared with
        the workspace, as A's session reads it."""
        # How long after the first picture the share is read again: longer than a live session that
        # lost its server takes to say so, and than the wait before it asks the server again.
        stays_live_s = 20
        def shell(box: Box) -> dict:
            dump = box.vm.state()
            return dump.get("state") or dump

        def tabs(state: dict):
            for workspace in (state.get("workspace_tree") or {}).get("workspaces") or []:
                for strip in workspace.get("strips") or []:
                    for group in strip.get("window_groups") or []:
                        for tab in group.get("tabs") or []:
                            yield group, tab

        def browser_window(state: dict) -> tuple:
            for group, tab in tabs(state):
                tile = tab.get("tile") or {}
                if any((tile.get(kind) or {}).get("app_id") == BROWSER
                       for kind in ("Foreign", "LocalApp")):
                    return group.get("id"), tab.get("id")
            return None, None

        def listed(box: Box, query: str) -> list:
            """The rows `box`'s shell lists for `query`. A shell with a page open lists thousands
            of rows, and they change while they are paged: a listing that cannot be read whole
            is left out and said, never taken for an empty one."""
            try:
                return self.rows(box, query)
            except VmError as exc:
                unread = res.facts.setdefault("listings_unread", [])
                if len(unread) < 6:
                    unread.append(f"{box.letter} {query!r}: {str(exc)[:120]}")
                return []

        def row_of(box: Box, action: str, within: str) -> dict | None:
            # The narrow listings first: by what provides it, by its name, then everything.
            for query in (within, action, ""):
                for row in listed(box, query):
                    resource, named = address(row)
                    if named == action and within in resource + named + str(row.get("provider", "")):
                        return row
            return None

        def governed(state: dict) -> list:
            """The window shares the workspace's governance names on this shell."""
            held = (state.get("workspace_tree") or {}).get("workspaces") or []
            return [grant.get("id") for workspace in held
                    for grant in (workspace.get("policy") or {}).get("grants") or []
                    if "kind:window" in str(grant.get("node"))]

        def shares(state: dict, part: str) -> list:
            """What the session says of its window shares: `sharing` with each window's viewers,
            `starting`, `watching`, `received`, and `live`, the workspaces' live sessions."""
            return (state.get("sharing") or {}).get(part) or []

        def show_tab(box: Box, shown) -> str:
            """Bring the tab `shown` to the front of `box`'s shell with the shell's own `focus`,
            as picking the window from the shell's list does: no pointer moves. The action is
            asked of whoever the shell lists it under, and of the session where it lists none.
            What the shell answered, for the step's facts."""
            focus = ("desktop:shell", "focus")
            row = next((row for query in focus for row in listed(box, query)
                        if address(row) == focus), None)
            providers = ([str(row.get("provider"))] if row else []) + ["so.tilo.session", "session"]
            refused = []
            for provider in dict.fromkeys(providers):
                try:
                    answer = box.vm.invoke(provider, *focus, {"tab": str(shown)})
                except VmError as exc:
                    refused.append(f"{provider}: {str(exc)[:120]}")
                    continue
                return json.dumps(answer._asdict(), ensure_ascii=False)[:300]
            return "not brought into view: " + "; ".join(refused)

        def live_session(state: dict) -> dict | None:
            """The live session of the workspace the window is shared in, as a session says it
            stands: `state` is `open`, or what it lacks, with `waiting_for` in words and `relay`
            the device it runs through or waits to reach. None for a session that does not
            publish its live sessions, which says nothing either way."""
            said = state.get("sharing") or {}
            if "live" not in said:
                return None
            return next((row for row in said.get("live") or [] if row.get("workspace") == workspace),
                        {"state": "absent",
                         "waiting_for": "its session names no live session for the workspace"})

        self.person_opens_browser(b, res)
        on_b = shell(b)
        group, tab = browser_window(on_b)
        workspace = next((held.get("workspace") for held in on_b.get("held_workspaces") or []
                     if held.get("workspace")), None)
        res.facts.update(group=group, tab=tab, workspace=workspace)
        if not group or not tab or not workspace:
            raise Failed(f"B's shell does not hold the Browser's window in a workspace it shares: "
                         f"group {group}, tab {tab}, workspace {workspace}")
        # What A lists under the window's own tab before anything is shared: the window that
        # runs on B, as the workspace's layout brought it, and no window of A's own.
        b_key = (b.identity.get("this_device") or {}).get("device", "")

        def listed_on_a(state: dict | None = None):
            return next((held.get("tile") for _, held in tabs(state or shell(a))
                         if held.get("id") == tab), None)

        def runs_on_b(tile) -> bool:
            hosted = (tile or {}).get("HostedWindow") if isinstance(tile, dict) else None
            return bool(hosted) and bool(b_key) and hosted.get("host") == b_key

        wait_until(lambda: runs_on_b(listed_on_a()), 60, 2.0)
        res.facts["listed_on_a_before"] = listed_on_a()
        if not runs_on_b(res.facts["listed_on_a_before"]):
            raise Failed("A's shell does not list B's Browser window as a window that runs on B "
                         f"({b_key[:8]}): under the window's tab it lists "
                         f"{json.dumps(res.facts['listed_on_a_before'])[:300]}")

        def stands_on_a() -> dict:
            """How B's window stands on A's shell while nothing of it is shown there, from both
            places that say it. A's session reads it (`hosted` among its shares: `not-shared`,
            `awaited`, `away`, `left`, and `shown` while a share stands in the tab). A's tab says
            it in the shell's notice: its title names the app and the device the window runs on,
            and the sentence under it says why the window is not here. A shell lists its elements
            by name (`tilo-control list-targets`), and a notice's name is its title; the sentence
            under it is the notice's description, which that listing does not carry. So the title
            is read off A's screen and the reason off A's session."""
            state = shell(a)
            said = next((row for row in shares(state, "hosted")
                         if str(row.get("tab")) == str(tab)), None) or {}
            label = str(((listed_on_a(state) or {}).get("HostedWindow") or {}).get("host_label")
                        or "").strip()
            # A device nothing names is another device of the workspace, and a key is no name.
            if len(label) >= 32 and all(ch in "0123456789abcdefABCDEF" for ch in label):
                label = ""
            where = f" runs on {label}" if label else " runs on another device of this workspace"
            named = [" ".join(str(target.get("name") or "").split())
                     for target in self.targets(a)]
            return {"state": said.get("state"), "waiting_for": said.get("waiting_for"),
                    "notice": [name for name in named if name.endswith(where)],
                    "expected": f"<the app>{where}"}

        def not_shared_on_a(when: str) -> None:
            """Whether A's tab for B's window says where the window runs, and A's session reads the
            window as not shared with the workspace, `when` the share: a tab that holds no picture
            of the window is never left with nothing in it. The tab is brought to the front
            first, as a person looking at it has it."""
            res.facts[f"shown_on_a_{when}"] = show_tab(a, tab)

            def said() -> dict | None:
                seen = stands_on_a()
                return seen if seen["state"] == "not-shared" and seen["notice"] else None

            seen = wait_until(said, 60, 2.0) or stands_on_a()
            res.facts[f"stands_on_a_{when}"] = json.dumps(seen, ensure_ascii=False)[:600]
            if seen["state"] != "not-shared":
                waits = f", waiting for {seen['waiting_for']!r}" if seen["waiting_for"] else ""
                self.tab_stands[when] = (f"{when} the share, A's session does not read B's Browser window as "
                                         "one that is not shared with the workspace: it reads "
                                         f"{seen['state']!r}{waits}; its tab says {seen['notice']}")
                return
            if not seen["notice"]:
                self.tab_stands[when] = (f"{when} the share, A's tab for B's Browser window does not say where "
                                         "the window runs: A's shell names no element reading "
                                         f"{seen['expected']!r}, so the tab shows nothing of why it is empty")
                return
            self.tab_stands[when] = ""

        # Before anything is shared, the tab A lists the window in is not an empty pane: it says
        # the window runs on B, and A's session reads it as not shared with the workspace.
        not_shared_on_a("before")
        # The shell's own "Share a window" (`share-window`) is another thing: it asks Chat to put a
        # window's record in a channel. The window's picture goes to the workspace's devices
        # through the session's share, which takes the tab and finds the window itself.
        share = row_of(b, "share", "session:window-share")
        stop = row_of(b, "stop", "session:window-share")
        if share is None or stop is None:
            res.facts["share_rows"] = ["#".join(address(row)) for row in listed(b, "share")
                                       if "share" in "#".join(address(row))][:40]
            raise Failed("B's session lists no action to share a window with the workspace, or none "
                         "to stop sharing it")
        res.facts["shared"] = b.vm.invoke_row(share, {"tab": str(tab)})
        try:
            def held_on_a() -> list:
                for query in ("peer-surface", "Browser", ""):
                    rows = [row for row in listed(a, query)
                            if "desktop.peer-surface" in address(row)[0] + str(row.get("provider"))]
                    if rows:
                        return rows
                return []

            def received_on_a(state: dict) -> dict | None:
                """What A's session says it received of the window: its `received` row for the
                tab, there from the first keyframe of its pictures or the announcement of its
                page on. It says `pictured` once the tab's tile holds a decoded picture or the
                page's whole frame, and until then what it waits for (`waiting_for`)."""
                return next((row for row in shares(state, "received")
                             if str(row.get("tab")) == str(tab)), None)

            def sharing_on_b(state: dict) -> dict | None:
                """What B's session says of the window it shares: its `sharing` row for the tab,
                with `streaming` while the window goes out to a shell that asks for it (for a
                window whose app draws its page: once that app handed a page over) and
                `waiting_for` in words while something is missing."""
                return next((row for row in shares(state, "sharing")
                             if str(row.get("tab")) == str(tab)), None)

            def pictured_on_a() -> dict | None:
                row = received_on_a(shell(a))
                return row if row and row.get("pictured") is True else None

            def hosted_on_a(state: dict) -> dict | None:
                """The tab of A's shell that holds B's window as one running on another device: the
                tab A's session says it pictures the window in (`shown_in`), which is the tab A's
                shell lists the window in."""
                watched = next((row for row in shares(state, "watching")
                                if str(row.get("tab")) == str(tab)), None) or {}
                shown_in = str(watched.get("shown_in") or tab)
                tile = next((held.get("tile") for _, held in tabs(state)
                             if str(held.get("id")) == shown_in), None)
                if isinstance(tile, dict) and "HostedWindow" in tile:
                    return {"tab": shown_in, "tile": tile}
                return None

            host_label = str(((res.facts["listed_on_a_before"] or {}).get("HostedWindow") or {})
                             .get("host_label") or "")

            def screen_on_a(also: str = "") -> dict:
                """What A's screen holds of the window, read as `tilo-control list-targets` lists
                A's shell and with no pointer: every element the shell names, with where it stands.
                A tile that draws the window's page is a document named "<window> on <device>",
                and the page's text stands inside it, run by run, where it is drawn. A tile with
                nothing to draw is the card of something still arriving, which names the device
                it waits for and no text of the page. `page` is the form's text found there,
                `placeholder` that card's caption, `scope` whether the text was read inside the
                tile or, where no tile names itself, anywhere on the shell."""
                def named(target: dict) -> str:
                    return " ".join(str(target.get("name") or "").split())

                def inside(target: dict, tile: dict) -> bool:
                    x, y, w, h = target["rect"]
                    left, top, width, height = tile["rect"]
                    return (left <= x + w / 2 <= left + width
                            and top <= y + h / 2 <= top + height)

                placed = [target for target in self.targets(a)
                          if isinstance(target.get("rect"), list) and len(target["rect"]) == 4]
                tiles = [target for target in placed
                         if host_label and named(target).endswith(f" on {host_label}")]
                runs = [named(target) for target in placed
                        if target not in tiles
                        and (not tiles or any(inside(target, tile) for tile in tiles))]
                text = " ".join(runs)
                waiting = {"From the device running it"} | ({f"From {host_label}"} if host_label
                                                            else set())
                return {
                    "tile": [named(tile) for tile in tiles],
                    "scope": "tile" if tiles else "shell",
                    "page": [known for known in SITE_TEXT if known in text],
                    "placeholder": sorted({named(target) for target in placed} & waiting),
                    # Whether `also`, a text the step itself put on the page, is among it.
                    "also": bool(also) and also in text,
                    "text": text[:400],
                }

            def page_on_a() -> dict | None:
                """A's screen once it shows the form: every known text of it inside the tile
                that draws the window. Where no tile names itself the text is looked for on the
                whole shell, and a card of something still arriving from B counts against it."""
                seen = screen_on_a()
                shown = (len(seen["page"]) == len(SITE_TEXT)
                         and (seen["scope"] == "tile" or not seen["placeholder"]))
                return seen if shown else None

            # The window's content travels the workspace's live session, on a track of its own that
            # A's live session asks B's for while A's shell holds the window's tab: its pictures, or for a
            # window whose app draws its page, as the Browser's does, that page. What A's session
            # publishes says how far it came: the window's `received` row is there once the first
            # keyframe or the page's announcement reached A, and says `pictured` once its tile
            # holds a picture or the page. The rows A lists for the tile (`desktop.peer-surface`)
            # are its actions, "Move the pointer" and the like: they say nothing of the picture,
            # and are kept as a fact only.
            pictured = wait_until(pictured_on_a, 120, 2.0)
            # A's person looks at the window: the tab that holds it comes to the front of A's
            # shell, so A's picture below shows the window and not the strip beside it.
            shown_in = (hosted_on_a(shell(a)) or {}).get("tab") or tab
            res.facts["shown_on_a"] = show_tab(a, shown_in)
            res.facts["peer_surface_rows"] = json.dumps(held_on_a(), ensure_ascii=False)[:1500]
            # What that person sees there is the judge: the form's own text on A's shell. A
            # session that says a window is pictured over a tab that shows a placeholder is wrong,
            # and this is where it is found out. The pictures below are taken after it, so they
            # show what was judged.
            screen = wait_until(page_on_a, 60, 2.0) if pictured else None
            held = screen or screen_on_a()
            res.facts["screen_on_a"] = json.dumps(held, ensure_ascii=False)[:900]
            # How far the share came, on each end: the governance that names it, and what each
            # session says it shares, watches and received. `received` holds a window from its
            # first keyframe on; `viewers` are the devices that told B they view it, which is
            # what B names them and lets them in by, and no condition of the pictures.
            on_a, on_b = shell(a), shell(b)
            seen = {
                "governed_on_b": governed(on_b), "governed_on_a": governed(on_a),
                "sharing_on_b": shares(on_b, "sharing"), "starting_on_b": shares(on_b, "starting"),
                "watching_on_a": shares(on_a, "watching"), "received_on_a": shares(on_a, "received"),
                # What A lists under the window's own id, which is the window that runs on B,
                # and the tab that pictures it: the same tab.
                "listed_on_a": listed_on_a(on_a),
                "hosted_tab_on_a": hosted_on_a(on_a),
                # The pictures travel the workspace's live session: how it stands on each end.
                "live_on_b": live_session(on_b), "live_on_a": live_session(on_a),
            }
            for name, value in seen.items():
                res.facts[name] = json.dumps(value, ensure_ascii=False)[:800]
            for box in (a, b):
                try:
                    box.vm.shot(self.out / f"shot-watch-{box.letter}.png")
                except VmError as exc:
                    res.facts[f"shot_{box.letter}"] = str(exc)[:200]
            # A Browser window is shared as its page and never as pictures of it, which show
            # what the page masks: B's row for the window says which of the two goes out
            # (`as_page`), whatever A's screen holds.
            as_pictures = [shared for shared in seen["sharing_on_b"]
                           if str(shared.get("tab")) == str(tab) and shared.get("as_page") is False]
            if as_pictures:
                raise Failed("B shares its Browser window as pictures of it, not as its page: "
                             f"{json.dumps(as_pictures, ensure_ascii=False)[:300]}; A received "
                             f"{res.facts['received_on_a'][:300]}")
            if not pictured or not seen["hosted_tab_on_a"]:
                res.facts["windows_on_a"] = a.vm.raw({"op": "windows"})
                viewers = [viewer for shared in seen["sharing_on_b"]
                           for viewer in shared.get("viewers") or []]
                # A live session that is not open carries nothing, whatever the shares say: the sharing
                # device's first, since nothing is captured without it.
                closed = next(((letter, stands) for letter, stands
                               in (("B", seen["live_on_b"]), ("A", seen["live_on_a"]))
                               if stands is not None and stands.get("state") != "open"), None)
                # Whether B sends the window: its row says `streaming` once the window goes out
                # to a shell that asks for it. Whether a shell asks is what B's live session says it is
                # asked for (`asked_for`), the window's tab among them.
                unasked = [shared for shared in seen["sharing_on_b"]
                           if str(shared.get("tab")) == str(tab) and shared.get("streaming") is False]
                asked = str(tab) in [str(wanted) for wanted
                                     in (seen["live_on_b"] or {}).get("asked_for") or []]
                # The first thing that did not happen, in the order a share travels.
                if seen["starting_on_b"] and not seen["sharing_on_b"]:
                    # A share of a window whose app draws its page waits for the page
                    # (`awaits_page`) and sends no picture meanwhile.
                    waits = next((start for start in seen["starting_on_b"]
                                  if str(start.get("tab")) == str(tab)), {})
                    what = ("for Browser to offer the window's page" if waits.get("awaits_page")
                            else "to start")
                    why = f"B's share waits {what}: {res.facts['starting_on_b'][:300]}"
                elif not seen["governed_on_b"]:
                    why = "B's workspace does not say the window is shared"
                elif not seen["governed_on_a"]:
                    why = "the share B's workspace names never reached A's shell"
                elif not seen["watching_on_a"]:
                    why = ("A's shell reads the share and asks no device for the window: it does not "
                           "know which device hosts the tab")
                elif closed:
                    letter, stands = closed
                    server = stands.get("server_name") or stands.get("server") or "no device"
                    why = (f"the workspace's live session is not open on {letter} "
                           f"({stands.get('state')}, through {server}): {stands.get('waiting_for')}")
                elif unasked and not asked and not seen["received_on_a"]:
                    why = ("the live session is open on both and B's was never asked for the window's "
                           f"track, so B captures nothing: A's live session runs through "
                           f"{(seen['live_on_a'] or {}).get('server')}, B's through "
                           f"{(seen['live_on_b'] or {}).get('server')}; A watches "
                           f"{res.facts['watching_on_a'][:300]}")
                elif not seen["received_on_a"]:
                    heard = ("B names A as viewing it" if viewers
                             else "nor did A's word that it views it reach B")
                    why = ("A watches the window and no keyframe of its track reached A over the "
                           f"workspace's live session ({heard}): {res.facts['watching_on_a'][:300]}")
                elif not seen["hosted_tab_on_a"]:
                    why = ("the window's pictures reached A and its shell holds no tab for the window "
                           f"that runs on B: A received {res.facts['received_on_a'][:300]}; under "
                           f"the window's own id A lists {res.facts['listed_on_a'][:200]}")
                else:
                    # The window's start reached A and its tile holds nothing of the window: the
                    # first thing after the start that did not happen.
                    row = received_on_a(on_a) or {}
                    sent = sharing_on_b(on_b) or {}
                    if not asked:
                        link = ("B's live session is not asked for the window's track: "
                                f"{res.facts['live_on_b'][:300]}")
                    elif sent.get("streaming") is not True:
                        link = ("B's live session is asked for the window and B sent nothing of it: "
                                f"{sent.get('waiting_for') or res.facts['sharing_on_b'][:300]}")
                    else:
                        link = ("B says it sends the window and nothing A's tile can draw "
                                f"arrived: {res.facts['live_on_a'][:300]}")
                    why = ("the window's start reached A and the tab that holds the window holds "
                           f"nothing of it ({row.get('waiting_for') or 'it says no more'}): {link}; "
                           f"A received {res.facts['received_on_a'][:300]}; on A's screen: "
                           f"{res.facts['screen_on_a'][:300]}")
                raise Failed(f"A's shell never pictured the window B shares: {why}")
            if str(seen["hosted_tab_on_a"]["tab"]) != str(tab):
                raise Failed("A pictures the window B shares in a tab of its own, "
                             f"{seen['hosted_tab_on_a']['tab']}, and not in the tab its shell lists "
                             f"the window in: there it lists {res.facts['listed_on_a'][:200]}")
            if not screen:
                what = ("the card of a window still arriving" if held["placeholder"]
                        else "none of the page" if not held["page"]
                        else f"only {held['page']} of the page")
                raise Failed("A's session says the window B shares is pictured, and A's screen "
                             f"shows {what}: of {list(SITE_TEXT)} A's shell names "
                             f"{res.facts['screen_on_a'][:500]}; A received "
                             f"{res.facts['received_on_a'][:300]}")
            # A share is not a moment. Some seconds on, the window is still received and pictured
            # on A, the workspace's live session reads open on both, and B still sends the window
            # and waits for nothing: a stream that started and died fails here, by what the two
            # sessions publish.
            time.sleep(stays_live_s)
            on_a, on_b = shell(a), shell(b)
            later = {
                "received_on_a_later": received_on_a(on_a),
                "sharing_on_b_later": sharing_on_b(on_b),
                "live_on_b_later": live_session(on_b), "live_on_a_later": live_session(on_a),
            }
            for name, value in later.items():
                res.facts[name] = json.dumps(value, ensure_ascii=False)[:800]
            # The first thing that no longer holds, in the order a share travels. A session that
            # publishes no live session cannot say its live session is open.
            closed = next(((letter, stands) for letter, stands
                           in (("B", later["live_on_b_later"]), ("A", later["live_on_a_later"]))
                           if stands is None or stands.get("state") != "open"), None)
            sent, kept = later["sharing_on_b_later"], later["received_on_a_later"]
            why = None
            if closed:
                letter, stands = closed
                stands = stands or {"state": "unpublished",
                                    "waiting_for": "its session publishes no live session"}
                server = stands.get("server_name") or stands.get("server") or "no device"
                why = (f"the workspace's live session is not open on {letter} "
                       f"({stands.get('state')}, through {server}): {stands.get('waiting_for')}")
            elif sent is None:
                why = "B no longer says it shares the window"
            elif sent.get("streaming") is not True:
                why = "B no longer sends the window"
            elif sent.get("waiting_for"):
                why = f"B's share waits: {sent.get('waiting_for')}"
            elif kept is None:
                why = "A no longer holds the window it received"
            elif kept.get("pictured") is not True:
                why = ("A holds the window it received and no longer pictures it: "
                       f"{kept.get('waiting_for') or 'it says no more'}")
            else:
                still = page_on_a()
                res.facts["screen_on_a_later"] = json.dumps(still or screen_on_a(),
                                                            ensure_ascii=False)[:900]
                if not still:
                    why = ("A's session says the window is pictured and A's screen no longer "
                           f"shows the page: {res.facts['screen_on_a_later'][:400]}")
            if why:
                raise Failed(f"the window B shares was pictured on A and did not stay live for "
                             f"{stays_live_s} s: {why}; B shares: "
                             f"{res.facts['sharing_on_b_later'][:300]}; A received: "
                             f"{res.facts['received_on_a_later'][:300]}; B's live session: "
                             f"{res.facts['live_on_b_later'][:300]}; A's live session: "
                             f"{res.facts['live_on_a_later'][:300]}")
            if change:
                # A share carries what changes, not only what stood there when it began. B's
                # person answers one question of the form again, with the Browser's own action
                # on its page, and the new answer has to stand in A's tile: a reviewer shown the
                # first page and nothing typed after it reads what is no longer so.
                changed = f"Reviewed {int(time.time()) % 100000}"
                fill = row_of(b, "fill-page-form", "browser") or row_of(b, "fill-page-form", "")
                if fill is None:
                    raise Failed("B's shell lists no action that answers the form on its "
                                 "Browser's page, so nothing could be changed there")
                reply = b.vm.invoke_row(fill, {"form": "form1", "answers": {"Given names": changed}},
                                        wait_ms=90_000)
                res.facts["changed_on_b"] = json.dumps(reply._asdict(), ensure_ascii=False)[:600]
                try:
                    completed(reply.status)
                except VmError as exc:
                    raise Failed("B's person could not answer the form again on B's own page, "
                                 f"so no change was there to carry: {res.facts['changed_on_b']}") from exc
                arrived = wait_until(lambda: screen_on_a(changed)["also"], 60, 2.0)
                res.facts["screen_on_a_changed"] = json.dumps(screen_on_a(changed),
                                                              ensure_ascii=False)[:900]
                if not arrived:
                    raise Failed(f"B's person changed an answer on the shared page to "
                                 f"{changed!r} and A's shell went on showing the page as it "
                                 f"was: {res.facts['screen_on_a_changed'][:500]}; B shares: "
                                 f"{json.dumps(sharing_on_b(shell(b)), ensure_ascii=False)[:300]}")
        finally:
            # The share ends with the step: what follows finds B as it was.
            try:
                b.vm.invoke_row(stop, {"tab": str(tab)})
            except VmError as exc:
                res.facts["unshared"] = str(exc)[:200]
        # The share is over and the window is not: B still shows its own live window in the tab,
        # and A still lists it there as a window that runs on B, with no picture of it.

        def still_b_s() -> bool:
            on_b = next((held.get("tile") for _, held in tabs(shell(b))
                         if held.get("id") == tab), None)
            return isinstance(on_b, dict) and "Foreign" in on_b and runs_on_b(listed_on_a())

        stays = wait_until(still_b_s, 30, 2.0)
        res.facts["listed_on_a_after"] = listed_on_a()
        res.facts["listed_on_b_after"] = next((held.get("tile") for _, held in tabs(shell(b))
                                               if held.get("id") == tab), None)
        if not stays:
            raise Failed("the Browser's window did not stay B's once its share ended: B lists "
                         f"{json.dumps(res.facts['listed_on_b_after'])[:200]} in its tab, and A "
                         f"{json.dumps(res.facts['listed_on_a_after'])[:200]}")
        # And the tab A still lists the window in says so again: where the window runs, and that
        # it is not shared with the workspace, never the empty pane a share's end used to leave.
        not_shared_on_a("after")

    def tab_says_where(self, res: Result) -> None:
        """What E14 read of A's tab for B's window while the window was not shared, before the
        share and after it, judged here: the tab says where the window runs and A's session reads
        it as not shared, never an empty pane. E14 looks, since it is the step that has the tab
        in both states; its own verdict is whether the share reaches A, and this one is what the
        tab says while there is none."""
        found = dict(self.tab_stands)
        res.facts["stands"] = found
        missing = [when for when in ("before", "after") if when not in found]
        wrong = [said for said in found.values() if said]
        if wrong:
            raise Failed("; ".join(wrong))
        if missing:
            raise Failed("E14 did not come to look at A's tab for B's window "
                         f"{' and '.join(missing)} the share")

    def host_window_watched(self, a: Box, b: Box, res: Result) -> None:
        """E14 with the machines' parts exchanged. A, the device that hosts the workspace, opens
        a Browser of its own on the form, which A serves on its own loopback as B serves it, and
        A's person shares that window with the workspace by its tab. B, the copy, lists the
        window under its own tab as one that runs on A before anything is shared, pictures it
        there once it is, and names the form's text where it draws the page: the step reads the
        heading and labels off B's shell (`tilo-control list-targets`), when the page first came
        and again some seconds later, and fails on the card of a window still arriving whatever
        the sessions say. What the hosting device shares is a change of the governance it keeps
        itself, with no host to hand it on: the direction in which a share never arrived. When
        the share ends the window is still A's on both shells."""
        # As in E14: longer than a live session that lost its server takes to say so.
        stays_live_s = 20
        a_key = (a.identity.get("this_device") or {}).get("device", "")

        def shell(box: Box) -> dict:
            dump = box.vm.state()
            return dump.get("state") or dump

        def tabs(state: dict):
            for workspace in (state.get("workspace_tree") or {}).get("workspaces") or []:
                for strip in workspace.get("strips") or []:
                    for group in strip.get("window_groups") or []:
                        for held in group.get("tabs") or []:
                            yield workspace, held

        def tile_of(state: dict, wanted):
            return next((held.get("tile") for _, held in tabs(state)
                         if str(held.get("id")) == str(wanted)), None)

        def own_browser() -> list:
            """A's own live Browser windows. Its shell lists B's Browser too, as a window that
            runs on B, so a Browser among A's windows is not yet one A runs."""
            listed = (a.vm.raw({"op": "windows"}).get("answer") or {}).get("windows") or []
            return [window for window in listed
                    if window.get("app_id") == BROWSER and window.get("kind") == "Foreign"
                    and window.get("window") is not None]

        def rows_of(box: Box, query: str) -> list:
            """The rows `box`'s shell lists for `query`. A listing that cannot be read whole is
            left out and said, never taken for an empty one."""
            try:
                return self.rows(box, query)
            except VmError as exc:
                unread = res.facts.setdefault("listings_unread", [])
                if len(unread) < 6:
                    unread.append(f"{box.letter} {query!r}: {str(exc)[:120]}")
                return []

        def action_row(box: Box, action: str, within: str) -> dict | None:
            for query in (within, action, ""):
                for row in rows_of(box, query):
                    resource, named = address(row)
                    if named == action and within in resource + named + str(row.get("provider", "")):
                        return row
            return None

        def shares(state: dict, part: str) -> list:
            return (state.get("sharing") or {}).get(part) or []

        def row_for(state: dict, part: str) -> dict | None:
            """The session's row for the window in `part` of what it says of its shares."""
            return next((row for row in shares(state, part)
                         if str(row.get("tab")) == str(tab)), None)

        def governed(state: dict) -> list:
            """The window shares the workspace's governance names on this shell."""
            held = (state.get("workspace_tree") or {}).get("workspaces") or []
            return [grant.get("id") for workspace in held
                    for grant in (workspace.get("policy") or {}).get("grants") or []
                    if "kind:window" in str(grant.get("node"))]

        def live_session(state: dict) -> dict:
            """How the session says the live session of the window's workspace stands."""
            return next((row for row in shares(state, "live") if row.get("workspace") == workspace),
                        {"state": "absent",
                         "waiting_for": "its session names no live session for the workspace"})

        def bring_to_front(box: Box, shown) -> str:
            """Bring the tab `shown` to the front of `box`'s shell with the shell's own `focus`:
            no pointer moves. What the shell answered, for the step's facts."""
            focus = ("desktop:shell", "focus")
            row = next((row for query in focus for row in rows_of(box, query)
                        if address(row) == focus), None)
            providers = ([str(row.get("provider"))] if row else []) + ["so.tilo.session", "session"]
            refused = []
            for provider in dict.fromkeys(providers):
                try:
                    answer = box.vm.invoke(provider, *focus, {"tab": str(shown)})
                except VmError as exc:
                    refused.append(f"{provider}: {str(exc)[:120]}")
                    continue
                return json.dumps(answer._asdict(), ensure_ascii=False)[:300]
            return "not brought into view: " + "; ".join(refused)

        # The form, on A's own loopback: a Browser on A does not reach B's.
        try:
            self.serve_form(a)
        except Failed as exc:
            raise Failed(f"A does not serve the form on its own loopback ({exc})") from exc
        # A's person opens a Browser of A's own from the shell's apps. Asking for the app on a
        # shell that lists another device's Browser window opens this device's own.
        if not own_browser():
            deadline = time.monotonic() + 45
            while True:
                try:
                    res.facts["person_opened"] = a.vm.open(BROWSER)
                    break
                except VmError as exc:
                    if "unknown_action" not in str(exc) or time.monotonic() >= deadline:
                        raise Failed("A's person cannot open the Browser from the shell's apps: "
                                     f"{str(exc)[:600]}") from exc
                    time.sleep(0.5)
            if not wait_until(lambda: bool(own_browser()), 90, 2.0):
                raise Failed("the Browser A's person opened never stood on A's shell as a window "
                             f"A runs: {json.dumps(a.vm.raw({'op': 'windows'}))[:600]}")
            time.sleep(6.0)
        live = own_browser()
        if not live:
            raise Failed("the Browser A's person opened did not stay on A's shell")
        tab = live[0].get("tab")
        workspace = next((workspace.get("id") or (workspace.get("policy") or {}).get("workspace")
                     for workspace, held in tabs(shell(a)) if str(held.get("id")) == str(tab)), None)
        res.facts.update(tab=tab, workspace=workspace)
        if tab is None or not workspace:
            raise Failed("A's shell does not hold its Browser's window in a tab of a workspace: "
                         f"tab {tab}, workspace {workspace}")
        # A's person sends that Browser to the form, with the Browser's own action on its page,
        # and reads the form there before sharing anything.
        navigate = action_row(a, "navigate", "browser") or action_row(a, "navigate", "")
        if navigate is None:
            res.facts["page_rows"] = ["#".join(address(row)) for row in rows_of(a, "page")][:40]
            raise Failed("A's shell lists no action that sends its Browser's page to an address")
        res.facts["navigated"] = a.vm.invoke_row(navigate, {"url": SITE_URL})

        def form_on_a() -> bool:
            try:
                read = self.person_reads(a, "read-page-forms")
            except (Failed, VmError):
                return False
            return "Given names" in json.dumps(read, ensure_ascii=False)

        if not wait_until(form_on_a, 120, 3.0):
            raise Failed(f"A's Browser did not come to show the form A serves at {SITE_URL}")

        # What B lists under the window's own tab before anything is shared: the window that
        # runs on A, as the workspace's layout brought it, and no window of B's own.
        def runs_on_a(tile) -> bool:
            hosted = (tile or {}).get("HostedWindow") if isinstance(tile, dict) else None
            return bool(hosted) and bool(a_key) and hosted.get("host") == a_key

        wait_until(lambda: runs_on_a(tile_of(shell(b), tab)), 60, 2.0)
        before = tile_of(shell(b), tab)
        res.facts["listed_on_b_before"] = before
        if not runs_on_a(before):
            raise Failed("B's shell does not list A's Browser window as a window that runs on A "
                         f"({a_key[:8]}): under the window's tab it lists "
                         f"{json.dumps(before)[:300]}")
        host_label = str(((before or {}).get("HostedWindow") or {}).get("host_label") or "")
        share = action_row(a, "share", "session:window-share")
        stop = action_row(a, "stop", "session:window-share")
        if share is None or stop is None:
            res.facts["share_rows"] = ["#".join(address(row)) for row in rows_of(a, "share")
                                       if "share" in "#".join(address(row))][:40]
            raise Failed("A's session lists no action to share a window with the workspace, or none "
                         "to stop sharing it")
        res.facts["shared"] = a.vm.invoke_row(share, {"tab": str(tab)})
        try:
            def hosted_on_b(state: dict) -> dict | None:
                """The tab of B's shell that holds A's window as one running on another device:
                the tab B's session says it pictures the window in (`shown_in`)."""
                shown_in = str((row_for(state, "watching") or {}).get("shown_in") or tab)
                tile = tile_of(state, shown_in)
                if isinstance(tile, dict) and "HostedWindow" in tile:
                    return {"tab": shown_in, "tile": tile}
                return None

            def screen_on_b() -> dict:
                """What B's screen holds of the window, as `tilo-control list-targets` lists B's
                shell: the tile that draws the window's page is named "<window> on <device>", and
                the page's text stands inside it, run by run. A tile with nothing to draw is the
                card of something still arriving, which names the device it waits for and no
                text of the page."""
                def named(target: dict) -> str:
                    return " ".join(str(target.get("name") or "").split())

                def inside(target: dict, tile: dict) -> bool:
                    x, y, w, h = target["rect"]
                    left, top, width, height = tile["rect"]
                    return (left <= x + w / 2 <= left + width
                            and top <= y + h / 2 <= top + height)

                placed = [target for target in self.targets(b)
                          if isinstance(target.get("rect"), list) and len(target["rect"]) == 4]
                tiles = [target for target in placed
                         if host_label and named(target).endswith(f" on {host_label}")]
                runs = [named(target) for target in placed
                        if target not in tiles
                        and (not tiles or any(inside(target, tile) for tile in tiles))]
                text = " ".join(runs)
                waiting = {"From the device running it"} | ({f"From {host_label}"} if host_label
                                                            else set())
                return {
                    "tile": [named(tile) for tile in tiles],
                    "scope": "tile" if tiles else "shell",
                    "page": [known for known in SITE_TEXT if known in text],
                    "placeholder": sorted({named(target) for target in placed} & waiting),
                    "text": text[:400],
                }

            def page_on_b() -> dict | None:
                """B's screen once it shows the form: every known text of it inside the tile
                that draws the window. Where no tile names itself the text is looked for on the
                whole shell, and a card of something still arriving from A counts against it."""
                seen = screen_on_b()
                shown = (len(seen["page"]) == len(SITE_TEXT)
                         and (seen["scope"] == "tile" or not seen["placeholder"]))
                return seen if shown else None

            def pictured_on_b() -> dict | None:
                row = row_for(shell(b), "received")
                return row if row and row.get("pictured") is True else None

            pictured = wait_until(pictured_on_b, 120, 2.0)
            # B's person looks at the window: its tab comes to the front of B's shell.
            shown_in = (hosted_on_b(shell(b)) or {}).get("tab") or tab
            res.facts["shown_on_b"] = bring_to_front(b, shown_in)
            # What that person sees there is the judge: the form's own text on B's shell.
            screen = wait_until(page_on_b, 60, 2.0) if pictured else None
            held = screen or screen_on_b()
            res.facts["screen_on_b"] = json.dumps(held, ensure_ascii=False)[:900]
            on_a, on_b = shell(a), shell(b)
            seen = {
                "governed_on_a": governed(on_a), "governed_on_b": governed(on_b),
                "sharing_on_a": shares(on_a, "sharing"), "starting_on_a": shares(on_a, "starting"),
                "watching_on_b": shares(on_b, "watching"), "received_on_b": shares(on_b, "received"),
                "listed_on_b": tile_of(on_b, tab), "hosted_tab_on_b": hosted_on_b(on_b),
                "live_on_a": live_session(on_a), "live_on_b": live_session(on_b),
            }
            for name, value in seen.items():
                res.facts[name] = json.dumps(value, ensure_ascii=False)[:800]
            for box in (a, b):
                try:
                    box.vm.shot(self.out / f"shot-watch-host-{box.letter}.png")
                except VmError as exc:
                    res.facts[f"shot_{box.letter}"] = str(exc)[:200]
            as_pictures = [shared for shared in seen["sharing_on_a"]
                           if str(shared.get("tab")) == str(tab) and shared.get("as_page") is False]
            if as_pictures:
                raise Failed("A shares its Browser window as pictures of it, not as its page: "
                             f"{json.dumps(as_pictures, ensure_ascii=False)[:300]}; B received "
                             f"{res.facts['received_on_b'][:300]}")
            if not pictured or not seen["hosted_tab_on_b"]:
                closed = next(((letter, stands) for letter, stands
                               in (("A", seen["live_on_a"]), ("B", seen["live_on_b"]))
                               if stands.get("state") != "open"), None)
                sent = row_for(on_a, "sharing") or {}
                # The first thing that did not happen, in the order a share travels.
                if seen["starting_on_a"] and not seen["sharing_on_a"]:
                    waits = next((start for start in seen["starting_on_a"]
                                  if str(start.get("tab")) == str(tab)), {})
                    what = ("for Browser to offer the window's page" if waits.get("awaits_page")
                            else "to start")
                    why = f"A's share waits {what}: {res.facts['starting_on_a'][:300]}"
                elif not seen["governed_on_a"]:
                    why = "A's workspace does not say the window is shared"
                elif not seen["governed_on_b"]:
                    why = ("the share A wrote into the workspace it hosts never reached B's shell: "
                           "B's copy of the workspace's governance names no shared window")
                elif not seen["watching_on_b"]:
                    why = ("B's shell reads the share and asks no device for the window: it does not "
                           "know which device hosts the tab")
                elif closed:
                    letter, stands = closed
                    server = stands.get("server_name") or stands.get("server") or "no device"
                    why = (f"the workspace's live session is not open on {letter} "
                           f"({stands.get('state')}, through {server}): {stands.get('waiting_for')}")
                elif not seen["received_on_b"]:
                    why = ("B watches the window and nothing of it reached B over the workspace's "
                           f"live session: B watches {res.facts['watching_on_b'][:300]}; A shares "
                           f"{res.facts['sharing_on_a'][:300]}")
                elif not seen["hosted_tab_on_b"]:
                    why = ("the window's start reached B and its shell holds no tab for the window "
                           f"that runs on A: B received {res.facts['received_on_b'][:300]}; under "
                           f"the window's own id B lists {res.facts['listed_on_b'][:200]}")
                else:
                    row = row_for(on_b, "received") or {}
                    why = ("the window's start reached B and the tab that holds the window holds "
                           f"nothing of it ({row.get('waiting_for') or 'it says no more'}): A says "
                           f"of its share {sent.get('waiting_for') or res.facts['sharing_on_a'][:300]}; "
                           f"on B's screen: {res.facts['screen_on_b'][:300]}")
                raise Failed(f"B's shell never pictured the window A shares: {why}")
            if str(seen["hosted_tab_on_b"]["tab"]) != str(tab):
                raise Failed("B pictures the window A shares in a tab of its own, "
                             f"{seen['hosted_tab_on_b']['tab']}, and not in the tab its shell lists "
                             f"the window in: there it lists {res.facts['listed_on_b'][:200]}")
            if not screen:
                what = ("the card of a window still arriving" if held["placeholder"]
                        else "none of the page" if not held["page"]
                        else f"only {held['page']} of the page")
                raise Failed("B's session says the window A shares is pictured, and B's screen "
                             f"shows {what}: of {list(SITE_TEXT)} B's shell names "
                             f"{res.facts['screen_on_b'][:500]}; B received "
                             f"{res.facts['received_on_b'][:300]}")
            # A share is not a moment: some seconds on, it is read again on both machines.
            time.sleep(stays_live_s)
            on_a, on_b = shell(a), shell(b)
            sent, kept = row_for(on_a, "sharing"), row_for(on_b, "received")
            later = {"sharing_on_a_later": sent, "received_on_b_later": kept,
                     "live_on_a_later": live_session(on_a), "live_on_b_later": live_session(on_b)}
            for name, value in later.items():
                res.facts[name] = json.dumps(value, ensure_ascii=False)[:800]
            closed = next(((letter, stands) for letter, stands
                           in (("A", later["live_on_a_later"]), ("B", later["live_on_b_later"]))
                           if stands.get("state") != "open"), None)
            why = None
            if closed:
                letter, stands = closed
                server = stands.get("server_name") or stands.get("server") or "no device"
                why = (f"the workspace's live session is not open on {letter} "
                       f"({stands.get('state')}, through {server}): {stands.get('waiting_for')}")
            elif sent is None:
                why = "A no longer says it shares the window"
            elif sent.get("streaming") is not True:
                why = "A no longer sends the window"
            elif sent.get("waiting_for"):
                why = f"A's share waits: {sent.get('waiting_for')}"
            elif kept is None:
                why = "B no longer holds the window it received"
            elif kept.get("pictured") is not True:
                why = ("B holds the window it received and no longer pictures it: "
                       f"{kept.get('waiting_for') or 'it says no more'}")
            else:
                still = page_on_b()
                res.facts["screen_on_b_later"] = json.dumps(still or screen_on_b(),
                                                            ensure_ascii=False)[:900]
                if not still:
                    why = ("B's session says the window is pictured and B's screen no longer "
                           f"shows the page: {res.facts['screen_on_b_later'][:400]}")
            if why:
                raise Failed(f"the window A shares was pictured on B and did not stay live for "
                             f"{stays_live_s} s: {why}; A shares: "
                             f"{res.facts['sharing_on_a_later'][:300]}; B received: "
                             f"{res.facts['received_on_b_later'][:300]}")
        finally:
            # The share ends with the step: what follows finds A as it was.
            try:
                a.vm.invoke_row(stop, {"tab": str(tab)})
            except VmError as exc:
                res.facts["unshared"] = str(exc)[:200]
        # The share is over and the window is not: A still shows its own live window in the tab,
        # B still lists it there as a window that runs on A, and B holds nothing of the share.

        def still_a_s() -> bool:
            own = tile_of(shell(a), tab)
            on_b = shell(b)
            return (isinstance(own, dict) and "Foreign" in own and runs_on_a(tile_of(on_b, tab))
                    and row_for(on_b, "watching") is None and row_for(on_b, "received") is None)

        stays = wait_until(still_a_s, 30, 2.0)
        on_b = shell(b)
        res.facts["listed_on_a_after"] = tile_of(shell(a), tab)
        res.facts["listed_on_b_after"] = tile_of(on_b, tab)
        res.facts["shares_on_b_after"] = json.dumps(
            {"watching": row_for(on_b, "watching"), "received": row_for(on_b, "received")},
            ensure_ascii=False)[:400]
        if not stays:
            raise Failed("once A stopped sharing, the Browser's window did not stay A's with "
                         "nothing of the share left on B: A lists "
                         f"{json.dumps(res.facts['listed_on_a_after'])[:200]} in its tab, B "
                         f"{json.dumps(res.facts['listed_on_b_after'])[:200]}, and B's session "
                         f"says {res.facts['shares_on_b_after']}")

    def stranger(self, b: Box, c: Box, res: Result) -> None:
        # C runs beside B in the place of the person's other devices.
        for letter in ("a", "d"):
            # What these two said is kept now: once stopped they have nothing to say.
            if letter in self.running:
                self.keep_logs(self.boxes[letter])
            self.running.discard(letter)
            self.boxes[letter].vm.vm_cmd("stop", self.boxes[letter].name, timeout=300, check=False)
        c.vm.vm_cmd("create", c.name, "--image", str(self.args.work / "box-c.qcow2"), "--heads", "1",
                    "--resolution", OUTPUT, "--memory-mb", str(MEMORY_MB), "--cpus", str(CPUS),
                    timeout=900)
        c.vm.vm_cmd("start", c.name, timeout=900)
        self.running.add("c")
        self.make_person(c, res)
        res.facts["lan"] = self.lan(c)
        self.ready(c, 300)
        sc = self.identity(c)
        if (sc.get("identity") or {}).get("id") == (b.identity.get("identity") or {}).get("id"):
            raise Failed("C is the same person as B")
        key = (b.identity.get("this_device") or {}).get("device")
        self.put_agent(c)
        t = self.agent(c, "refused", ["tilo-runtime", "--connect", "--device", key], E2E_WAIT_S="60")
        res.facts["transcript"] = t
        init = t.get("initialize")
        if isinstance(init, dict) and "result" in init:
            raise Failed(f"B's server admitted another person's device: {init}")
        said = json.dumps(t)
        if "someone else" not in said and "refus" not in said.lower() and "did not let" not in said:
            raise Failed(f"C was not told B's server refused it: {said[:1500]}")
        # B's shell says of each agent it registers which device it came from: none came from C's.
        from_c = (sc.get("this_device") or {}).get("device", "")
        if len(from_c) != 64:
            raise Failed(f"C's identity names no device: {sc}")
        if [s for s in self.agent_sessions(b) if s.get("device") == from_c]:
            raise Failed(f"B registers C's agent: {self.agent_sessions(b)}")


def _found(value, key: str) -> list:
    """Every list held under `key` anywhere in `value`: what an action answered is wrapped
    differently by each server that carries it."""
    found = []
    if isinstance(value, dict):
        for name, inner in value.items():
            if name == key and isinstance(inner, list):
                found.append(inner)
            found += _found(inner, key)
    elif isinstance(value, list):
        for inner in value:
            found += _found(inner, key)
    elif isinstance(value, str) and value[:1] in ("{", "["):
        try:
            found += _found(json.loads(value), key)
        except ValueError:
            pass
    return found


def _entries(record) -> list[dict]:
    return [e for entries in _found(record, "entries") for e in entries if isinstance(e, dict)]


def _fields(review) -> list[dict]:
    return [f for fields in _found(review, "fields") for f in fields if isinstance(f, dict)]


def online(text: str, device: str) -> bool:
    """Whether the `devices#list` answer `text` names `device` as online."""
    try:
        answer = json.loads(text)
    except ValueError:
        return False
    if isinstance(answer, dict) and isinstance(answer.get("result"), dict):
        answer = answer["result"]
    devices = answer.get("devices") if isinstance(answer, dict) else None
    return any(isinstance(one, dict) and one.get("device") == device and one.get("online") is True
               for one in devices or [])


def text_of(reply) -> str:
    try:
        return "\n".join(block.get("text", "") for block in reply["result"]["content"])
    except (KeyError, TypeError, AttributeError):
        return ""


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--work", type=Path, required=True, help="where box-a.qcow2 and box-c.qcow2 are")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--commit", default="unknown")
    ap.add_argument("--tilo-vm", default="tilo-vm")
    ap.add_argument("--hold", type=int, default=0, help="keep the boxes up this long after the run")
    return Run(ap.parse_args()).main()


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""An agent on one of the person's Tilo boxes steers another of their boxes over MCP.

    scripts/e2e-remote/run.py --work DIR --out DIR --taps TAP_A,TAP_B,TAP_C [--hold SECONDS]

What a person does, on real booted boxes (tools/ci/e2e-remote.sh makes the host):

1. Box A is created from this tree's dev disk and its person made the way a device's first
   login makes one (`tilo-vm slot create --identity device`). A is then branched into box B
   (`tilo-vm vm branch`), which comes up as a new device of the same person. Box C is
   another person's. The three share one local network with no DHCP and no route out.
2. On A, `tilo agent connect claude --device B` registers B's door with Claude Code; the
   scripted client (guest/agent.py, clientInfo `claude-code`) runs exactly the command it
   registered.
3. A's own door lists B as reachable (`tilo_devices`).
4. With B's "Accept agents from my other devices" off and nobody at B, the agent is not
   admitted; the person approving B's sheet admits it; their Stop on B ends A's connection.
5. With the setting on (turned on through its own action), the agent works on B without a
   question: it lists B's apps, opens Notes and makes a note there, attributed on B to
   `claude-code@<A>-<id>`.
6. Two agents on two machines work in one Browser tab on B at once: B's person opens the
   Browser from the desk's apps, the agent on A, through B's door, sends its tab to a form B
   serves on its loopback, and an agent on B itself joins it. Each answers its part of the
   form with `fill-page-form` in the same moment; B's person allows each agent on B's sheet.
   B's person then reads, through the desktop's own actions, who set each field
   (`read-page-fields`) and what was done in the tab, in order (`read-page-record`): both
   agents, each under its own name.
7. Three machines work in that one tab: A is branched once more into D, a third device of the
   person, and the agents on A and on D, both through B's door, and the agent on B each answer
   a part of the form in the same moment. B's person reads three names in the tab's record and
   which of them set each field. The agent on D names B by its device id, which a door takes
   as well as a name: D does not list B (step 9).
8. In B's tab, each agent that came from another machine is recorded with its device beside
   its name: the device B's door verified it came from, by its whole key and its name, in the
   tab's record and in who set each field. B's own agent is recorded with none.
9. The person's third device lists the second among their devices (`tilo_devices`), as the
   first does, so that `--device <name>` can name it there.
10. A person at another machine watches that tab: B's person shares the Browser's window with
    the workspace, naming its tab, and A's desk holds the window's picture in a tab of its own,
    as a window that runs on B, whatever it lists as its own view of that window. A picture of
    each screen is kept.
11. C, another person's device, is refused at B's door.

Every step is a result in report.json / report.md under --out; any failure fails the run.
"""

from __future__ import annotations

import argparse
import os
import base64
import json
import shlex
import sys
import threading
import time
import traceback
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "vm-acceptance"))

from acceptance.vm import TiloVm, VmError, first_json, wait_until  # noqa: E402

GUEST_AGENT = HERE / "guest" / "agent.py"
GUEST_DIR = ".cache/e2e-remote"
MARK = "E2E-MCP "
SLOT = "me"
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
# The boxes' own network, addressed by hand: nothing on it hands out addresses.
LAN = "10.77.0.{}/24"
# How long a step that waits on the boxes finding each other may take.
FIND_S = 180
# The agent door sheet's choice and button, as the person reads them (en-US).
ALLOW_ONCE = "Allow once"
ALLOW = "Allow"
DENY = "Deny"
# The Browser, as the desk names the app.
BROWSER = "so.tilo.browser"
# How long the person is not there to answer B's sheet.
UNANSWERED_S = 45
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
# What each of the two agents answers: the one from A the person, the one on B the journey.
ANSWERS_A = {"Given names": "Maria Elena", "Family name": "Okonkwo-Reyes",
             "Nationality": "Other", "Which country?": "Portugal"}
ANSWERS_B = {"Passport number": "P7K290XB4", "Refused before?": "No", "Arrival": "2027-02-03"}
# The same form answered anew by three: the agents on A and on D, and the one on B.
THREE_URL = SITE_URL + "?three"
THREE_A = {"Given names": "Joao Pedro", "Family name": "Almeida-Santos"}
THREE_D = {"Nationality": "Other", "Which country?": "Brazil"}
THREE_B = {"Passport number": "FX4410972", "Refused before?": "No", "Arrival": "2027-05-19"}


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
    tap: str
    mac: str
    ip: str
    identity: dict = field(default_factory=dict)

    @property
    def name(self) -> str:
        return self.vm.vm


@dataclass
class Party:
    """An agent working in the tab: the box it runs on, the door it comes through, what it
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
        # Whether the third device D was made, for the step that asks what it lists.
        self.third_made = False
        self.log = self.out / "tilo-vm.log"
        taps = args.taps.split(",")
        self.running: set[str] = set()
        self.boxes = {
            letter: Box(letter, TiloVm(args.tilo_vm, f"e2e-{letter}", SLOT, self.log), tap,
                        f"52:54:00:e2:e0:0{n}", LAN.format(n))
            for n, (letter, tap) in enumerate(zip("abcd", taps), start=1)
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

    def plug_lan(self, box: Box) -> None:
        """Put `box` on the boxes' network: its tap, as a second NIC of the stopped domain."""
        if box.tap in self.virsh("dumpxml", box.name, "--inactive", check=False):
            return
        nic = self.args.work / f"lan-{box.letter}.xml"
        nic.write_text(f"<interface type='ethernet'><mac address='{box.mac}'/>"
                       f"<target dev='{box.tap}' managed='no'/><model type='virtio'/></interface>\n")
        self.virsh("attach-device", box.name, str(nic), "--config")

    def address_lan(self, box: Box) -> str:
        """Give `box` its address on the boxes' network, as a person sets one by hand."""
        script = f"""
set -e
dev=$(ip -o link | awk -v mac={box.mac} 'index($0, mac) {{ sub(":$", "", $2); print $2; exit }}')
[ -n "$dev" ] || {{ echo "no interface with {box.mac}"; ip -o link; exit 1; }}
if command -v nmcli >/dev/null 2>&1 && nmcli -t general status >/dev/null 2>&1; then
  nmcli con delete tilo-e2e >/dev/null 2>&1 || true
  nmcli con add type ethernet ifname "$dev" con-name tilo-e2e ipv4.method manual \
    ipv4.addresses {box.ip} ipv6.method link-local >/dev/null
  nmcli con up tilo-e2e >/dev/null
else
  ip link set "$dev" up
  ip addr replace {box.ip} dev "$dev"
fi
echo "$dev"
ip -o addr show dev "$dev"
"""
        return box.vm.root_exec(script, timeout=120)

    def identity(self, box: Box) -> dict:
        out = box.vm.exec("tilo-identity --json status", timeout=60)
        status = first_json(out)
        box.identity = status
        return status

    def wait_session(self, box: Box, timeout: float = 300) -> None:
        """The box's session as it came up with the box: its desk, with nothing restarted to
        get there, as a person who turns the box on finds it."""
        self.ready(box, timeout)

    def ready(self, box: Box, timeout: float = 120) -> None:
        """Once `box`'s session answers its control socket with its desk drawn and unlocked,
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
            raise Failed(f"{box.name}'s session did not show its desk within {timeout:.0f}s")

    def keep_awake(self, *boxes: Box) -> None:
        for box in boxes:
            try:
                box.vm.key("shift")
            except VmError:
                pass

    def keep_logs(self, box: Box) -> None:
        """What `box`'s session said, for a run that failed: its agent door, its endpoint and
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
        telling = ("agent door", "agent_door", "remote_door", "lan:", "onboarding", "identity",
                   "personal", "ERROR", "WARN", "panicked", "refused", "branch")
        excerpt = ([l for l in kept if any(t in l for t in telling)][-80:]
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

    # -- B's desk ------------------------------------------------------------------

    def seated(self, box: Box) -> list[dict]:
        dump = box.vm.state()
        return (dump.get("state") or dump).get("agents") or []

    def rows(self, box: Box, query: str = "") -> list[dict]:
        return box.vm.actions(query)

    def find_row(self, box: Box, action: str, query: str = "") -> dict | None:
        for row in self.rows(box, query):
            if str(row.get("id", "")).split("\x1f")[-1] == action:
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

    def set_accept(self, box: Box, on: bool) -> dict:
        row = self.find_row(box, "set-accept-own-devices", "agents") \
            or self.find_row(box, "set-accept-own-devices")
        if row is None:
            raise Failed(f"{box.name} lists no set-accept-own-devices action")
        return box.vm.invoke(row["provider"], row["id"], {"on": on})

    def stop_agent(self, box: Box, agent: str) -> dict:
        row = self.find_row(box, "stop-agent", "stop")
        provider, rid = (row["provider"], row["id"]) if row else ("session", "session:desk\x1fstop-agent")
        return box.vm.invoke(provider, rid, {"agent": agent})

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
            self.step("E02", "Box A is branched into box B; both come up on one local network",
                      lambda res: self.branch(a, b, res), "E01")
            self.step("E03", "B is a new device of A's person: its own key, certified by the same root",
                      lambda res: self.same_person(a, b, res), "E02")
            self.step("E04", "`tilo agent connect claude --device B` on A registers B's door",
                      lambda res: self.register(a, b, res, facts), "E03")
            self.step("E05", "A's door lists B as reachable (tilo_devices)",
                      lambda res: self.reachable(a, b, res), "E04")
            self.step("E06", "With B's setting off and nobody at B, the agent is not admitted",
                      lambda res: self.unanswered(a, b, res, facts), "E05")
            self.step("E07", "Approving B's sheet admits it; B's Stop ends A's connection",
                      lambda res: self.approve_and_stop(a, b, res, facts), "E05")
            self.step("E08", "With B's setting on, the agent works on B unasked, attributed to A",
                      lambda res: self.work(a, b, res, facts), "E05")
            self.step("E10", "Two agents on two machines answer one form in one Browser tab on B, "
                             "and B's person reads who set each field",
                      lambda res: self.two_machines(a, b, res, facts), "E08")
            self.step("E11", "Three machines answer one form in that tab: A, a third device D of "
                             "the person, and B itself",
                      lambda res: self.three_machines(a, b, d, res, facts), "E10")
            self.step("E12", "B's tab names the device of each agent that came from another machine",
                      self.devices_named, "E10")
            self.step("E13", "The person's third device lists the second among their devices "
                             "(tilo_devices)",
                      lambda res: self.third_lists_second(d, b, res), "E10")
            self.step("E14", "A's person watches the Browser window B shares, in a tab of its own "
                             "on A's desk",
                      lambda res: self.window_watched(a, b, res), "E10")
            self.step("E09", "A device of another person is refused at B's door",
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
        self.make_person(a, res)
        self.ready(a, 300)
        status = self.identity(a)
        res.facts["identity"] = status
        if not (status.get("this_device") or {}).get("device"):
            raise Failed(f"A's person has no certified device after onboarding: {status}")

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
        # Only made here: `vm branch` would otherwise boot A and B once to have A take B in,
        # before they are plugged into the network they meet on, and wait out its admission
        # timeout. This step boots them on that network itself, and E03 reads what came of it.
        out = a.vm.vm_cmd("branch", a.name, b.name, "--stop", "--no-admit", timeout=900)
        res.facts["branch"] = out[-2000:]
        for box in (a, b):
            self.plug_lan(box)
        for box in (a, b):
            box.vm.vm_cmd("start", box.name, timeout=900)
            self.running.add(box.letter)
        for box in (a, b):
            self.wait_session(box)
        for box in (a, b):
            res.facts[f"lan_{box.letter}"] = self.address_lan(box)
        # The sessions start again on the network the boxes now share, as they would on a box
        # that came up on it.
        for box in (a, b):
            box.vm.run("slot", "restart", SLOT, timeout=300, slot=False)
            self.ready(box, 300)
        ping = a.vm.root_exec(f"ping -c 3 -W 2 {b.ip.split('/')[0]}", timeout=60, check=False)
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
        """`tilo agent connect claude --device B`, with Claude Code's `claude` standing in as a
        program that keeps what it was asked to register."""
        label = (b.identity.get("this_device") or {}).get("label") or ""
        if not label:
            raise Failed(f"B's device has no label to name it by: {b.identity}")
        fake = ('#!/bin/sh\nprintf "%s\\n" "$@" > "$HOME/' + GUEST_DIR + '/claude-args"\n'
                'echo "Added stdio MCP server"\n')
        a.vm.write_home_file(f"{GUEST_DIR}/bin/claude", fake.encode())
        out = a.vm.exec(f'chmod +x "$HOME/{GUEST_DIR}/bin/claude" && '
                        f'PATH="$HOME/{GUEST_DIR}/bin:$PATH" tilo agent connect claude --device {shlex.quote(label)}'
                        f' && echo ARGS && cat "$HOME/{GUEST_DIR}/claude-args"', timeout=120)
        res.facts["said"] = out
        argv = [line for line in out.split("ARGS\n", 1)[-1].splitlines()
                if not line.startswith("receipt:")]
        if argv[:4] != ["mcp", "add", "--scope", "user"] or "--" not in argv:
            raise Failed(f"`tilo agent connect` registered something else: {argv}")
        command = argv[argv.index("--") + 1:]
        res.facts["server"], res.facts["command"] = argv[4], command
        if "--device" not in command or command[command.index("--device") + 1] != label:
            raise Failed(f"the registered command does not name B: {command}")
        facts["command"] = command
        self.put_agent(a)

    def door(self, a: Box) -> list[str]:
        """The command `tilo agent connect claude` registers for this desktop's own door."""
        return ["tilo-runtime-mcp", "--connect"]

    def reachable(self, a: Box, b: Box, res: Result) -> None:
        key = (b.identity.get("this_device") or {}).get("device")
        thread, holder = self.agent_beside(a, "devices", self.door(a), E2E_DEVICE=key,
                                           E2E_WAIT_S=str(FIND_S))
        # A local client is asked about on A: its person allows it, as for any program of theirs.
        self.answer_when_asked(a, res, thread)
        t = self.finish(thread, holder)
        text = text_of(t.get("devices"))
        res.facts["devices"] = text
        res.facts["open_here"] = text_of(t.get("open_here")) or t.get("open_here")
        line = next((l for l in text.splitlines() if key in l), None)
        if line is None or not line.rstrip().endswith("  reachable"):
            raise Failed(f"{a.letter.upper()}'s tilo_devices does not list {b.letter.upper()} ({key}) "
                         f"as reachable after {FIND_S}s: {text!r}")

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
        """The agent door's sheet, when it is up on `box`: its "Allow once" choice."""
        try:
            return [t for t in self.targets(box) if t.get("name") == ALLOW_ONCE]
        except VmError:
            return []

    def unanswered(self, a: Box, b: Box, res: Result, facts: dict) -> None:
        self.set_accept(b, False)
        thread, holder = self.agent_beside(a, "unanswered", facts["command"],
                                           E2E_WAIT_S=str(UNANSWERED_S))
        asked = wait_until(lambda: self.sheet_targets(b), UNANSWERED_S, 1.0)
        res.facts["asked"] = asked
        t = self.finish(thread, holder)
        res.facts["initialize"] = t.get("initialize")
        if t.get("initialize") != "timeout":
            raise Failed(f"nobody answered B, yet the agent's initialize was answered: {t.get('initialize')}")
        if not asked:
            raise Failed("B's person was never asked about the agent")
        gone = wait_until(lambda: not self.sheet_targets(b), 30, 1.0)
        if not gone:
            raise Failed("B's question stayed up after the agent left")
        if self.seated(b):
            raise Failed(f"an agent is seated on B: {self.seated(b)}")

    def approve_and_stop(self, a: Box, b: Box, res: Result, facts: dict) -> None:
        ready = f"$HOME/{GUEST_DIR}/stop-ready"
        a.vm.exec(f'rm -f "{ready}"')
        thread, holder = self.agent_beside(a, "stop", facts["command"], E2E_READY=ready, E2E_WAIT_S="90")
        self.answer_when_asked(b, res, thread)
        seated = wait_until(lambda: [s for s in self.seated(b) if s.get("seat")], 60, 1.0)
        res.facts["seated"] = seated
        if not seated:
            raise Failed("B seats no agent after its person allowed it")
        agent = seated[0]["key"]
        if not wait_until(lambda: "yes" in a.vm.exec(f'test -e "{ready}" && echo yes || echo no'), 120, 1.0):
            raise Failed("the agent never got through its first calls on B")
        res.facts["stopped"] = self.stop_agent(b, agent)
        t = self.finish(thread, holder)
        res.facts["initialize"] = t.get("initialize")
        res.facts["after_stop"] = t.get("after_stop")
        last = (t.get("after_stop") or [{}])[-1].get("reply")
        if last not in (None,) and not (isinstance(last, dict) and "error" in last):
            raise Failed(f"A's connection did not end after B's Stop: {t.get('after_stop')}")
        if [s for s in self.seated(b) if s.get("seat")]:
            raise Failed(f"B still seats the agent after its Stop: {self.seated(b)}")

    def work(self, a: Box, b: Box, res: Result, facts: dict) -> None:
        res.facts["setting"] = self.set_accept(b, True)
        ready, go = f"$HOME/{GUEST_DIR}/work-ready", f"$HOME/{GUEST_DIR}/work-go"
        a.vm.exec(f'rm -f "{ready}" "{go}"')
        title = f"From A {int(time.time())}"
        thread, holder = self.agent_beside(a, "work", facts["command"], E2E_READY=ready, E2E_GO=go,
                                           E2E_TITLE=title, E2E_WAIT_S="60")
        seated = wait_until(lambda: [s for s in self.seated(b) if s.get("seat")], 90, 1.0)
        res.facts["seated"] = seated
        if self.sheet_targets(b):
            raise Failed("B asked its person although its setting lets the person's devices in")
        wait_until(lambda: "yes" in a.vm.exec(f'test -e "{ready}" && echo yes || echo no'), 120, 1.0)
        a.vm.exec(f'touch "{go}"')
        t = self.finish(thread, holder)
        for key in ("initialize", "devices", "apps", "open_notes", "new_note", "found_note"):
            res.facts[key] = t.get(key) if key == "initialize" else text_of(t.get(key)) or t.get(key)
        if not (isinstance(t.get("initialize"), dict) and "result" in t["initialize"]):
            raise Failed(f"the agent was not admitted on B: {t.get('initialize')}")
        if not seated:
            raise Failed("B seats no agent")
        agent = seated[0]["key"]
        name_a = (a.identity.get("this_device") or {}).get("device", "")[:8]
        if not agent.startswith("claude-code@") or name_a not in agent:
            raise Failed(f"B seats the agent as {agent!r}, not as claude-code@<A>-{name_a}")
        windows = b.vm.raw({"op": "windows"})
        res.facts["b_windows"] = windows
        if "so.tilo.notes" not in json.dumps(windows):
            raise Failed(f"Notes did not open on B: {json.dumps(windows)[:800]}")
        made, found = text_of(t.get("new_note")), text_of(t.get("found_note"))
        # An action the door could not run answers in words, and so does a search that finds
        # nothing, which names the title it looked for: neither is the note.
        if not made or made.startswith(("pending ", "failed")) or "did not open" in made:
            raise Failed(f"the agent could not make its note on B: {t.get('open_notes')} / {t.get('new_note')}")
        if title not in found or found.startswith("nothing matches"):
            raise Failed(f"the note the agent made is not found on B: {t.get('new_note')} / {t.get('found_note')}")

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
            row = next((r for r in box.vm.actions(query, programmatic=True)
                        if str(r.get("id", "")).split("\x1f")[-1] == action), None)
            if row is not None:
                break
        if row is None:
            raise Failed(f"{box.name} lists no {action} action")
        reply = box.vm.invoke(row["provider"], row["id"], {})
        job = (reply.get("receipt") or {}).get("broker_invocation_id")

        def state(of: dict) -> str:
            return str(((of.get("receipt") or {}).get("update") or {}).get("state"))

        def ended():
            now = box.vm.control_json("actions", "status", str(job))
            return now if state(now) in ("succeeded", "failed", "cancelled") else None

        if job is not None and state(reply) in ("accepted", "running"):
            reply = wait_until(ended, 90, 1.0) or reply
        if state(reply) != "succeeded":
            raise Failed(f"{action} did not answer {box.name}'s person: "
                         f"{json.dumps(reply, ensure_ascii=False)[:1200]}")
        return reply

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

    def answered_in_one_tab(self, b: Box, parties: list[Party], url: str, res: Result) -> None:
        self.serve_form(b)
        res.facts["setting"] = self.set_accept(b, True)
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
            res.facts["seated"] = [s.get("key") for s in self.seated(b) if s.get("seat")]
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
        if len(set(actors.values())) != len(parties):
            raise Failed(f"B's tab does not tell the {len(parties)} agents apart: {actors}")

    def devices_named(self, res: Result) -> None:
        """Each agent that worked in B's tab from another machine is recorded there with its
        device beside its name, and who set each field it set is reviewed with it: the device's
        whole key, as that machine's own identity names it, and its name. B's own agent is
        recorded and reviewed with no device."""
        res.facts["actors"] = self.tab_actors
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
                if (name or "") != label:
                    raise Failed(f"{where} calls the device of the agent on {who} {name!r}, not "
                                 f"{label!r} as that device calls itself")

    def person_opens_browser(self, b: Box, res: Result) -> None:
        """B's person opens the Browser from their desk's list of apps, and it is there once
        its window stands on the desk and stays: a Browser that ends as it starts shows its
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
                res.facts["person_opened"] = b.vm.invoke("applications", BROWSER)
                break
            except VmError as exc:
                if "unknown_action" not in str(exc) or time.monotonic() >= deadline:
                    raise Failed(f"{b.name}'s person cannot open the Browser from the desk's apps: "
                                 f"{str(exc)[:600]}") from exc
                time.sleep(0.5)
        if not wait_until(open_windows, 90, 2.0):
            raise Failed("the Browser B's person opened never stood on B's desk")
        time.sleep(6.0)
        if not open_windows():
            raise Failed("the Browser B's person opened did not stay on B's desk")

    def two_machines(self, a: Box, b: Box, res: Result, facts: dict) -> None:
        # B's person has the Browser open; the agent on A sends its tab to the form, and the
        # agent on B joins the tab.
        self.in_one_tab(b, [
            Party(a, facts["command"], ANSWERS_A, "Maria Elena",
                  ("Given names", "Family name", "Nationality", "Which country?"), leads=True),
            Party(b, self.door(b), ANSWERS_B, "P7K290XB4", ("Passport number", "Arrival"),
                  remote=False),
        ], SITE_URL, res)

    def third_device(self, a: Box, d: Box, res: Result) -> None:
        """D: A branched once more, so a third device of the person, on the boxes' network."""
        # Only made, as B was: D meets A on the boxes' network, where this step starts both.
        out = a.vm.vm_cmd("branch", a.name, d.name, "--stop", "--no-admit", timeout=900)
        res.facts["branch"] = out[-1200:]
        self.running.discard("a")
        for box in (a, d):
            self.plug_lan(box)
        # D wakes holding the address A was given by hand, so it is up and given its own before
        # A is started again.
        d.vm.vm_cmd("start", d.name, timeout=900)
        self.running.add("d")
        self.wait_session(d, 600)
        res.facts["lan_d"] = self.address_lan(d)
        a.vm.vm_cmd("start", a.name, timeout=900)
        self.running.add("a")
        self.wait_session(a, 600)
        res.facts["lan_a"] = self.address_lan(a)
        for box in (a, d):
            box.vm.run("slot", "restart", SLOT, timeout=300, slot=False)
            self.ready(box, 300)
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
        # step of its own asks: its agent comes through B's door by B's device id, which a door
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
            Party(b, self.door(b), THREE_B, "FX4410972", ("Passport number", "Arrival"),
                  remote=False),
        ], THREE_URL, res)

    def third_lists_second(self, d: Box, b: Box, res: Result) -> None:
        """Whether D, enrolled after B, knows B as one of the person's devices: what
        `tilo_devices` lists there, and so whether `--device <name>` could name B on D."""
        if not self.third_made:
            raise Failed("no third device was made")
        self.reachable(d, b, res)

    def window_watched(self, a: Box, b: Box, res: Result) -> None:
        """B's person shares the Browser's window with the workspace, naming it by its tab
        ("Share a window with the workspace"), and A, their other device, holds its picture in a
        tab of its own, as a window that runs on B: a share is the workspace's, so nobody asks for
        it on A. What A's desk lists under the window's own tab is A's own view of it, opened on
        A, and is left as it is; what B shares is what runs on B, which is what a person
        reviewing the tab from another machine has to see."""
        def desk(box: Box) -> dict:
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
            """The rows `box`'s desk lists for `query`, as a person's menu lists them and as
            the desktop lists them for scripts. A desk with a page open lists thousands of
            rows, and they change while they are paged: a listing that cannot be read whole is
            left out and said, never taken for an empty one."""
            rows = []
            for read in (lambda: self.rows(box, query),
                         lambda: box.vm.actions(query, programmatic=True)):
                try:
                    rows += read()
                except VmError as exc:
                    unread = res.facts.setdefault("listings_unread", [])
                    if len(unread) < 6:
                        unread.append(f"{box.letter} {query!r}: {str(exc)[:120]}")
            return rows

        def row_of(box: Box, action: str, within: str) -> dict | None:
            # The narrow listings first: by what provides it, by its name, then everything.
            for query in (within, action, ""):
                for row in listed(box, query):
                    rid = str(row.get("id", ""))
                    if rid.split("\x1f")[-1] == action and within in rid + str(row.get("provider", "")):
                        return row
            return None

        def governed(state: dict) -> list:
            """The window shares the workspace's governance names on this desk."""
            held = (state.get("workspace_tree") or {}).get("workspaces") or []
            return [grant.get("id") for workspace in held
                    for grant in (workspace.get("policy") or {}).get("grants") or []
                    if "kind:window" in str(grant.get("node"))]

        def shares(state: dict, part: str) -> list:
            """What the session says of its window shares: `sharing` with each window's viewers,
            `starting`, `watching`, `received`, and `live`, the workspaces' live planes."""
            return (state.get("sharing") or {}).get(part) or []

        def plane(state: dict) -> dict | None:
            """The live plane of the workspace the window is shared in, as a session says it
            stands: `state` is `open`, or what it lacks, with `waiting_for` in words and `relay`
            the device it runs through or waits to reach. None for a session that does not
            publish its planes, which says nothing either way."""
            said = state.get("sharing") or {}
            if "live" not in said:
                return None
            return next((row for row in said.get("live") or [] if row.get("room") == room),
                        {"state": "absent",
                         "waiting_for": "its session names no live plane for the workspace"})

        self.person_opens_browser(b, res)
        on_b = desk(b)
        group, tab = browser_window(on_b)
        room = next((held.get("room") for held in on_b.get("held_workspaces") or []
                     if held.get("room")), None)
        res.facts.update(group=group, tab=tab, workspace=room)
        if not group or not tab or not room:
            raise Failed(f"B's desk does not hold the Browser's window in a workspace it shares: "
                         f"group {group}, tab {tab}, workspace {room}")
        # What A lists under the window's own tab before anything is shared: its own view of the
        # window, where the desk's record brought it. The step holds whether it is there or not.
        res.facts["own_view_on_a_before"] = next((held.get("tile") for _, held in tabs(desk(a))
                                                  if held.get("id") == tab), None)
        # The desk's own "Share a window" (`share-window`) is another thing: it asks Chat to put a
        # window's record in a channel. The window's picture goes to the workspace's devices
        # through the session's share, which takes the tab and finds the window itself.
        share = row_of(b, "share", "session:window-share")
        stop = row_of(b, "stop", "session:window-share")
        if share is None or stop is None:
            res.facts["share_rows"] = [str(row.get("id")) for row in listed(b, "share")
                                       if "share" in str(row.get("id"))][:40]
            raise Failed("B's session lists no action to share a window with the workspace, or none "
                         "to stop sharing it")
        res.facts["shared"] = b.vm.invoke(share["provider"], share["id"], {"tab": str(tab)})
        try:
            def held_on_a() -> list:
                for query in ("peer-surface", "Browser", ""):
                    rows = [row for row in listed(a, query)
                            if "desktop.peer-surface" in str(row.get("id")) + str(row.get("provider"))]
                    if rows:
                        return rows
                return []

            def pictured_on_a() -> list:
                rows = held_on_a()
                return rows if "Live" in json.dumps(rows, ensure_ascii=False) else []

            def hosted_on_a(state: dict) -> dict | None:
                """The tab of A's desk that holds B's window as one running on another device: the
                tab A's session says it pictures the window in (`shown_in`), which is a tab of its
                own wherever A lists its own view under the window's id."""
                watched = next((row for row in shares(state, "watching")
                                if str(row.get("tab")) == str(tab)), None) or {}
                shown_in = str(watched.get("shown_in") or tab)
                tile = next((held.get("tile") for _, held in tabs(state)
                             if str(held.get("id")) == shown_in), None)
                if isinstance(tile, dict) and "HostedWindow" in tile:
                    return {"tab": shown_in, "tile": tile}
                return None

            # The window's pictures travel the workspace's live plane, on a track of its own that
            # A's plane asks B's for while A's desk holds the window's tab: a tab of its own, made
            # when the share reached A's desk. Its row is there once the track's first keyframe
            # reached A, and says Live once its tile decoded it.
            rows = wait_until(pictured_on_a, 120, 2.0) or held_on_a()
            res.facts["peer_surface_rows"] = json.dumps(rows or [], ensure_ascii=False)[:1500]
            # How far the share came, on each end: the governance that names it, and what each
            # session says it shares, watches and received. `received` holds a window from its
            # first keyframe on; `viewers` are the devices that told B they view it, which is
            # what B names them and lets them in by, and no condition of the pictures.
            on_a, on_b = desk(a), desk(b)
            seen = {
                "governed_on_b": governed(on_b), "governed_on_a": governed(on_a),
                "sharing_on_b": shares(on_b, "sharing"), "starting_on_b": shares(on_b, "starting"),
                "watching_on_a": shares(on_a, "watching"), "received_on_a": shares(on_a, "received"),
                # What A lists under the window's own id, which is its own view of the window,
                # and the tab that holds the window B shares.
                "own_view_on_a": next((held.get("tile") for _, held in tabs(on_a)
                                       if held.get("id") == tab), None),
                "hosted_tab_on_a": hosted_on_a(on_a),
                # The pictures travel the workspace's live plane: how it stands on each end.
                "live_on_b": plane(on_b), "live_on_a": plane(on_a),
            }
            for name, value in seen.items():
                res.facts[name] = json.dumps(value, ensure_ascii=False)[:800]
            for box in (a, b):
                try:
                    box.vm.shot(self.out / f"shot-watch-{box.letter}.png")
                except VmError as exc:
                    res.facts[f"shot_{box.letter}"] = str(exc)[:200]
            if not rows or not seen["hosted_tab_on_a"]:
                res.facts["windows_on_a"] = a.vm.raw({"op": "windows"})
                viewers = [viewer for shared in seen["sharing_on_b"]
                           for viewer in shared.get("viewers") or []]
                # A plane that is not open carries nothing, whatever the shares say: the sharing
                # device's first, since nothing is captured without it.
                closed = next(((letter, stands) for letter, stands
                               in (("B", seen["live_on_b"]), ("A", seen["live_on_a"]))
                               if stands is not None and stands.get("state") != "open"), None)
                # Whether B's plane was asked for the window's track: a build that says so, says
                # `streaming` on the window's row.
                unasked = [shared for shared in seen["sharing_on_b"]
                           if str(shared.get("tab")) == str(tab) and shared.get("streaming") is False]
                # The first thing that did not happen, in the order a share travels.
                if seen["starting_on_b"] and not seen["sharing_on_b"]:
                    why = f"B's share waits to start: {res.facts['starting_on_b'][:300]}"
                elif not seen["governed_on_b"]:
                    why = "B's workspace does not say the window is shared"
                elif not seen["governed_on_a"]:
                    why = "the share B's workspace names never reached A's desk"
                elif not seen["watching_on_a"]:
                    why = ("A's desk reads the share and asks no device for the window: it does not "
                           "know which device hosts the tab")
                elif closed:
                    letter, stands = closed
                    relay = stands.get("relay_name") or stands.get("relay") or "no device"
                    why = (f"the workspace's live plane is not open on {letter} "
                           f"({stands.get('state')}, through {relay}): {stands.get('waiting_for')}")
                elif unasked and not seen["received_on_a"]:
                    why = ("the live plane is open on both and B's was never asked for the window's "
                           f"track, so B captures nothing: A's plane runs through "
                           f"{(seen['live_on_a'] or {}).get('relay')}, B's through "
                           f"{(seen['live_on_b'] or {}).get('relay')}; A watches "
                           f"{res.facts['watching_on_a'][:300]}")
                elif not seen["received_on_a"]:
                    heard = ("B names A as viewing it" if viewers
                             else "nor did A's word that it views it reach B")
                    why = ("A watches the window and no keyframe of its track reached A over the "
                           f"workspace's live plane ({heard}): {res.facts['watching_on_a'][:300]}")
                elif not seen["hosted_tab_on_a"]:
                    why = ("the window's pictures reached A and its desk holds no tab for the window "
                           f"that runs on B: A received {res.facts['received_on_a'][:300]}; under "
                           f"the window's own id A lists {res.facts['own_view_on_a'][:200]}")
                else:
                    why = ("the window's pictures reached A and the tab that holds the window "
                           f"pictures none of them: {res.facts['hosted_tab_on_a'][:300]}; A received "
                           f"{res.facts['received_on_a'][:300]}")
                raise Failed(f"A's desk never pictured the window B shares: {why}")
            if "Live" not in res.facts["peer_surface_rows"]:
                raise Failed("A's desk holds the shared window in a tab of its own and no picture of "
                             "it came: "
                             f"{res.facts['peer_surface_rows'][:400]}; A received: "
                             f"{res.facts['received_on_a'][:200]}; B shares: "
                             f"{res.facts['sharing_on_b'][:200]}")
        finally:
            # The share ends with the step: what follows finds B as it was.
            try:
                b.vm.invoke(stop["provider"], stop["id"], {"tab": str(tab)})
            except VmError as exc:
                res.facts["unshared"] = str(exc)[:200]

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
        self.plug_lan(c)
        c.vm.vm_cmd("start", c.name, timeout=900)
        self.running.add("c")
        self.make_person(c, res)
        res.facts["lan"] = self.address_lan(c)
        c.vm.run("slot", "restart", SLOT, timeout=300, slot=False)
        self.ready(c, 300)
        sc = self.identity(c)
        if (sc.get("identity") or {}).get("id") == (b.identity.get("identity") or {}).get("id"):
            raise Failed("C is the same person as B")
        key = (b.identity.get("this_device") or {}).get("device")
        self.put_agent(c)
        t = self.agent(c, "refused", ["tilo-runtime-mcp", "--connect", "--device", key], E2E_WAIT_S="60")
        res.facts["transcript"] = t
        init = t.get("initialize")
        if isinstance(init, dict) and "result" in init:
            raise Failed(f"B's door admitted another person's device: {init}")
        said = json.dumps(t)
        if "someone else" not in said and "refus" not in said.lower():
            raise Failed(f"C was not told B's door refused it: {said[:1500]}")
        if [s for s in self.seated(b) if s.get("seat") and "claude-code" in s.get("key", "")
                and (sc.get("this_device") or {}).get("device", "")[:8] in s.get("key", "")]:
            raise Failed("B seats C's agent")


def _found(value, key: str) -> list:
    """Every list held under `key` anywhere in `value`: what an action answered is wrapped
    differently by each door that carries it."""
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
    ap.add_argument("--taps", required=True, help="the tap devices of boxes a, b, c and d")
    ap.add_argument("--hold", type=int, default=0, help="keep the boxes up this long after the run")
    return Run(ap.parse_args()).main()


if __name__ == "__main__":
    sys.exit(main())

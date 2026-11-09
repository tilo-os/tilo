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
6. C, another person's device, is refused at B's door.

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
# A and B run side by side; C runs beside B once A has stopped. A box takes 3 GiB: with less its
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
# How long the person is not there to answer B's sheet.
UNANSWERED_S = 45


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


class Run:
    def __init__(self, args):
        self.args = args
        self.out: Path = args.out
        self.out.mkdir(parents=True, exist_ok=True)
        self.results: list[Result] = []
        self.log = self.out / "tilo-vm.log"
        taps = args.taps.split(",")
        self.running: set[str] = set()
        self.boxes = {
            letter: Box(letter, TiloVm(args.tilo_vm, f"e2e-{letter}", SLOT, self.log), tap,
                        f"52:54:00:e2:e0:0{n}", LAN.format(n))
            for n, (letter, tap) in enumerate(zip("abc", taps), start=1)
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
        try:
            self.ready(box, timeout)
        except (VmError, Failed):
            # A slot whose session did not come up with the box: start it as tilo-vm does.
            box.vm.run("slot", "restart", SLOT, timeout=300, slot=False)
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
        out = box.vm.exec(f'{line} python3 "$HOME/{GUEST_DIR}/agent.py" {scenario}', timeout=timeout)
        for text in reversed(out.splitlines()):
            if text.startswith(MARK):
                transcript = json.loads(text[len(MARK):])
                self.write(f"agent-{box.letter}-{scenario}-{int(time.time())}.json", transcript)
                if transcript.get("client_error"):
                    raise Failed(f"the scripted agent failed: {transcript['client_error']}")
                return transcript
        raise Failed(f"the scripted agent printed no transcript: {out[-600:]!r}")

    def agent_beside(self, box: Box, scenario: str, command: list[str], **env: str):
        """Run the agent on `box` in a thread; the box keeps what it said in `box_`."""
        holder: dict = {}

        def run():
            try:
                holder["transcript"] = self.agent(box, scenario, command, **env)
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
        a, b, c = self.boxes["a"], self.boxes["b"], self.boxes["c"]
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
            self.step("E09", "A device of another person is refused at B's door",
                      lambda res: self.stranger(b, c, res), "E03")
        finally:
            ok = self.report()
            if not ok:
                for box in (a, b, c):
                    self.keep_logs(box)
            if self.args.hold:
                print(f"holding the boxes for {self.args.hold}s: t vm ssh, then as {Path.home().name}: "
                      f"tilo-vm vm list", flush=True)
                time.sleep(self.args.hold)
            for box in (a, b, c):
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
        out = a.vm.vm_cmd("branch", a.name, b.name, "--stop", timeout=900)
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
            raise Failed(f"A's tilo_devices does not list B ({key}) as reachable after {FIND_S}s: {text!r}")

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
        if title not in text_of(t.get("found_note")):
            raise Failed(f"the note the agent made is not found on B: {t.get('new_note')} / {t.get('found_note')}")

    def stranger(self, b: Box, c: Box, res: Result) -> None:
        # C runs beside B in A's place.
        self.running.discard("a")
        self.boxes["a"].vm.vm_cmd("stop", self.boxes["a"].name, timeout=300, check=False)
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
    ap.add_argument("--taps", required=True, help="the tap devices of boxes a, b and c")
    ap.add_argument("--hold", type=int, default=0, help="keep the boxes up this long after the run")
    return Run(ap.parse_args()).main()


if __name__ == "__main__":
    sys.exit(main())

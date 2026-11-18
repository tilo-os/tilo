#!/usr/bin/env python3
"""Live eval of the agent: ask it what a person asks, with the person's real model, and judge
what it did, beside the same model driving the same apps through the `tilo` MCP tools.

There is one agent; the canvas and Chat are only places that ask it. So the eval asks
it directly, headless: each ask is a message the person sends to the agent they keep
(`session:send-to#send`, the one send a send-to box, the canvas and a script run), and it lands
in their direct conversation with the agent, which answers it as a turn. For each ask it records

* the model calls and their prompt tokens and time to first byte (the session's model gateway
  log, one `timing plan` line per call) and the wall seconds from the send to the turn's end
  (the agent's "the turn ended" line in the session log),
* the turn's final words and its step lines (the agent's "turn step" lines),
* the data the ask should have changed, read by a tilolang program the eval's own agent key
  runs (the session's `tilo` tool),

and judges each against its bar (`ASKS`, `BARS`): the right effect in the data, an honest final
message, step lines naming the right subjects, the time kept, no app window opened, and the
calls and seconds a person waits.

`--path mcp` asks the same asks of the same model as an external agent through the session's
MCP server (`tilo_find`, `tilo_read`, `tilo_do` with batched calls, `tilo_ls`), admitted by the
agent key the session was started with: the traditional path, for comparison. `--compare DIR`
puts another run's numbers beside this run's in the summary.

`--canvas-check` ends the run with one ask typed on the canvas, the way a person asks there, to
show it is the same agent answering in the same number of calls.

Everything goes to the results directory: `summary.md` (the table), `results.json`, and each
ask's session and gateway log lines.

Usage:
  dist/dev/agent-eval.py [--model-record FILE] [--results DIR] [--path agent|mcp]
                         [--no-build] [--profile P] [--attach RUNLOG] [--only NAME[,NAME…]]
                         [--compare DIR] [--canvas-check] [--keep]

Without `--attach` it starts its own `dist/dev/shell-run.sh` session (headless, lavapipe, never a
window, never this machine's desktop) and ends it with the run (SIGTERM to the pid shell-run
prints); `--attach RUNLOG` uses a session another shell-run started, from what it printed.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import signal
import socket
import subprocess
import sys
import time
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

REPO = Path(__file__).resolve().parents[2]

# What reads as the agent giving up or claiming it did nothing.
DISHONEST = [
    "without doing anything",
    "did nothing",
    "request refused",
    "without an answer",
]

# The agent the eval keeps in the workspace, made by the first send.
AGENT_NAME = "Helper"


@dataclass
class Ask:
    name: str
    text: str
    # greeting: 1 call, 3 s. simple: 1 call, 15 s. any: 1 call. summary: 2 calls (`BARS`).
    kind: str = "simple"
    # A tilolang program whose answer shows the effect, and the judge of that answer.
    program: str | None = None
    effect: Callable[[str], bool] | None = None
    # What the final words must say (lower-cased substrings, any of each group).
    reply_has: list[list[str]] = field(default_factory=list)
    # What at least one step line must name (lower-cased substrings, all of them).
    step_has: list[str] = field(default_factory=list)
    # A program run before the ask, to set up what it changes (and so what it is judged on).
    before: str | None = None
    # A program run after the ask is judged, to put things back for the next one.
    after: str | None = None


# The canvas: how the person in these asks shows their workspace.
FRAMES = 'shell.show_style(shell.viewing, style: "so.tilo.shell:frames")'


def tomorrow(today: dt.date) -> str:
    return (today + dt.timedelta(days=1)).isoformat()


def next_friday(today: dt.date) -> str:
    days = (4 - today.weekday()) % 7 or 7
    return (today + dt.timedelta(days=days)).isoformat()


def lines_with(answer: str, word: str) -> str:
    return "\n".join(line for line in answer.splitlines() if word.lower() in line.lower())


def by_reference(answer: str) -> dict[str, str]:
    """A program's answer's lines by the reference that starts them (`n7   body ¶1  …`), lower-cased:
    what it shows of each record."""
    out: dict[str, str] = {}
    for line in answer.splitlines():
        ref, _, rest = line.strip().partition(" ")
        if re.fullmatch(r"[a-z]+\d+", ref):
            out[ref] = out.get(ref, "") + " " + rest.lower()
    return out


MUM = (
    'for t in find(tasks.Task).filter(t -> t.title.lower().contains("mum")) '
    "{ print(t.title, t.due, t.at, t.remind) }"
)


def at_local(answer: str, day: dt.date, hour: int) -> bool:
    """Whether `answer` names `hour`:00 on `day` where the person is, as a moment: written as
    their time of day (no zone), or as an instant in UTC or at an offset, which is converted."""
    for m in re.finditer(r"\d{4}-\d\d-\d\dT\d\d:\d\d(?::\d\d)?(?:Z|[+-]\d\d:\d\d)?", answer):
        text = m.group(0).replace("Z", "+00:00")
        try:
            moment = dt.datetime.fromisoformat(text)
        except ValueError:
            continue
        moment = moment.astimezone() if moment.tzinfo else moment
        if moment.date() == day and moment.hour == hour and moment.minute == 0:
            return True
    return False


def asks(today: dt.date) -> list[Ask]:
    mum = lambda a: lines_with(a, "mum")  # noqa: E731
    return [
        Ask("hi", "hi", kind="greeting"),
        Ask(
            "capabilities",
            "what can you do?",
            kind="greeting",
            reply_has=[["task", "mail", "note"]],
        ),
        Ask(
            "remind",
            "remind me to call mum tomorrow at 6pm",
            program=MUM,
            effect=lambda a: tomorrow(today) in mum(a)
            and at_local(mum(a), today + dt.timedelta(days=1), 18),
            reply_has=[["mum"], ["6", "18"]],
            step_has=["mum"],
        ),
        Ask(
            "plate",
            "what's on my plate today?",
            reply_has=[["haircut", "boiler", "landlord", "oat milk", "mum"]],
        ),
        Ask("mails", "show me my mails", reply_has=[["ada", "bo", "grace", "mail", "inbox"]]),
        Ask("ada", "any mail from Ada?", reply_has=[["ada"]]),
        Ask(
            "reply-bo",
            "reply to Bo that Friday works",
            kind="any",
            program="for d in find(mail.Draft) { print(d.to, d.subject, d.body) }",
            effect=lambda a: "friday" in a.lower() and "bo" in a.lower(),
            reply_has=[["bo"]],
        ),
        Ask(
            "groceries",
            "make a note with the groceries for the weekend: eggs, milk, bread",
            program="for n in find(notes.Note, changed: today) { print(n.title, read(n)) }",
            effect=lambda a: any(
                all(item in text for item in ("eggs", "milk", "bread"))
                for text in by_reference(a).values()
            ),
            reply_has=[["grocer", "note"]],
            step_has=["grocer"],
        ),
        Ask(
            "haircut",
            "mark book a haircut as done",
            program='for t in find(tasks.Task).filter(t -> t.title == "Book a haircut") '
            "{ print(t.title, t.done) }",
            effect=lambda a: lines_with(a, "haircut").strip().endswith(("1", "true")),
            reply_has=[["haircut"]],
            step_has=["haircut"],
        ),
        Ask("beside", "put my notes next to the inbox", kind="simple"),
        Ask("summary", "summarise what changed in my notes this week", kind="summary",
            reply_has=[["grocer", "note"]]),
        Ask(
            "strips",
            "switch to strips",
            program="shell.viewing.style",
            effect=lambda a: "strips" in a,
            before=FRAMES,
            after=FRAMES,
        ),
        Ask(
            "move",
            "move the reminder to friday",
            program=MUM,
            effect=lambda a: next_friday(today) in mum(a),
            reply_has=[["friday", "fri"]],
            step_has=["mum"],
        ),
        Ask(
            "before",
            "what did I ask you before?",
            reply_has=[["remind", "mum", "haircut", "grocer", "strips", "friday"]],
        ),
        Ask(
            "follow-up",
            "and which of those did you finish?",
            kind="any",
            reply_has=[["remind", "mum", "haircut", "grocer", "strips", "note", "all"]],
        ),
    ]


# The most calls and seconds each kind of ask may take: one call for every ask, the person's bar
# (a second only when a step of the first failed, `FAILED_STEP`), a greeting within 3 s. A
# summary is two: the model cannot summarise what it has not read, so it reads, then writes.
BARS = {"greeting": (1, 3.0), "simple": (1, 15.0), "any": (1, None), "summary": (2, None)}

# A step that did not do what it was sent to: the one reason a second call is in the bar.
FAILED_STEP = re.compile(r"\[(refused|failed)\]$")

ESCAPES = re.compile(r"(\x1b|\\x1b)\[[0-9;]*[A-Za-z]")


def plain(text: str) -> str:
    return ESCAPES.sub("", text)


def fields(line: str) -> dict[str, str]:
    """The `key=value` fields of a tracing line (values quoted or bare)."""
    out = {}
    for m in re.finditer(r'(\w+)=("(?:[^"\\]|\\.)*"|\S+)', line):
        value = m.group(2)
        if value.startswith('"'):
            try:
                value = json.loads(value)
            except json.JSONDecodeError:
                value = value[1:-1]
        out[m.group(1)] = value
    return out


STAMP = re.compile(r"^\d{4}-\d\d-\d\dT")


def turn_id(line: str) -> int:
    m = re.search(r"the turn ended id=(\d+)", line)
    return int(m.group(1)) if m else -1


def ended_in(text: str, after: int = -1) -> dict | None:
    """How the turn ended, from the agent's "the turn ended" record in `text`: its state and the
    words the person reads (logged as `reply="…"`; older builds logged them as the event's own
    text, which runs on over the lines after it when the reply has several). Only a turn whose
    id is past `after` counts: an earlier turn that a new message cancelled ends in the same log
    window, and is not this ask's."""
    lines = text.splitlines()
    at = [
        i
        for i, line in enumerate(lines)
        if "the turn ended" in line and turn_id(line) > after
    ]
    if not at:
        return None
    first = at[-1]
    record = [lines[first]]
    for line in lines[first + 1 :]:
        if STAMP.match(line):
            break
        record.append(line)
    whole = "\n".join(record)
    got = fields(lines[first])
    got["id"] = str(turn_id(lines[first]))
    words = got.get("reply")
    if words is None:
        m = re.search(r"the turn ended .*?steps=\d+ (.*?) app=so\.tilo\.agent", whole, re.S)
        words = m.group(1) if m else ""
    return {
        "state": got.get("state", ""),
        "message": words,
        "steps": got.get("steps"),
        "id": int(got["id"]),
    }


class Session:
    def __init__(self, args: argparse.Namespace, results: Path):
        self.results = results
        self.bin = REPO / "target" / args.profile
        self.proc = None
        if args.attach:
            text = Path(args.attach).read_text(errors="replace")
        else:
            cmd = [
                str(REPO / "dist/dev/shell-run.sh"),
                "--size",
                args.size,
                "--scale",
                args.scale,
                "--profile",
                args.profile,
                "--agent-key",
                str(results / "agent.key"),
            ]
            if args.no_build:
                cmd.insert(1, "--no-build")
            out = open(results / "shell-run.log", "w")
            self.proc = subprocess.Popen(
                cmd, stdout=out, stderr=subprocess.STDOUT, start_new_session=True, cwd=REPO
            )
            print(f"shell-run pid {self.proc.pid}", flush=True)
            deadline = time.time() + 3600
            while time.time() < deadline:
                text = (results / "shell-run.log").read_text(errors="replace")
                if "The session runs (pid" in text:
                    break
                if self.proc.poll() is not None:
                    raise SystemExit(f"shell-run ended:\n{text[-3000:]}")
                time.sleep(1)
            else:
                raise SystemExit("the session did not start")
        self.api = re.search(r"^TILO_MCP_SERVER=(\S+)", text, re.M).group(1)
        self.control = re.search(r"^TILO_TEST_CONTROL=(\S+)", text, re.M).group(1)
        self.log = Path(re.search(r"\(log: (\S+session\.log)\)", text).group(1))
        self.gateway_log = self.log.parent / "model-gateway.log"
        self.env = dict(os.environ, TILO_TEST_CONTROL=self.control)
        # The last turn this eval saw end: a later ask counts only a later turn's end.
        self.last_turn = -1
        # Whether the last ask's turn never ended within the bound.
        self.unended = False
        # The eval's own connection to the session's MCP server, which reads and sets up the data.
        self.checker: "Mcp | None" = None
        # An attached session whose agent answered already keeps the agent the eval made.
        self.agent_made = self.log.exists() and "answering the conversation" in self.log.read_text(
            errors="replace"
        )
        key = re.search(r"--agent-key (\S+)", text)
        self.agent_key = Path(args.agent_key) if args.agent_key else (
            results / "agent.key" if self.proc else (Path(key.group(1)) if key else None)
        )

    def stop(self) -> None:
        if self.proc is not None and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=60)
            except subprocess.TimeoutExpired:
                os.killpg(self.proc.pid, signal.SIGKILL)

    def control_cmd(self, *args: str) -> str:
        done = subprocess.run(
            [str(self.bin / "tilo-control"), *args],
            env=self.env,
            capture_output=True,
            text=True,
            timeout=60,
        )
        return done.stdout + done.stderr

    def snapshot(self, path: Path) -> None:
        self.control_cmd("snapshot", str(path))

    def program(self, program: str) -> str:
        """Run `program` with the session's `tilo` tool as the eval's own agent (the agent key the
        session was started with): its answer's text."""
        if self.checker is None:
            self.checker = Mcp(self.api, self.agent_key.read_text().strip(), "Eval checker")
        return self.checker.tool("tilo", {"program": program, "confirm": True})

    def send(self, text: str) -> str:
        """Send `text` to the agent the person keeps, as a send-to box does."""
        to = {"kind": "agent", "name": AGENT_NAME} if self.agent_made else {
            "kind": "new-agent",
            "name": AGENT_NAME,
        }
        self.agent_made = True
        return self.control_cmd(
            "actions",
            "invoke",
            "so.tilo.session",
            "session:send-to\x1fsend",
            "--args-json",
            json.dumps({"to": [to], "text": text}),
        )

    def size(self, path: Path) -> int:
        return path.stat().st_size if path.exists() else 0

    def read_since(self, path: Path, at: int) -> str:
        if not path.exists():
            return ""
        with open(path, "rb") as f:
            f.seek(at)
            return plain(f.read().decode(errors="replace"))


def number(value) -> int:
    """A gateway timing field as a number: `-` (not measured) reads as 0."""
    return int(value) if isinstance(value, str) and value.isdigit() else 0


def step_of(line: str) -> dict:
    """A step line ("turn step"): its label is the words up to its outcome, spaces and all."""
    got = fields(line)
    m = re.search(r"label=(.*?) outcome=", line)
    if m:
        got["label"] = m.group(1).strip('"')
    return got


def delivered_ms(text: str, sent: float) -> int | None:
    """How long the message took from the send to the agent: the agent's first "answering the
    conversation's turn" line after it, by its own timestamp, less the wall time of the send."""
    for line in text.splitlines():
        if "answering the conversation's turn" not in line:
            continue
        m = re.match(r"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+)Z", line)
        if m:
            at = dt.datetime.fromisoformat(m.group(1)).replace(tzinfo=dt.timezone.utc)
            return round((at.timestamp() - sent) * 1000)
    return None


def gateway_calls(text: str) -> list[dict]:
    return [fields(line) for line in text.splitlines() if "timing plan" in line]


def ask_agent(s: Session, ask: Ask, out: Path, timeout: float) -> dict:
    log_at, gw_at = s.size(s.log), s.size(s.gateway_log)
    began = time.time()
    sent = s.send(ask.text)
    ended = None
    while time.time() - began < timeout:
        seen = ended_in(s.read_since(s.log, log_at), s.last_turn)
        if seen is not None and s.unended and seen["state"] == "cancelled":
            # The earlier ask's turn, stopped by this message.
            s.last_turn = seen["id"]
            s.unended = False
            continue
        if seen is not None:
            break
        time.sleep(0.1)
    seconds = time.time() - began
    time.sleep(0.3)
    session_text = s.read_since(s.log, log_at)
    ended = ended_in(session_text, s.last_turn)
    if ended is not None:
        s.last_turn = ended["id"]
    else:
        # A turn that never ended within the bound is stopped by the next message: the first
        # turn to end after it is that one, cancelled, and is not the next ask's.
        s.unended = True
    time.sleep(0.3)
    gateway_text = s.read_since(s.gateway_log, gw_at)
    delivered = delivered_ms(session_text, began)
    (out / f"{ask.name}.session.log").write_text(session_text)
    (out / f"{ask.name}.gateway.log").write_text(gateway_text)
    calls = gateway_calls(gateway_text)
    tools = [fields(line) for line in session_text.splitlines() if "agent tool call" in line]
    steps = [step_of(line) for line in session_text.splitlines() if "turn step" in line]
    return {
        "sent": sent.strip()[:300],
        "ended": ended,
        "seconds": round(seconds, 1),
        "delivered_ms": delivered,
        "calls": len(calls),
        "prompt_tokens": [number(c.get("prompt_tokens")) for c in calls],
        "first_byte_ms": [number(c.get("first_byte_ms")) for c in calls],
        "call_ms": [number(c.get("done_ms")) for c in calls],
        "tools": [f"{t.get('tool')} {str(t.get('args', ''))[:300]}" for t in tools],
        "steps": [f"{st.get('label')} [{st.get('outcome')}]" for st in steps],
    }


class Mcp:
    """The session's MCP server as an external agent (Claude Code, any MCP client) reaches it:
    newline-delimited MCP JSON-RPC on its socket, admitted as the person's agent by the agent key
    the session was started with (`shell-run.sh --agent-key`)."""

    def __init__(self, path: str, key: str, name: str = "Eval agent"):
        self.path = path
        self.key = key
        self.name = name
        self.connect()

    def connect(self) -> None:
        """Connect and be admitted: again after a call that broke the connection (one that
        outlived its timeout, a step waiting on the person), as a client reconnects."""
        path, key = self.path, self.key
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.file = self.sock.makefile("rwb")
        self.next = 1
        self.call(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "clientInfo": {"name": self.name, "version": "1"},
                "capabilities": {"experimental": {"so.tilo": {"key": key}}},
            },
            timeout=120,
        )
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        self.tools = self.call("tools/list", {})["tools"]

    def send(self, message: dict) -> None:
        self.file.write(json.dumps(message).encode() + b"\n")
        self.file.flush()

    def call(self, method: str, params: dict, timeout: float = 120) -> dict:
        ident = self.next
        self.next += 1
        self.sock.settimeout(timeout)
        self.send({"jsonrpc": "2.0", "id": ident, "method": method, "params": params})
        while True:
            line = self.file.readline()
            if not line:
                raise RuntimeError("the MCP server closed the connection")
            message = json.loads(line)
            if message.get("id") == ident:
                if "error" in message:
                    raise RuntimeError(json.dumps(message["error"]))
                return message["result"]

    def tool(self, name: str, arguments: dict) -> str:
        try:
            result = self.call("tools/call", {"name": name, "arguments": arguments})
        except (RuntimeError, OSError) as error:
            # The call's answer never came: the connection is spent. The agent reads the error,
            # and its next call goes over a fresh connection.
            try:
                self.sock.close()
                self.connect()
            except (RuntimeError, OSError):
                pass
            return f"error: {error}"
        text = "\n".join(
            block.get("text", "")
            for block in result.get("content", [])
            if block.get("type") == "text"
        )
        return ("error: " + text) if result.get("isError") else text


MCP_SYSTEM = (
    "You are the person's assistant in Tilo, their workspace of apps (tasks, mail, notes, chat "
    "and more). Use the tilo tools to read and change their apps on their behalf, then answer "
    "them briefly. {clock}"
)


class McpAgent:
    """An external agent on the traditional path: the same model, driving the session's apps
    through the `tilo` MCP tools, as Claude Code does. The conversation is kept across asks, as
    one chat with the agent is."""

    def __init__(self, s: Session, record: dict):
        for _ in range(120):
            if s.agent_key and s.agent_key.exists() and s.agent_key.read_text().strip():
                break
            time.sleep(1)
        self.mcp = Mcp(s.api, s.agent_key.read_text().strip())
        profile = record["profile"]
        self.endpoint = profile["endpoint"].rstrip("/") + "/v1/messages"
        self.model = profile["default_model"]
        self.messages: list[dict] = []
        now = dt.datetime.now().astimezone()
        self.system = MCP_SYSTEM.format(
            clock=f"It is {now.strftime('%A %Y-%m-%d %H:%M')} where the person is "
            f"(UTC{now.strftime('%z')})."
        )
        self.tools = [
            {
                "name": t["name"],
                "description": t.get("description", ""),
                "input_schema": t["inputSchema"],
            }
            for t in self.mcp.tools
        ]

    def model_call(self) -> dict:
        body = json.dumps(
            {
                "model": self.model,
                "max_tokens": 4096,
                "system": self.system,
                "tools": self.tools,
                "messages": self.messages,
            }
        ).encode()
        request = urllib.request.Request(
            self.endpoint,
            data=body,
            headers={"content-type": "application/json", "anthropic-version": "2023-06-01"},
        )
        with urllib.request.urlopen(request, timeout=180) as response:
            return json.loads(response.read())

    def ask(self, text: str, most: int = 15) -> dict:
        self.messages.append({"role": "user", "content": text})
        began = time.time()
        calls, tools, said, tokens = 0, [], "", []
        while calls < most:
            response = self.model_call()
            calls += 1
            tokens.append(int(response.get("usage", {}).get("input_tokens", 0)))
            content = [
                block for block in response.get("content", []) if block.get("type") != "thinking"
            ]
            self.messages.append({"role": "assistant", "content": content or "…"})
            uses = [block for block in content if block.get("type") == "tool_use"]
            said = "\n".join(
                block.get("text", "") for block in content if block.get("type") == "text"
            )
            if not uses:
                break
            results = []
            for use in uses:
                tools.append(f"{use['name']} {json.dumps(use.get('input', {}))[:300]}")
                result = self.mcp.tool(use["name"], use.get("input", {}))
                results.append(
                    {"type": "tool_result", "tool_use_id": use["id"], "content": result[:20000]}
                )
            self.messages.append({"role": "user", "content": results})
        return {
            "ended": {"state": "succeeded" if said.strip() else "failed", "message": said},
            "seconds": round(time.time() - began, 1),
            "calls": calls,
            "prompt_tokens": tokens,
            "first_byte_ms": [],
            "call_ms": [],
            "tools": tools,
            "steps": [],
        }


def judge(ask: Ask, got: dict, data: str | None, path: str) -> list[str]:
    """What fails the ask's bar, in words; empty when it passes."""
    fails = []
    ended = got["ended"]
    if not ended:
        return ["never ended"]
    message = ended.get("message", "")
    if ended.get("state") != "succeeded":
        fails.append(f"ended {ended.get('state')}")
    if any(word in message.lower() for word in DISHONEST):
        fails.append("dishonest/failing words")
    for group in ask.reply_has:
        if not any(word in message.lower() for word in group):
            fails.append(f"reply lacks {group}")
    if path == "agent":
        for word in ask.step_has:
            if not any(word in step.lower() for step in got["steps"]):
                fails.append(f"no step names {word!r}")
        # An app's own window: its application's open (`app.<id>#open`) or the session's
        # `open_app`; placing a thing on the canvas (`shell:layout#open`, `#place`) is not one.
        if any(re.search(r"app\.[\w.-]+#open\b|open_app|open-app", t) for t in got["tools"]):
            fails.append("opened an app window")
    if ask.effect is not None and not (data and ask.effect(data)):
        fails.append("effect missing in data")
    most_calls, most_seconds = BARS[ask.kind]
    if ask.kind != "greeting" and any(FAILED_STEP.search(step) for step in got["steps"]):
        most_calls += 1
    if got["calls"] > most_calls:
        fails.append(f"{got['calls']} calls > {most_calls}")
    if most_seconds is not None and got["seconds"] > most_seconds:
        fails.append(f"{got['seconds']} s > {most_seconds}")
    return fails


def canvas_check(s: Session, results: Path, timeout: float) -> dict:
    """One ask typed on the canvas, as a person asks there: the same agent answers it."""
    s.program('shell.show_style(shell.viewing, style: "so.tilo.shell:frames")')
    time.sleep(3)
    log_at, gw_at = s.size(s.log), s.size(s.gateway_log)
    s.control_cmd("click-at", "360", "700")
    time.sleep(0.6)
    s.control_cmd("type", "what's on my plate today?")
    began = time.time()
    s.control_cmd("key", "Enter")
    ended = None
    while time.time() - began < timeout:
        if "the turn ended" in s.read_since(s.log, log_at):
            break
        time.sleep(0.1)
    seconds = round(time.time() - began, 1)
    time.sleep(0.3)
    ended = ended_in(s.read_since(s.log, log_at))
    time.sleep(2)
    s.snapshot(results / "canvas-check.png")
    calls = gateway_calls(s.read_since(s.gateway_log, gw_at))
    return {"ended": ended, "seconds": seconds, "calls": len(calls)}


def main() -> int:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    # The session's agent asks the person's model on their workspace's server, through this
    # computer's `tilo login` session; only the external agent of `--path mcp` asks a provider
    # itself, with this record's endpoint and model.
    p.add_argument("--model-record", default=None)
    p.add_argument("--results", default=None)
    p.add_argument("--path", choices=["agent", "mcp"], default="agent")
    p.add_argument("--no-build", action="store_true")
    p.add_argument("--profile", default="nested")
    p.add_argument("--attach", default=None, help="the output of a running shell-run.sh")
    p.add_argument("--agent-key", default=None, help="the attached session's agent key file")
    p.add_argument("--only", default=None)
    p.add_argument("--compare", default=None, help="another run's results dir, shown beside")
    p.add_argument("--canvas-check", action="store_true")
    p.add_argument("--keep", action="store_true")
    p.add_argument("--size", default="2880x1800", help="the headless output's size")
    p.add_argument("--scale", default="2", help="the headless output's scale")
    p.add_argument("--timeout", type=float, default=300)
    p.add_argument(
        "--rejudge",
        default=None,
        help="judge a finished run's results dir again from what it saved, with this harness",
    )
    p.add_argument("--today", default=None, help="the run's day, for --rejudge (YYYY-MM-DD)")
    args = p.parse_args()
    if args.rejudge:
        return rejudge(args)
    stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    results = Path(args.results or Path.home() / "work/agent-eval" / f"{stamp}-{args.path}")
    results = results.resolve()
    results.mkdir(parents=True, exist_ok=True)
    today = dt.date.today()
    chosen = asks(today)
    if args.only:
        names = args.only.split(",")
        chosen = [a for a in chosen if a.name in names]
    s = Session(args, results)
    rows = []
    canvas = None
    try:
        # The apps and the agent come up with the session.
        if s.proc is not None:
            time.sleep(15)
        s.program(FRAMES)
        external = None
        if args.path == "mcp":
            if args.model_record is None:
                raise SystemExit("--path mcp needs --model-record: the external agent's model")
            external = McpAgent(s, json.loads(Path(args.model_record).read_text()))
        for ask in chosen:
            print(f"asking {ask.name!r} ({args.path})", flush=True)
            if ask.before:
                s.program(ask.before)
                time.sleep(1)
            if external is not None:
                got = external.ask(ask.text)
            else:
                got = ask_agent(s, ask, results, args.timeout)
            data = s.program(ask.program) if ask.program else None
            if data is not None:
                (results / f"{ask.name}.data.txt").write_text(data)
            fails = judge(ask, got, data, args.path)
            rows.append({"name": ask.name, "text": ask.text, **got, "data": data, "fails": fails})
            print(
                f"  {got['calls']} calls, {got['seconds']} s, "
                f"delivered {got.get('delivered_ms')} ms, "
                f"{'PASS' if not fails else 'FAIL: ' + '; '.join(fails)}",
                flush=True,
            )
            if ask.after:
                s.program(ask.after)
        if args.canvas_check and args.path == "agent":
            canvas = canvas_check(s, results, args.timeout)
            print(f"canvas check: {canvas}", flush=True)
    finally:
        (results / "results.json").write_text(json.dumps(rows, indent=2))
        write_summary(args, results, rows, canvas)
        print(f"results: {results}", flush=True)
        if args.keep or args.attach:
            print(f"kept: TILO_TEST_CONTROL={s.control} api={s.api}")
        else:
            s.stop()
    return 0 if rows and all(not row["fails"] for row in rows) else 1


def rejudge(args) -> int:
    """Judge a finished run again from its saved logs and data: what a run measured stays as
    it was measured, and a fixed judge reads it the same way the live one would have."""
    results = Path(args.rejudge).resolve()
    rows = json.loads((results / "results.json").read_text())
    today = dt.date.fromisoformat(args.today) if args.today else dt.date.today()
    by_name = {a.name: a for a in asks(today)}
    for row in rows:
        ask = by_name.get(row["name"])
        if ask is None:
            continue
        log = results / f"{row['name']}.session.log"
        if args.path == "agent" and log.exists():
            text = log.read_text()
            row["ended"] = ended_in(text) or row["ended"]
            row["steps"] = [
                f"{st.get('label')} [{st.get('outcome')}]"
                for st in (step_of(line) for line in text.splitlines() if "turn step" in line)
            ]
        row["fails"] = judge(ask, row, row.get("data"), args.path)
    (results / "results.json").write_text(json.dumps(rows, indent=2))
    write_summary(args, results, rows, None)
    print((results / "summary.md").read_text().split("\n### ")[0])
    return 0


def beats(row: dict, other: dict) -> bool:
    """Whether this run's answer to an ask beats the other path's: it passes, in no more calls,
    and sooner, or the other failed it."""
    if row["fails"]:
        return False
    if other["fails"]:
        return True
    return row["calls"] <= other["calls"] and row["seconds"] < other["seconds"]


def write_summary(args, results: Path, rows: list[dict], canvas: dict | None) -> None:
    other = {}
    if args.compare:
        other = {
            r["name"]: r for r in json.loads((Path(args.compare) / "results.json").read_text())
        }
    head = "| ask | calls | s | pass | why |"
    rule = "|---|---|---|---|---|"
    if other:
        head += " other calls | other s | other pass | beats other |"
        rule += "---|---|---|---|"
    lines = [f"path: {args.path}" + (f"; other: {args.compare}" if other else ""), "", head, rule]
    for row in rows:
        line = (
            f"| {row['text']} | {row['calls']} | {row['seconds']} | "
            f"{'yes' if not row['fails'] else 'no'} | {'; '.join(row['fails'])} |"
        )
        if other:
            o = other.get(row["name"])
            line += (
                f" {o['calls']} | {o['seconds']} | {'yes' if not o['fails'] else 'no'} |"
                f" {'yes' if beats(row, o) else 'no'} |"
                if o
                else " | | | |"
            )
        lines.append(line)
    passed = sum(1 for row in rows if not row["fails"])
    lines.append(f"\n{passed}/{len(rows)} pass")
    if canvas:
        lines.append(f"\ncanvas check: {canvas}")
    for row in rows:
        lines.append(f"\n### {row['text']}\n")
        lines.append(f"ended: {row['ended']}\n")
        lines.append(f"prompt tokens: {row.get('prompt_tokens')}; first byte ms: "
                     f"{row.get('first_byte_ms')}; call ms: {row.get('call_ms')}; "
                     f"delivered ms: {row.get('delivered_ms')}\n")
        lines.append("steps:\n" + "\n".join(f"- {st}" for st in row["steps"]))
        lines.append("\ntools:\n" + "\n".join(f"- `{t}`" for t in row["tools"]))
        if row.get("data") is not None:
            lines.append(f"\ndata:\n```\n{row['data'][:1500]}\n```")
    (results / "summary.md").write_text("\n".join(lines) + "\n")


if __name__ == "__main__":
    sys.exit(main())

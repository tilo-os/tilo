#!/usr/bin/env python3
"""Connect an agent over MCP and have it do real work, in a Tilo shell.

Ada opens Files in her shell to watch. Her agent connects to the session's MCP server, the
person allows it on the sheet the shell puts up, and from then on the agent works through the
workspace's own tools: it writes a file, adds a Tasks issue, writes a note and posts in Chat, and
asks Ada a question, which she answers on her shell. Every claim is read off Ada's shell (its
windows, the controls it shows and their accessible names, the agents its state lists), never off
the agent's own reply.

    python3 agent-mcp.py OUT_DIR
"""

from __future__ import annotations

import base64
import json
import re
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from story import Failed, Person, Story, until, walk  # noqa: E402
from real_apps import Agent  # noqa: E402

SHELL = "so.tilo.session"
CHROME = "desktop:chrome"
APPS = "file-explorer tasks notes chat settings activity"
ACTIVITY = "so.tilo.activity"
# The session's history of a workspace, which Activity's History draws.
ACTIVITY_PROVIDER = "tilo.activity"
ACTIVITY_RESOURCE = "workspace.activity"
AGENT = "Claude Code"
FILE = "agent-notes.md"
BODY = b"# Notes from your agent\n\nThe spring budget is filed.\n"
TASK = "Check the spring budget"
NOTE = "Spring plans"
CHAT = "I filed the budget under Spring."
QUESTION = "Shall I file the budget under Spring?"
ANSWER = "Yes, file it"


class Working:
    """Ada's agent at her session's MCP server, and the person answering the sheets it raises."""

    def __init__(self, story: Story, ada: Person):
        self.story = story
        self.ada = ada
        self.agent: Agent | None = None
        self.said: list[dict] = []

    def allows(self) -> str:
        """The person's answer to a sheet asking about the agent: allowing it while it works here
        where the sheet offers that, else this once. The name pressed, or "" with no sheet."""
        allows = [name for name in self.ada.names() if name.lower().startswith("allow")]
        if not allows:
            return ""
        pressed = next((name for name in allows if "once" not in name.lower()), allows[0])
        self.ada.session.click_named(pressed)
        return pressed

    def connect(self) -> None:
        mcp = Path(self.ada.session.runtime_directory()) / "tilo-session/agent.sock"
        until("the session's MCP server", mcp.exists, 60)
        self.agent = Agent(mcp, AGENT)
        for _ in range(60):
            if self.agent.hears(1.0):
                break
            self.allows()
        if not self.agent.answered():
            raise Failed(f"the MCP server refused the agent: {self.agent.served}")
        self.agent.notify("notifications/initialized")

    def tools(self) -> list[str]:
        self.agent.send("tools/list", {})
        reply = self.agent.reply() or {}
        return [tool.get("name") for tool in (reply.get("result") or {}).get("tools") or []]

    def answer(self, tool: str, arguments: dict, deadline: float = 60.0) -> tuple[str, bool]:
        """(text, failed) of one tool call, the person answering any sheet it raises."""
        self.agent.send("tools/call", {"name": tool, "arguments": arguments})
        for _ in range(int(deadline)):
            if self.agent.hears(1.0):
                break
            self.allows()
        message = self.agent.reply() or {}
        result = message.get("result") or {}
        try:
            text = result["content"][0]["text"]
        except (KeyError, IndexError, TypeError):
            text = json.dumps(message)
        self.said.append({"tool": tool, "arguments": arguments, "answer": text[:4000]})
        return text, bool(result.get("isError")) or "result" not in message

    def call(self, tool: str, arguments: dict, deadline: float = 60.0) -> str:
        """One tool call's text, or Failed with what it answered. A call answered while its
        change is still on its way ("pending <id>: …") is followed to how it ended, as an agent
        asks `tilo_status`."""
        text, failed = self.answer(tool, arguments, deadline)
        until_end = time.monotonic() + deadline
        while (pending := re.match(r"pending ([^:\s]+)", text)) and time.monotonic() < until_end:
            time.sleep(0.25)
            self.allows()
            text, failed = self.answer("tilo_status", {"invocation_id": pending.group(1)})
        if failed or text.startswith("pending "):
            raise Failed(f"{tool} {json.dumps(arguments)[:200]} failed: {text[:800]}")
        return text

    def do(self, paths: list[str], args: dict) -> str:
        """The first of `paths` the workspace takes, as an agent tries what it lists."""
        refused = None
        for path in paths:
            try:
                return self.call("tilo_do", {"path": path, "args": args, "wait_ms": 30000})
            except Failed as error:
                if not re.search(r"has no action|no resource|unknown|no such|unavailable|not found",
                                 str(error), re.I):
                    raise
                refused = error
        raise refused


def steps(story: Story) -> None:
    ada = story.person("Ada")
    work = Working(story, ada)
    ada.prepare(APPS)

    def ada_opens():
        took = ada.start(apps=APPS)
        story.timing("Ada's session start, to its first frame", took["tookMs"])
        story.timing("Ada's session start, to its first frame, with the development launcher's installing", took["launcherMs"])
        # Her own changes are under the name she gives, as History shows them beside the agent's.
        ada.name_self("Ada")
        return {"tookMs": took["tookMs"]}

    story.step("Ada opens Tilo and gives her name", ada_opens)

    def files():
        ada.session.launch("file-explorer")
        ada.session.answering("file-explorer", deadline=120)
        until("the folder Files opened on", lambda: ada.shows("New folder"), 30, 0.25)
        story.note("files-controls", ada.names())

    story.step("Ada opens Files to watch her agent work", files)

    def connect():
        work.connect()
        tools = work.tools()
        story.note("agent-tools", tools)

        def listed():
            agents = (ada.session.state_dump() or {}).get("agents") or []
            return next((a for a in agents if AGENT in (a.get("name") or "")), None)

        story.note("agent-listed", until(f"{AGENT} among the agents Ada's shell lists", listed, 30))
        if "tilo_do" not in tools:
            raise Failed(f"the MCP server offers no tilo_do: {tools}")

    story.step("Ada connects their agent to the workspace", connect)

    def writes_file():
        # The folder Ada's Files window shows: Files' own write, the one it offers agents.
        try:
            work.do(["files#write-file-here"], {"name": FILE, "text": BODY.decode()})
        except Failed as refused:
            # Where Files offers no write here, the workspace's put: the content, then its name.
            stored = work.call("tilo_put_many", {"files": [base64.b64encode(BODY).decode()]})
            content = re.search(r"tilo\.content:[0-9a-f]+", stored)
            if not content:
                raise Failed(f"Files' write was refused ({refused}) and the upload answered no "
                             f"content reference: {stored[:300]}") from refused
            work.do(["files#open"], {"content": content.group(), "name": FILE})
        try:
            until(f"{FILE} in Ada's Files window", lambda: ada.shows(FILE), 30, 0.25)
        finally:
            story.note("files-after-write", ada.names())

    story.step("The agent writes a file", writes_file)

    def opened(app: str, text: str, what: str) -> None:
        ada.session.launch(app)
        try:
            until(what, lambda: ada.shows(text), 30, 0.25)
        finally:
            story.note(f"{app}-controls", ada.names())

    def task():
        work.do(["tasks.issues#create", "tasks#create"], {"title": TASK})
        opened("tasks", TASK, f"the issue “{TASK}” in Ada's Tasks")

    story.step("The agent adds a Tasks issue", task)

    def note():
        work.do(["notes#new-note"], {"title": NOTE})
        work.do(["notes#append-to-note"], {"title": NOTE, "markdown": "- file the budget\n- book the trip\n"})
        opened("notes", NOTE, f"the note “{NOTE}” in Ada's Notes")

    story.step("The agent writes a note", note)

    def chat():
        work.do(["chat.workspace#begin-chat", "chat#begin-chat"], {})
        read = work.call("tilo_read", {"path": "chat.workspace"})
        channels = [c for c in dict.fromkeys(re.findall(r"\bc_[a-z0-9_-]+\b", read)) if c != "c_voice"]
        work.do(["chat.messages#post", "chat#post"], {"channel": channels[0] if channels else "c_chat",
                                                      "text": CHAT})
        opened("chat", CHAT, "the message in Ada's Chat")

    story.step("The agent posts in Chat", chat)

    def under_its_name():
        # The shell's own history of the workspace: every change, each under who made it, in the
        # Activity window the shell opens on its explore view.
        tree = (ada.session.state_dump() or {}).get("workspace_tree") or {}
        ada.session.invoke(SHELL, f"{CHROME}\u001factivity",
                           {"workspace": tree.get("active"), "view": "explore"})
        until("the Activity window on Ada's shell", lambda: next(
            (w for w in ada.session.windows()
             if w.get("app_id") == ACTIVITY and w.get("rect")), None), 60, 0.25)
        # Its History lists the workspace's changes. A row of that list is no control the shell
        # names, so what it lists is read from the feed the window draws: the session's
        # `tilo.activity` history of the workspace, each change with its author.
        # Each of the agent's changes, as History words it: what was done, naming the item it
        # changed (the file's name, the issue's title, the note's title, the message's words). A
        # row that names no item is said in the failure.
        changes = {f"the file {FILE}": ("write file", FILE),
                   f"the issue “{TASK}”": ("create", TASK),
                   f"the note “{NOTE}”": ("new note", NOTE),
                   "the message": ("post", CHAT)}

        def feed() -> list[dict]:
            answer = ada.session.invoke(ACTIVITY_PROVIDER, f"{ACTIVITY_RESOURCE}\u001ffeed",
                                        {"scope": "this-workspace", "limit": 0})
            # A read answers its value as the item it streams.
            update = (answer.get("receipt") or {}).get("update") or {}
            value = update.get("value") or (update.get("output") or {}).get("value")
            if not isinstance(value, dict) or "entries" not in value:
                story.note("history-feed-answer", answer)
                raise Failed(f"the workspace's history answered no feed: {json.dumps(answer)[:600]}")
            return value["entries"]

        def about(entry: dict, change: tuple[str, str]) -> bool:
            done, item = change
            summary = str(entry.get("summary") or "").lower()
            return summary.startswith(done) and item.lower() in summary

        def listed():
            entries = feed()
            found = {what: [e for e in entries if about(e, change)] for what, change in changes.items()}
            return found if all(found.values()) else None

        try:
            found = until("History listing the agent's file, issue, note and message", listed, 30, 0.5)
        except Failed as missing:
            entries = feed()
            story.note("history-feed", [{k: e.get(k) for k in ("summary", "resource", "app", "author")}
                                        for e in entries])
            raise Failed(f"{missing}; it lists {len(entries)} changes") from None
        else:
            entries = feed()
            story.note("history-feed", [{k: e.get(k) for k in ("summary", "resource", "app", "author")}
                                        for e in entries])
        # What History must say, all of it checked so the step names every fault it shows.
        wrong = []
        # Ada's own changes are under her name: she is the only person in the workspace, so a
        # person's change under no name is hers.
        nameless = [e for e in entries if (e.get("author") or {}).get("relation") != "software"
                    and (e.get("author") or {}).get("name") in ("Unknown member", "", None)]
        if nameless:
            said = sorted({e.get("summary") or "" for e in nameless})
            wrong.append(f"{len(nameless)} of Ada's own changes show as “Unknown member”, not "
                         f"under her name ({'; '.join(said)})")
        # Each of the agent's changes is under the agent's own name.
        names = {}
        for what, rows in found.items():
            if not any(AGENT == ((row.get("author") or {}).get("name") or "") for row in rows):
                names[what] = sorted({(row.get("author") or {}).get("name") or "nobody"
                                      for row in rows})
        if names:
            wrong.append(f"the agent's changes are under {', '.join(sorted({n for v in names.values() for n in v}))}, "
                         f"a name it never gave, not under “{AGENT}”, the name Ada's top bar shows "
                         f"(on {', '.join(names)})")
        # Each row names the item it changed, as a person needs to tell the rows apart.
        items = {f"the file {FILE}": FILE, f"the issue “{TASK}”": TASK, f"the note “{NOTE}”": NOTE,
                 "the message": CHAT}
        unsaid = {what: found[what][0].get("summary") for what, item in items.items()
                  if not any(item.lower() in (row.get("summary") or "").lower()
                             for row in found[what])}
        if unsaid:
            wrong.append("no row names the item it changed: " + "; ".join(
                f"{what} reads “{summary}”" for what, summary in unsaid.items()))
        if wrong:
            raise Failed("History: " + ". ".join(wrong))

    story.step("Ada sees each change under the agent’s name", under_its_name)

    def asks():
        answer: dict = {}

        def ask():
            try:
                answer["text"] = work.call("tilo_ask", {"question": QUESTION,
                                                        "choices": [ANSWER, "Leave it for now"]}, 90)
            except Failed as error:
                answer["error"] = str(error)

        asking = threading.Thread(target=ask, daemon=True)
        asking.start()
        until("the agent's question on Ada's shell", lambda: ada.shows(QUESTION), 30, 0.25)
        ada.press(ANSWER, "the answer Ada picks")
        asking.join(60)
        if "text" not in answer:
            raise Failed(f"the agent was not told her answer: {answer.get('error', 'no answer')}")
        if ANSWER not in answer["text"]:
            raise Failed(f"the agent was told {answer['text']!r}, not her answer")

    try:
        story.step("The agent asks Ada a question, and Ada answers it", asks)
    finally:
        story.note("agent-calls", work.said)


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: agent-mcp.py OUT_DIR", file=sys.stderr)
        return 2
    return walk("agent-mcp", Path(sys.argv[1]), steps)


if __name__ == "__main__":
    sys.exit(main())

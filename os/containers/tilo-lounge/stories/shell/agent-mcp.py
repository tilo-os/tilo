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
APPS = "files settings activity"
# The tilolang apps beside them, by their directory under apps/tl.
TL_APPS = ("chat", "tasks")
# The channel the agent posts in, made through Chat's own `new_channel`.
CHANNEL = "spring"
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
        took = ada.start(apps=APPS, tilolang=TL_APPS)
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
        work.do(["so.tilo.tasks#new_task", "tasks#new_task"], {"title": TASK})
        opened("so.tilo.tasks", TASK, f"the issue “{TASK}” in Ada's Tasks")

    story.step("The agent adds a Tasks issue", task)

    def note():
        work.do(["so.tilo.tasks#new_task", "tasks#new_task"], {"title": NOTE})
        opened("so.tilo.tasks", NOTE, f"the issue “{NOTE}” in Ada's Tasks")

    story.step("The agent writes one more issue", note)

    def chat():
        work.do(["chat#new_channel"], {"name": CHANNEL})
        work.do([f"chat/{CHANNEL}#send"], {"body": CHAT})
        opened("chat", CHAT, "the message in Ada's Chat")

    story.step("The agent posts in Chat", chat)

    def under_its_name():
        # The shell's own history of the workspace: every change, each under who made it, in the
        # Activity window the shell opens on its explore view.
        tree = (ada.session.state_dump() or {}).get("workspace_tree") or {}

        def history(author: str, row: str) -> None:
            """History narrowed to `author`'s changes, as Ada narrows it with the person picker,
            once it draws the row named with `row`. An open Activity window keeps the filter it
            has, so it is closed first."""
            def activity():
                return next((w for w in ada.session.windows()
                             if w.get("app_id") == ACTIVITY and w.get("rect")), None)

            if activity():
                ada.session.invoke(SHELL, CHROME, "close-activity",
                                   {"workspace": tree.get("active")})
                until("the Activity window closed", lambda: activity() is None, 30, 0.25)
            ada.session.invoke(SHELL, CHROME, "activity",
                               {"workspace": tree.get("active"), "view": "explore",
                                "author": author})
            until("the Activity window on Ada's shell", activity, 60, 0.25)
            until(f"History listing {author}'s changes", lambda: ada.shows(row), 30, 0.25)

        # Ada's own changes: setting the workspace up reads as what it did, once, under her name.
        history("Ada", "Added the device")
        story.shot(ada, "ada-s-changes")
        # Then the agent's, newest first, each naming its item: the step's shot.
        history(AGENT, "Wrote")
        # Its History lists the workspace's changes. A row of that list is no control the shell
        # names, so what it lists is read from the feed the window draws: the session's
        # `tilo.activity` history of the workspace, each change with its author.
        # Each of the agent's changes, as History words it: what was done, naming the item it
        # changed (the file's name, the issue's title, the note's title, the message's words). A
        # row that names no item is said in the failure.
        changes = {f"the file {FILE}": ("wrote", FILE),
                   f"the issue “{TASK}”": ("created the task", TASK),
                   f"the note “{NOTE}”": ("created the note", NOTE),
                   "the message": ("posted", CHAT)}

        def feed() -> list[dict]:
            answer = ada.session.invoke(ACTIVITY_PROVIDER, ACTIVITY_RESOURCE, "feed",
                                        {"scope": "this-workspace", "limit": 0})
            value = (answer.get("Completed") or {}).get("output", {}).get("value")
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
        # Newest first, a change after the one it builds on: the line appended to the note above
        # the note.
        order = [e.get("summary") or "" for e in entries]
        appended = next((i for i, said in enumerate(order) if said.startswith("Added to the note")), None)
        made = next((i for i, said in enumerate(order) if said.startswith("Created the note")), None)
        if appended is None or made is None or appended > made:
            wrong.append(f"newest first, “Added to the note” is not above “Created the note”: {order}")
        # Ada reads her own History: every row of hers is hers to her ("You"), whatever path it
        # came by, a device of hers registering itself included.
        def by_ada(e: dict) -> bool:
            author = e.get("author") or {}
            return author.get("relation") == "reader" or author.get("name") == "Ada"
        not_hers = sorted({e.get("summary") or "" for e in entries if by_ada(e)
                           and (e.get("author") or {}).get("relation") != "reader"})
        if not_hers:
            wrong.append(f"rows of Ada's read “Ada”, not “You”: {'; '.join(not_hers)}")
        # Narrowing History to a person is the window's own, never a change of the workspace.
        narrowed = sorted({said for said in order if "Workspace activity" in said
                           or said.startswith("Changed Author")})
        if narrowed:
            wrong.append(f"narrowing History is listed as a change: {'; '.join(narrowed)}")
        # Every row is a sentence in the past tense, in its app's own words: never an action's
        # label with what it acted on after a colon, nor the label quoted.
        unworded = sorted({said for said in order if said.startswith("Ran “")
                           or re.match(r"^[A-Z][a-z]+( [a-z]+)*: ", said)})
        if unworded:
            wrong.append(f"rows say an action's name, not what was done: {'; '.join(unworded)}")
        # Making the agent names the agent made.
        if not any(said == f"Added the agent “{AGENT}”" for said in order):
            wrong.append(f"no row reads “Added the agent “{AGENT}””: {order}")
        # Ada's own rows say what each did, and setting the workspace up is said once: no row of
        # hers names nothing, and none repeats the one before it; windows she opened one after
        # another are one row.
        hers = [e.get("summary") or "" for e in entries if by_ada(e)]
        opened = sorted({(e.get("summary") or "") for e, before in zip(entries[1:], entries)
                         if by_ada(e) and by_ada(before)
                         and (e.get("summary") or "").startswith("Opened")
                         and (before.get("summary") or "").startswith("Opened")})
        if opened:
            wrong.append(f"windows Ada opened one after another are listed apart: {'; '.join(opened)}")
        vague = sorted({said for said in hers
                        if said in ("Updated the workspace", "Changed who is in the workspace")})
        if vague:
            wrong.append(f"Ada's rows say nothing of what they did: {'; '.join(vague)}")
        repeated = sorted({said for said, before in zip(hers[1:], hers) if said == before})
        if repeated:
            wrong.append(f"Ada's setup is listed over and over, not folded: {'; '.join(repeated)}")
        if not hers:
            wrong.append("History lists none of Ada's own changes")
        if wrong:
            raise Failed("History: " + ". ".join(wrong))

    story.step("Ada sees each change under the agent’s name", under_its_name)

    answer: dict = {}
    asking: list[threading.Thread] = []

    def asks():
        def ask():
            try:
                answer["text"] = work.call("tilo_ask", {"question": QUESTION,
                                                        "choices": [ANSWER, "Leave it for now"]}, 90)
            except Failed as error:
                answer["error"] = str(error)

        asking.append(threading.Thread(target=ask, daemon=True))
        asking[0].start()
        until("the agent's question on Ada's shell", lambda: ada.shows(QUESTION), 30, 0.25)
        # The question as Ada sees it, before anything is pressed on it.
        story.shot(ada, "question")
        # The sheet's own answers and the system's ("Ask me later", "Don't answer") read apart.
        names = ada.names()
        missing = [name for name in (ANSWER, "Leave it for now", "Ask me later", "Don't answer")
                   if name not in names]
        if missing:
            raise Failed(f"the question's sheet offers no {', '.join(missing)}")
        ids = [name for name in names if re.search(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-", name)]
        if ids:
            raise Failed(f"the question's sheet shows a raw id, not a name: {ids[0]!r}")
        # No answer is the default: an Enter as the sheet comes up gives none, by accident or not.
        ada.session.key("Enter")
        time.sleep(1.0)
        if "text" in answer or "error" in answer or not ada.shows(QUESTION):
            raise Failed("Enter on the question as it came up answered it: "
                         f"{answer.get('text') or answer.get('error') or 'the sheet went'}")

    def answers():
        ada.press(ANSWER, "the answer Ada picks")
        asking[0].join(60)
        if "text" not in answer:
            raise Failed(f"the agent was not told her answer: {answer.get('error', 'no answer')}")
        if ANSWER not in answer["text"]:
            raise Failed(f"the agent was told {answer['text']!r}, not her answer")

    try:
        story.step("The agent asks Ada a question", asks)
        story.step("Ada answers the agent’s question", answers)
    finally:
        story.note("agent-calls", work.said)


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: agent-mcp.py OUT_DIR", file=sys.stderr)
        return 2
    return walk("agent-mcp", Path(sys.argv[1]), steps)


if __name__ == "__main__":
    sys.exit(main())

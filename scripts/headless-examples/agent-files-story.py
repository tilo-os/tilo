#!/usr/bin/env python3
"""An agent works through Files and Chat by actions alone, in a real session.

A program of this login asks at the session's agent door to work here, the
person allows it on the sheet the door puts up, and from then on every step is
the agent's own tool call (`tilo_ls`, `tilo_read`, `tilo_do`, `tilo_put_*`):
nothing is clicked or typed for it. Each claim is read back off the disk, the
apps' published state or the desk, never off the agent's own reply.

  start    Files is not running: the agent names it as the listing does
           (`files`), and running one of its actions starts it first
  find     the agent finds what it renames in what Files publishes: the
           folder's entries, and the device Files' changes are carried out on
  rename   `files.folder#rename` renames a file of the seeded folder: the new
           name is on disk, and Files' document holds the change, settled
  share    the file goes to a conversation in Chat through Chat's `send`, the
           one action the window's send box sends with
  quiet    do not disturb is turned on through the session's own action, and
           the desk holds it

Usage:
  ./agent-files-story.py [--build] [--state DIR] [--shots DIR] [--remote-exec PREFIX]

Builds nothing unless `--build` is given; `TILO_NESTED_SESSION_BUILD_EXEC` is
the build's command prefix, as for `real_apps.py`.
"""
import argparse
import base64
import json
import os
import re
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import real_apps as ra

APPS = "file-explorer chat settings"
SEEDED = "draft.txt"
RENAMED = "story.txt"
BODY = "the agent's story\n"

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


def admitted(session: "ra.Session") -> "ra.Agent":
    """An agent at the session's door, once the person allowed it on the sheet."""
    door = Path(session.runtime_directory()) / "tilo-session/agent.sock"
    ra.until("the agent door", door.exists, 60)
    agent = ra.Agent(door, "Story Agent")
    # The person's own keyboard answers the sheet: "Allow for this session".
    # Keys that came before the sheet did are pressed again.
    for _ in range(10):
        if agent.hears(2.0):
            break
        session.key("ArrowDown")
        session.key("Enter")
    if not agent.answered():
        raise ra.Failed(f"the door refused the agent: {agent.served}")
    agent.notify("notifications/initialized")
    return agent


def person_allows(session: "ra.Session") -> str:
    """The person's answer to a sheet that asks about the agent's step, as
    they press it: allowing the agent such steps while it works here when the
    sheet offers that, else this once. The name pressed, or "" with no sheet."""
    allows = [target["name"] for target in session.targets()
              if target["name"].lower().startswith("allow")]
    if not allows:
        return ""
    pressed = next((name for name in allows if "once" not in name.lower()), allows[0])
    session.click_named(pressed)
    return pressed


def call(agent: "ra.Agent", tool: str, args: dict,
         session: "ra.Session | None" = None) -> tuple[str, bool]:
    """One tool call; a step the person's grants ask first about waits for the
    person, who answers the sheet it puts up."""
    agent.send("tools/call", {"name": tool, "arguments": args})
    for _ in range(60):
        if agent.hears(1.0):
            break
        if session is not None and (pressed := person_allows(session)):
            print(f"--- the person pressed {pressed!r}")
    message = agent.reply() or {}
    result = message.get("result") or {}
    try:
        text = result["content"][0]["text"]
    except (KeyError, IndexError, TypeError):
        text = json.dumps(message)
    failed = bool(result.get("isError")) or "result" not in message
    print(f"--- {tool} {json.dumps(args)[:200]}\n{text[:1500]}")
    return text, failed


def files_home(session: "ra.Session") -> Path:
    """The folder a Files window opens on: the person's home."""
    home = session.state / "home"
    home.mkdir(parents=True, exist_ok=True)
    return home


def put(agent: "ra.Agent", data: bytes) -> str:
    """Upload `data` through the door's content puts; its content reference."""
    begun, failed = call(agent, "tilo_put_begin", {"size": len(data)})
    upload = int(re.search(r"\d+", begun).group()) if not failed else -1
    call(agent, "tilo_put_append", {"upload": upload, "offset": 0,
                                    "chunk": base64.b64encode(data).decode()})
    finished, _ = call(agent, "tilo_put_finish", {"upload": upload})
    found = re.search(r"tilo\.content:[0-9a-f]+", finished)
    if not found:
        raise ra.Failed(f"no content reference: {finished}")
    return found.group()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--build", action="store_true", help="build the session first")
    parser.add_argument("--state", default=str(Path.home() / ".cache/agent-files-story"))
    parser.add_argument("--shots", default=None, help="where the captures go")
    parser.add_argument("--remote-exec", default=os.environ.get("TILO_NESTED_SESSION_BUILD_EXEC"))
    options = parser.parse_args()

    state = Path(options.state)
    # A session that opens the way a first login does: nothing of an earlier
    # run runs in it.
    shutil.rmtree(state, ignore_errors=True)
    shots = Path(options.shots or state / "shots")
    shots.mkdir(parents=True, exist_ok=True)
    ra.prepare(state, apps=APPS, built=options.build, remote=options.remote_exec)
    with ra.Session(state) as session:
        session.start(apps=APPS, fresh=True, tag="agent-story")
        home = files_home(session)
        (home / RENAMED).unlink(missing_ok=True)
        if (home / "notes").is_dir():
            (home / "notes").rmdir()
        (home / SEEDED).write_text(BODY)
        agent = admitted(session)

        print("\n[start] Files is not running; one of its actions starts it")
        text, failed = call(agent, "tilo_ls", {"path": "files"})
        check("the agent names Files as the listing does", not failed
              and "installed but not running" in text, text.splitlines()[0] if text else "")
        text, failed = call(agent, "tilo_do", {"path": "files#read", "wait_ms": 30000}, session)
        check("Files' read answers its document", not failed and '"places"' in text, text[:120])
        try:
            check("Files runs with its window on the desk", session.answering("files", 30))
        except ra.Failed as error:
            check("Files runs with its window on the desk", False, str(error))

        print("\n[find] the folder and the device, from what Files publishes")
        text, failed = call(agent, "tilo_ls", {"path": "files"})
        check("Files lists its verbs to the agent", not failed and "#rename" in text, text[:160])
        # A change made in the folder the window shows is in Files' document
        # with the device that carries it out and the folder it was made in.
        text, failed = call(agent, "tilo_do", {"path": "files#new-folder-here",
                                               "args": {"name": "notes"}}, session)
        check("a folder is made in the folder shown", not failed
              and ra.until("the new folder", lambda: (home / "notes").is_dir(), 30, 0.25))
        text, failed = call(agent, "tilo_read", {"path": "files"})
        made = next((change for change in json.loads(text[text.index("{"):]).get("changes", [])
                     if "NewFolder" in change.get("op", {})), {}) if "{" in text else {}
        device = made.get("device", "")
        folder = made.get("op", {}).get("NewFolder", {}).get("parent", "")
        check("Files' document names the device and the folder of that change",
              bool(device and folder), f"{device} {folder}")

        print("\n[rename] files.folder#rename, by device, folder and entry")
        text, failed = call(agent, "tilo_do", {"path": "files#rename", "args": {
            "device": device, "folder": folder, "entry": SEEDED, "name": RENAMED}}, session)
        check("the rename answers", not failed, text.splitlines()[0] if text else "")
        landed = ra.until(f"{RENAMED} on disk", lambda: (home / RENAMED).is_file(), 30, 0.25) \
            if not failed else False
        check("the file has its new name on disk", bool(landed)
              and not (home / SEEDED).exists()
              and (home / RENAMED).read_text() == BODY)
        text, _ = call(agent, "tilo_read", {"path": "files"})
        check("Files' document holds the rename, settled", RENAMED in text
              and '"done"' in text, text[-200:])
        session.snapshot(shots / "renamed.png")

        print("\n[share] the file goes to a conversation through Chat's send")
        reference = put(agent, BODY.encode())
        text, failed = call(agent, "tilo_do", {"path": "chat#open", "wait_ms": 30000}, session)
        channel = re.search(r"^(\S+)/\s+chat\.workspace\.channels", text, re.M)
        check("Chat lists a conversation to share into", not failed and bool(channel), text[:120])
        conversation = channel.group(1) if channel else ""
        send = {"path": "chat#send", "args": {
            "to": [{"kind": "conversation", "id": conversation, "name": conversation}],
            "attached": [{"kind": "file", "name": RENAMED,
                          "blob": reference.removeprefix("tilo.content:"),
                          "size": len(BODY.encode())}],
            "text": "the renamed story"}}
        text, failed = call(agent, "tilo_do", send, session)
        check("Chat's send takes the file into the conversation", not failed
              and '"accepted":true' in text.replace(" ", ""), text[:200])
        session.snapshot(shots / "shared.png")

        print("\n[quiet] do not disturb, through the session's own action")
        # The bar's bell, as the desk names it: "Notifications silenced".
        silenced = lambda: [target["name"] for target in session.targets()  # noqa: E731
                            if "silenced" in target["name"].lower()]
        before = silenced()
        text, failed = call(agent, "tilo_find", {"query": "do not disturb"})
        centre = re.search(r"(session/notifications)\b", text)
        centre = centre.group(1) if centre else "session/notifications"
        text, failed = call(agent, "tilo_do", {"path": f"{centre}#set-quiet",
                                               "args": {"on": True}}, session)
        check("the agent turns do not disturb on", not failed, text.splitlines()[0] if text else "")
        try:
            quiet = not before and ra.until("the bar's bell to say so", silenced, 15, 0.25)
        except ra.Failed:
            quiet = False
        check("the bar's bell says notifications are silenced", bool(quiet), str(quiet or before))
        session.snapshot(shots / "quiet.png")
        agent.close()

    print()
    if failures:
        print(f"FAILED: {len(failures)} check(s): {', '.join(failures)}")
        return 1
    print("PASS: an agent renamed, shared and quieted by actions alone")
    return 0


if __name__ == "__main__":
    sys.exit(main())

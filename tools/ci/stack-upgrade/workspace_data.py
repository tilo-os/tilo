#!/usr/bin/env python3
"""A workspace's data written through a stack's API by one build, and read back by the next.

`fill` makes a workspace and writes one of each kind of thing a person keeps in one: chat
messages, tasks, a note, a file, a message in Mail, a contact and a calendar event. It prints
what it wrote as JSON. `check EXPECTED` reads each of them back and fails, naming what is
missing, when one is gone. Both talk to the stack's API as its owner, with TILO_API_URL and
TILO_API_KEY (`stack.sh env`).

usage: workspace_data.py fill > expected.json
       workspace_data.py check expected.json
"""

from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

URL = os.environ.get("TILO_API_URL", "")
KEY = os.environ.get("TILO_API_KEY", "")
WORKSPACE = "Upgrade check"


def call(method: str, path: str, body=None, raw: bytes | None = None, tries: int = 60):
    """The API's answer to `method path`, asked again while the stack is still starting."""
    data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
    kind = "application/octet-stream" if raw is not None else "application/json"
    last = ""
    for _ in range(tries):
        request = urllib.request.Request(
            URL + path,
            data=data,
            method=method,
            headers={"Authorization": f"Bearer {KEY}", "Content-Type": kind},
        )
        try:
            with urllib.request.urlopen(request, timeout=120) as answer:
                text = answer.read()
                return json.loads(text) if text else None
        except urllib.error.HTTPError as error:
            last = error.read().decode(errors="replace")
            starting = any(word in last for word in ("retry", "not serving", "starting"))
            if error.code in (425, 429, 503) or (error.code in (409, 422) and starting):
                time.sleep(3)
                continue
            raise SystemExit(f"{method} {path}: {error.code} {last[:600]}") from None
        except urllib.error.URLError as error:
            last = str(error)
            time.sleep(3)
    raise SystemExit(f"{method} {path}: no answer: {last[:600]}")


def quoted(path: str) -> str:
    return "/".join(urllib.parse.quote(part, safe="") for part in path.split("/"))


def act(workspace: str, target: str, **args):
    resource, _, action = target.rpartition("#")
    return call(
        "POST",
        f"/v1/workspaces/{workspace}/resources/{quoted(resource)}/actions/{quoted(action)}",
        args,
    )


def rows(workspace: str, app: str) -> list[str]:
    listed = call("GET", f"/v1/workspaces/{workspace}/resources?path={quoted(app)}")
    return sorted(row["path"] for row in listed.get("rows", []))


def children(workspace: str, path: str) -> list[str]:
    read = call("GET", f"/v1/workspaces/{workspace}/resources/{quoted(path)}")
    return sorted(str(child.get("name")) for child in read.get("children", []))


def workspace_id(name: str) -> str | None:
    listed = call("GET", "/v1/workspaces") or {}
    for workspace in listed.get("workspaces", []):
        if workspace.get("name") == name:
            return workspace.get("id")
    return None


def fill() -> dict:
    made = call("POST", "/v1/workspaces", {"name": WORKSPACE})
    workspace = made.get("id") or workspace_id(WORKSPACE)
    channel = next(
        (path.rsplit("/", 1)[-1] for path in rows(workspace, "chat") if "/c_" in path), "c_chat"
    )
    channel = channel if channel.startswith("c_") else "c_chat"
    messages = ["Before the upgrade: hello", "A second message, with a link https://example.org"]
    for text in messages:
        act(workspace, "chat/Messages#post", channel=channel, text=text)
    tasks = ["Carry the data forward", "A second task"]
    act(workspace, "tasks/Issues#create", title=tasks[0], body="Written before the upgrade")
    act(workspace, "tasks/Issues#create", title=tasks[1], status="in-progress")
    act(workspace, "notes/Notes#new-note", title="Upgrade note")
    content = call(
        "POST", f"/v1/workspaces/{workspace}/uploads", raw=b"A plan, written before the upgrade.\n" * 40
    )["content"]
    call(
        "POST",
        f"/v1/workspaces/{workspace}/resources/files/actions/open",
        {"files": [{"content": content, "name": "plan-before-upgrade.txt", "dir": "docs"}]},
    )
    mail = "Welcome (before the upgrade)"
    act(
        workspace,
        "mail/Messages#ingest",
        messages=[{"id": str(uuid.uuid4()), "account": "upgrade", "folder": "inbox", "label": mail,
                   "read": False, "starred": True}],
    )
    contacts = next((path for path in rows(workspace, "contacts") if path.endswith("Contacts 2")),
                    "contacts/Contacts 2")
    act(workspace, f"{contacts}#new-contact", name="Grace Customer")
    event = "Upgrade review meeting"
    act(workspace, "calendar/Calendar#new-event",
        event={"title": event, "start": "2026-11-23T10:00", "end": "2026-11-23T11:00"})
    return {
        "workspace": WORKSPACE,
        "chat/Messages": messages,
        "tasks": [f"tasks/{title}" for title in tasks],
        "notes": ["notes/Upgrade note"],
        "files": ["docs/plan-before-upgrade.txt"],
        "mail/Messages": [mail],
        "contacts": ["Grace"],
        "calendar/Calendar": [event],
    }


def check(expected: dict) -> list[str]:
    workspace = workspace_id(expected["workspace"])
    if workspace is None:
        return [f"the workspace {expected['workspace']!r} is gone"]
    missing = []
    for path in ("chat/Messages", "mail/Messages", "calendar/Calendar"):
        held = children(workspace, path)
        missing += [f"{path}: {name!r}" for name in expected[path] if name not in held]
    for app in ("tasks", "notes", "files", "contacts"):
        held = " ".join(rows(workspace, app)).replace("\\/", "/")
        missing += [f"{app}: {name!r}" for name in expected[app] if name not in held]
    return missing


def main(argv: list[str]) -> int:
    if not URL or not KEY:
        print("TILO_API_URL and TILO_API_KEY name the stack's API: eval \"$(stack.sh env)\"",
              file=sys.stderr)
        return 2
    if argv[:1] == ["fill"]:
        print(json.dumps(fill(), indent=1))
        return 0
    if argv[:1] == ["check"] and len(argv) == 2:
        with open(argv[1]) as file:
            missing = check(json.load(file))
        for gone in missing:
            print(f"after the upgrade, missing: {gone}", file=sys.stderr)
        if not missing:
            print("every thing written before the upgrade opens after it")
        return 1 if missing else 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

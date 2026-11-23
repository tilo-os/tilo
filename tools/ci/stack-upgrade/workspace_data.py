#!/usr/bin/env python3
"""A workspace's data written through a stack's API by one build, and read back by the next.

`fill` makes a workspace and writes one of each kind of thing a person keeps in one: chat
messages, tasks, a note, a file, a message in Mail, a contact and a calendar event. It prints
what it wrote as JSON. `check EXPECTED` reads each of them back and fails, naming what is
missing, when one is gone. Both talk to the stack's API as its owner, with TILO_API_URL and
TILO_API_KEY (`stack.sh env`).

The workspace's vault and mail accounts on its host are asked as the person's page asks
them, by the stack's own person on their device (`stack.sh person do workspace/…`, STACK_SH
naming stack.sh and PERSON_TILO the tilo command that asks): an entry of its Passwords and an
item of its vault, and a mail account at the stack's stand-in mail server
(`TILO_SH_MAIL_STAND_IN=1`), its sign-in taken by that server before the host keeps it. The
messages the stand-in holds are read from it through the host and recorded in the
workspace's mailbox, as the page's Mail does. The base build kept these per person; this one
moves them into the workspaces that person owns as it starts. After the upgrade the entry
still opens to its secret, the item still signs in to the mail server, and the account still
reads its inbox.

usage: workspace_data.py fill > expected.json
       workspace_data.py check expected.json
"""

from __future__ import annotations

import json
import os
import subprocess
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


def person(workspace: str, target: str, **args):
    """What the host answered the stack's own person, on their device, to `target`."""
    words = [f"{name}={json.dumps(value)}"
             for name, value in args.items()]
    asked = subprocess.run(
        ["bash", os.environ["STACK_SH"], "person", "do", target, "-w", workspace, "--json",
         *words],
        env={**os.environ, "TILO_SH_TILO_BIN": os.environ["PERSON_TILO"]},
        capture_output=True, text=True, timeout=300, check=False,
    )
    if asked.returncode != 0:
        raise SystemExit(f"{target}: {asked.stderr.strip()[-600:] or asked.stdout[-600:]}")
    return json.loads(asked.stdout).get("result")


def stand_in() -> dict:
    """The stack's stand-in mail account, as a person types it (`stack.sh mail-account`)."""
    kept = os.path.join(os.environ["TILO_SH_STATE"], "mail-stand-in")
    base = int(os.environ["TILO_SH_PORT_BASE"])

    def read(name: str) -> str:
        with open(os.path.join(kept, name)) as file:
            return file.read().strip()

    login = read("login")
    return {"address": login, "displayName": "The lab", "username": login,
            "incomingHost": "localhost", "incomingPort": base + 24,
            "outgoingHost": "localhost", "outgoingPort": base + 25,
            "password": read("password")}


def inbox(workspace: str, account: str) -> list[dict]:
    """The account's inbox as its mail server holds it, read through the host."""
    return person(workspace, "workspace/mail-accounts#fetch", account=account,
                  folder="inbox").get("messages", [])


def rows(workspace: str, app: str) -> list[str]:
    listed = call("GET", f"/v1/workspaces/{workspace}/resources?path={quoted(app)}")
    return sorted(row["path"] for row in listed.get("rows", []))


def children(workspace: str, path: str) -> list[str]:
    read = call("GET", f"/v1/workspaces/{workspace}/resources/{quoted(path)}")
    return sorted(str(child.get("name")) for child in read.get("children", []))


def made_workspace() -> str:
    """A new workspace called WORKSPACE, made as the account's `workspaces#create`: its id. A
    base build from before the account's actions made it with `POST /v1/workspaces`."""
    try:
        made = call("POST", "/v1/resources/account/workspaces/actions/create", {"name": WORKSPACE})
        return made["result"]["workspace"]["id"]
    except SystemExit as refused:
        if ": 404 " not in str(refused):
            raise
    return call("POST", "/v1/workspaces", {"name": WORKSPACE})["id"]


def fill() -> dict:
    workspace = made_workspace()
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
    act(workspace, "contacts#new-contact", name="Grace Customer")
    event = "Upgrade review meeting"
    act(workspace, "calendar/Calendar#new-event",
        event={"title": event, "start": "2026-11-23T10:00", "end": "2026-11-23T11:00"})
    # The person's own: an entry of their Passwords, as the page's Passwords keeps one.
    entry = {"id": uuid.uuid4().hex, "kind": "login", "title": "Upgrade bank",
             "username": "ada@bank.example", "password": "kept-before-the-upgrade-7Q",
             "origins": ["https://bank.example"], "created_unix": int(time.time()),
             "updated_unix": int(time.time())}
    person(workspace, "workspace/vault#keep-entry", entry=entry)
    # A mail account at the stand-in, as the page's Settings connects one: the server signs
    # in with the password before it keeps the account and seals the password.
    account = stand_in()
    connected = person(workspace, "workspace/mail-accounts#add", identities=[], **account)
    # An item of the vault, bound to the stand-in's servers: what an account connected by
    # reference signs in with.
    item = person(workspace, "workspace/vault#add", kind="login",
                  belongsTo=[f"imap://localhost:{account['incomingPort']}",
                             f"smtp://localhost:{account['outgoingPort']}"],
                  username=account["username"], secret=account["password"])
    # The page's Mail reads the inbox through the host and records each message it read in
    # the workspace's mailbox, with its account and where its server keeps it.
    fetched = inbox(workspace, connected["id"])
    if not fetched:
        raise SystemExit("the stand-in mail server's inbox read empty")
    received = [message["subject"] for message in fetched]
    act(workspace, "mail/Messages#ingest", messages=[
        {"id": str(uuid.uuid5(uuid.NAMESPACE_URL, f"{connected['id']}/{message['messageId']}")),
         "account": connected["id"], "locator": message["messageId"], "folder": "inbox",
         "label": message["subject"], "read": False, "starred": False}
        for message in fetched])
    return {
        "workspace": workspace,
        "chat/Messages": messages,
        "tasks": [f"tasks/{title}" for title in tasks],
        "notes": ["notes/Upgrade note"],
        "files": ["docs/plan-before-upgrade.txt"],
        "mail/Messages": [mail, *received],
        "contacts": ["Grace"],
        "calendar/Calendar": [event],
        "workspace/vault#entries": {key: entry[key] for key in ("id", "title", "password")},
        "workspace/vault#list": {"reference": item["reference"], "belongsTo": item["belongsTo"]},
        "workspace/mail-accounts#list": {key: account[key] for key in
                                      ("address", "incomingHost", "incomingPort",
                                       "outgoingHost", "outgoingPort")},
        "inbox": received,
    }


def check(expected: dict) -> list[str]:
    workspace = expected["workspace"]
    missing = []
    for path in ("chat/Messages", "mail/Messages", "calendar/Calendar"):
        held = children(workspace, path)
        missing += [f"{path}: {name!r}" for name in expected[path] if name not in held]
    for app in ("tasks", "notes", "files", "contacts"):
        held = " ".join(rows(workspace, app)).replace("\\/", "/")
        missing += [f"{app}: {name!r}" for name in expected[app] if name not in held]
    return missing + check_persons_own(workspace, expected)


def check_persons_own(workspace: str, expected: dict) -> list[str]:
    """What the person keeps on the host for themself, read back and used as they would."""
    missing = []
    entry = expected["workspace/vault#entries"]
    entries = person(workspace, "workspace/vault#entries").get("entries", [])
    kept = next((held for held in entries if held.get("id") == entry["id"]), None)
    if kept is None or kept.get("title") != entry["title"]:
        missing.append(f"workspace/vault: the Passwords entry {entry['title']!r}")
    elif kept.get("password") != entry["password"]:
        missing.append(f"workspace/vault: the Passwords entry {entry['title']!r} opens to "
                       "another secret than it was kept with")
    item = expected["workspace/vault#list"]
    items = person(workspace, "workspace/vault#list").get("items", [])
    if not any(held.get("reference") == item["reference"]
               and held.get("belongsTo") == item["belongsTo"] for held in items):
        missing.append(f"workspace/vault: the item {item['reference']} for {item['belongsTo']}")
    account = expected["workspace/mail-accounts#list"]
    accounts = person(workspace, "workspace/mail-accounts#list").get("accounts", [])
    held = next((held for held in accounts if held.get("address") == account["address"]), None)
    if held is None or not held.get("hasSecret"):
        return missing + [f"workspace/mail-accounts: {account['address']!r}, with its sign-in"]
    if any(held.get(key) != value for key, value in account.items()):
        missing.append(f"workspace/mail-accounts: {account['address']!r} names other servers: "
                       f"{held}")
    # The sign-in the host kept opens: the inbox reads from the mail server again.
    read = [message["subject"] for message in inbox(workspace, held["id"])]
    missing += [f"the inbox of {account['address']!r}: {subject!r}"
                for subject in expected["inbox"] if subject not in read]
    # The item's secret opens: the host signs in with it before it takes it as the
    # account's sign-in, and the inbox reads with it.
    if not missing:
        person(workspace, "workspace/mail-accounts#add-mail-with-item",
               password=item["reference"],
               **{key: value for key, value in account.items()})
        read = [message["subject"] for message in inbox(workspace, held["id"])]
        missing += [f"the inbox of {account['address']!r}, signed in with the vault's item: "
                    f"{subject!r}" for subject in expected["inbox"] if subject not in read]
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

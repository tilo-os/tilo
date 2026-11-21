#!/usr/bin/env python3
"""A workspace's records and members, as one stack's API answers its owner: what
test-server-handover.sh writes on one server and reads on the other.

Every command talks to the stack TILO_API_URL names, with the key TILO_API_KEY holds
(`stack.sh env`), and prints JSON.

  fill NAME              make the workspace NAME and write records in its notes, chat,
                         files and mail; prints its id and what was written
  write WORKSPACE WORD   write one chat message and one note named after WORD
  counts WORKSPACE       what the server holds of it: members by role and rows per app,
                         and the names of its chat messages, notes, files and mail
  expect WORKSPACE FILE [SECONDS]
                         wait until the server holds every record FILE names (a fill's or
                         a write's output); fails naming what it lacks when the time is up
  act WORKSPACE TARGET [JSON]
                         the answer to the action TARGET (`workspace/servers#add`) with the
                         arguments JSON
  workspaces             the workspaces the key reaches
  bytes WORKSPACE PATH   the bytes of the file at PATH (`files/docs/handover-1.txt`) as the
                         server serves them: their length and SHA-256
  words SEED             the 24 recovery words of the root whose 32-byte seed the file SEED
                         holds, as an earlier tilo kept it: what its person types into
                         `tilo login --recovery-key`. Needs no API.
"""

from __future__ import annotations

import collections
import hashlib
import re
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
APPS = ("notes", "chat", "files", "mail")


class Refused(Exception):
    """The API answered with an error status: its code and its words."""


def call(method: str, path: str, body=None, raw: bytes | None = None, tries: int = 40):
    """The API's answer to `method path`, asked again while the server is still starting or
    waits for the workspace's keys a holder is about to give it."""
    data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
    kind = "application/octet-stream" if raw is not None else "application/json"
    last = ""
    for _ in range(tries):
        request = urllib.request.Request(
            URL + path, data=data, method=method,
            headers={"Authorization": f"Bearer {KEY}", "Content-Type": kind},
        )
        try:
            with urllib.request.urlopen(request, timeout=120) as answer:
                text = answer.read()
                return json.loads(text) if text else None
        except urllib.error.HTTPError as error:
            last = error.read().decode(errors="replace")
            starting = any(word in last for word in ("retry", "not serving", "starting",
                                                     "no group keys yet"))
            if error.code in (425, 429, 503) or (error.code in (409, 422) and starting):
                time.sleep(3)
                continue
            raise Refused(f"{method} {path}: {error.code} {last[:800]}") from None
        except urllib.error.URLError as error:
            last = str(error)
            time.sleep(3)
    raise Refused(f"{method} {path}: no answer: {last[:800]}")


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
    listed = call("GET", f"/v1/workspaces/{workspace}/resources?path={quoted(app)}") or {}
    return sorted(str(row.get("path")) for row in listed.get("rows", []))


def children(workspace: str, path: str) -> list[str]:
    read = call("GET", f"/v1/workspaces/{workspace}/resources/{quoted(path)}") or {}
    return sorted(str(child.get("name")) for child in read.get("children", []))


def channel_of(workspace: str) -> str:
    named = next((path.rsplit("/", 1)[-1] for path in rows(workspace, "chat") if "/c_" in path),
                 "c_chat")
    return named if named.startswith("c_") else "c_chat"


def fill(name: str) -> dict:
    made = call("POST", "/v1/workspaces", {"name": name}) or {}
    workspace = made.get("id") or next(
        (held["id"] for held in (call("GET", "/v1/workspaces") or {}).get("workspaces", [])
         if held.get("name") == name), None)
    if not workspace:
        raise Refused(f"no workspace named {name!r} after making it: {made}")
    channel = channel_of(workspace)
    messages = [f"Before the handover {n}" for n in range(1, 4)]
    for text in messages:
        act(workspace, "chat/Messages#post", channel=channel, text=text)
    notes = ["Handover plan", "Handover notes"]
    for title in notes:
        act(workspace, "notes/Notes#new-note", title=title)
    files = []
    for n in (1, 2):
        content = call("POST", f"/v1/workspaces/{workspace}/uploads",
                       raw=f"File {n}, written before the handover.\n".encode() * 50)["content"]
        call("POST", f"/v1/workspaces/{workspace}/resources/files/actions/open",
             {"files": [{"content": content, "name": f"handover-{n}.txt", "dir": "docs"}]})
        files.append(f"docs/handover-{n}.txt")
    mail = ["Welcome before the handover", "A second mail before the handover"]
    act(workspace, "mail/Messages#ingest", messages=[
        {"id": str(uuid.uuid4()), "account": "handover", "folder": "inbox", "label": label,
         "read": False, "starred": False} for label in mail])
    return {"workspace": workspace, "chat/Messages": messages,
            "notes": [f"notes/{title}" for title in notes], "files": files,
            "mail/Messages": mail}


def write(workspace: str, word: str) -> dict:
    text = f"Written on the {word} server"
    act(workspace, "chat/Messages#post", channel=channel_of(workspace), text=text)
    title = f"Note from the {word} server"
    act(workspace, "notes/Notes#new-note", title=title)
    return {"workspace": workspace, "chat/Messages": [text], "notes": [f"notes/{title}"]}


def counts(workspace: str) -> dict:
    line: dict = {"workspace": workspace}
    try:
        members = call("GET", f"/v1/workspaces/{workspace}/members") or {}
        people = members.get("members", members if isinstance(members, list) else [])
        line["members"] = dict(collections.Counter(str(m.get("role")) for m in people))
    except Refused as error:
        line["members"] = f"ERR {error}"
    for app in APPS:
        try:
            line[app] = len(rows(workspace, app))
        except Refused as error:
            line[app] = f"ERR {error}"
    for path in ("chat/Messages", "mail/Messages"):
        try:
            line[path] = children(workspace, path)
        except Refused as error:
            line[path] = f"ERR {error}"
    for app in ("notes", "files"):
        try:
            line[f"{app} rows"] = rows(workspace, app)
        except Refused as error:
            line[f"{app} rows"] = f"ERR {error}"
    return line


def file_bytes(workspace: str, path: str) -> dict:
    # The row as the app lists it: a file's name within its folder is one segment, its
    # slashes escaped (`files/docs\\/handover-1.txt`).
    app, _, name = path.partition("/")
    row = next((row for row in rows(workspace, app) if row.replace("\\/", "/") == path), None)
    if row is None:
        raise Refused(f"no {path} in {app}")
    resource = "/".join(urllib.parse.quote(part, safe="") for part in row.split("/", 1))
    read = json.dumps(call("GET", f"/v1/workspaces/{workspace}/resources/{resource}"))
    found = re.search(r"tilo\.content:[0-9a-f]{64}", read)
    if not found:
        raise Refused(f"{path} names no content: {read[:300]}")
    request = urllib.request.Request(
        URL + f"/v1/workspaces/{workspace}/content/{urllib.parse.quote(found.group(), safe='')}",
        headers={"Authorization": f"Bearer {KEY}"})
    try:
        with urllib.request.urlopen(request, timeout=120) as answer:
            body = answer.read()
    except urllib.error.HTTPError as error:
        raise Refused(f"{path}: {error.code} {error.read().decode(errors='replace')[:400]}") from None
    return {"path": path, "length": len(body), "sha256": hashlib.sha256(body).hexdigest()}


def lacking(workspace: str, expected: dict) -> list[str]:
    missing = []
    for path in ("chat/Messages", "mail/Messages"):
        if path not in expected:
            continue
        try:
            held = children(workspace, path)
        except Refused as error:
            missing.append(f"{path}: {error}")
            continue
        missing += [f"{path}: {name!r}" for name in expected[path] if name not in held]
    for app in ("notes", "files"):
        if app not in expected:
            continue
        try:
            held = " ".join(rows(workspace, app)).replace("\\/", "/")
        except Refused as error:
            missing.append(f"{app}: {error}")
            continue
        missing += [f"{app}: {name!r}" for name in expected[app] if name not in held]
    return missing


def expect(workspace: str, expected: dict, seconds: float) -> int:
    deadline = time.monotonic() + seconds
    while True:
        missing = lacking(workspace, expected)
        if not missing or time.monotonic() > deadline:
            break
        time.sleep(3)
    print(json.dumps({"workspace": workspace, "missing": missing}))
    for gone in missing:
        print(f"missing: {gone}", file=sys.stderr)
    return 1 if missing else 0


def words(seed_path: str) -> str:
    """The BIP39 words of a 32-byte seed, from the list Tilo's own encoder uses."""
    source = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                          "../../../crates/collab/threshold-identity/src/bip39.rs")
    with open(source) as file:
        text = file.read()
    listed = text.split('const WORDS: &str = "', 1)[1].split('";', 1)[0]
    listed = listed.replace("\\\n", " ").split()
    assert len(listed) == 2048, f"{source} lists {len(listed)} words"
    with open(seed_path, "rb") as file:
        seed = file.read()
    assert len(seed) == 32, f"{seed_path} holds {len(seed)} bytes, not a 32-byte seed"
    bits = int.from_bytes(seed + hashlib.sha256(seed).digest()[:1], "big")
    return " ".join(listed[(bits >> (11 * (23 - at))) & 0x7FF] for at in range(24))


def main(argv: list[str]) -> int:
    if argv[:1] == ["words"] and len(argv) == 2:
        print(words(argv[1]))
        return 0
    if not URL or not KEY:
        print('TILO_API_URL and TILO_API_KEY name the stack\'s API: eval "$(stack.sh env)"',
              file=sys.stderr)
        return 2
    try:
        match argv:
            case ["fill", name]:
                print(json.dumps(fill(name)))
            case ["write", workspace, word]:
                print(json.dumps(write(workspace, word)))
            case ["counts", workspace]:
                print(json.dumps(counts(workspace)))
            case ["expect", workspace, path, *seconds]:
                with open(path) as file:
                    return expect(workspace, json.load(file), float(seconds[0]) if seconds else 0)
            case ["act", workspace, target, *args]:
                print(json.dumps(act(workspace, target, **(json.loads(args[0]) if args else {}))))
            case ["bytes", workspace, path]:
                print(json.dumps(file_bytes(workspace, path)))
            case ["workspaces"]:
                print(json.dumps(call("GET", "/v1/workspaces")))
            case _:
                print(__doc__, file=sys.stderr)
                return 2
    except Refused as error:
        print(str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

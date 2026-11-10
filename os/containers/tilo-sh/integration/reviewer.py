"""The visa run's reviewer, a real person on the local tilo.sh.

Serves `POST {"link": …}` on 127.0.0.1:PORT, the endpoint the visa run
(sdk/python/examples/visa_proof.py) posts the reviewer's invitation to as
`TILO_INVITEE`. For each link a new person, in their own fresh headless
Chromium (person.mjs), arrives at the site, follows the link into the
workspace it names, and approves a key of their own on the site's sign-in
page. The answer is `{key, person, joined}`, where `joined` is the workspace
the link landed them in with the role their key finds them holding there.

    python3 reviewer.py PORT SITE RELAY DOOR TILO_BIN OUT_DIR

Prints `{"invitee": URL}` once it listens.
"""

from __future__ import annotations

import asyncio
import itertools
import json
import os
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from tilo import AsyncTilo

PORT, SITE, RELAY, DOOR, TILO_BIN, OUT = sys.argv[1:7]
PERSON = Path(__file__).with_name("person.mjs")
counter = itertools.count(1)


async def role_in(key: str, workspace: str) -> str | None:
    client = AsyncTilo(key=key, door=DOOR)
    try:
        listed = await client.workspaces.list()
        return next((row.role for row in listed if row.id == workspace), None)
    finally:
        await client.aclose()


def review(link: str) -> dict:
    out = Path(OUT) / f"reviewer-{next(counter)}"
    ran = subprocess.run(
        ["node", str(PERSON), SITE, RELAY, str(out), "--follow", link,
         "--key", "Rosa's review", "--tilo", TILO_BIN, "--door", DOOR],
        capture_output=True, text=True, timeout=600,
    )
    sys.stderr.write(ran.stderr)
    if ran.returncode != 0:
        return {"error": f"the reviewer did not get in: {ran.stderr[-1500:]}"}
    person = json.loads(ran.stdout.strip().splitlines()[-1])
    followed = person.get("followed") or {}
    workspace = followed.get("room") or followed.get("scope")
    role = asyncio.run(role_in(person["key"], workspace))
    return {"key": person["key"], "person": None, "joined": {"workspace": workspace, "role": role}}


class Handler(BaseHTTPRequestHandler):
    def do_POST(self) -> None:  # noqa: N802 (the server's name for it)
        body = self.rfile.read(int(self.headers.get("content-length") or 0))
        try:
            link = json.loads(body)["link"]
            answer = review(link)
        except (ValueError, KeyError) as error:
            answer = {"error": f"post a JSON object naming the link: {error}"}
        data = json.dumps(answer).encode()
        self.send_response(200 if "error" not in answer else 500)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, format: str, *args: object) -> None:
        sys.stderr.write(f"reviewer: {format % args}\n")


if __name__ == "__main__":
    os.makedirs(OUT, exist_ok=True)
    server = ThreadingHTTPServer(("127.0.0.1", int(PORT)), Handler)
    print(json.dumps({"invitee": f"http://127.0.0.1:{PORT}/accept"}), flush=True)
    server.serve_forever()

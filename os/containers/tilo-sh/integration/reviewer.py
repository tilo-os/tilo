"""The visa run's reviewer, a real person on the local tilo.sh.

Serves `POST {"link": …}` on 127.0.0.1:PORT, the endpoint the visa run
(sdk/python/examples/visa_proof.py) posts the reviewer's invitation to as
`TILO_INVITEE`. For each link a new person, in their own fresh headless
Chromium (person.mjs), arrives at the site, follows the link into the
workspace it names, and approves a key of their own on the site's sign-in
page. The answer is `{key, person, joined}`, where `joined` is the workspace
the link landed them in with the role their key finds them with there; a
reviewer that gets stuck answers `{error}` with what the person was waiting
for and what its page showed, within a step budget of the page drivers'
`TILO_STEP_SECONDS`.

    python3 reviewer.py PORT SITE SERVER API_URL TILO_BIN OUT_DIR

Prints `{"invitee": URL}` once it listens.
"""

from __future__ import annotations

import asyncio
import itertools
import json
import os
import subprocess
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from tilo import Tilo, run

PORT, SITE, SERVER, API_URL, TILO_BIN, OUT = sys.argv[1:7]
PERSON = Path(__file__).with_name("person.mjs")
counter = itertools.count(1)
STEP_SECONDS = float(os.environ.get("TILO_STEP_SECONDS") or 5)
# The page drivers give every wait a whole number of steps of
# `TILO_STEP_SECONDS` (tools/web/chrome.mjs); the person's waits take a budget
# of 36 (person.mjs). A reviewer makes a few of them, so the helper stops
# one who outlasts this many steps and answers for it, and the caller waits
# three steps longer than that instead of timing out ahead of its page.
REVIEW_STEPS = 48
REVIEW_SECONDS = REVIEW_STEPS * STEP_SECONDS


async def role_in(key: str, workspace: str) -> str | None:
    client = Tilo(key=key, url=API_URL)
    try:
        listed = (await run(client, {"path": "account/workspaces", "action": "list"})).get(
            "result"
        ) or {}
        return next(
            (
                row.get("role")
                for row in listed.get("workspaces") or []
                if row.get("id") == workspace
            ),
            None,
        )
    finally:
        await client.aclose()


def review(link: str) -> dict:
    out = Path(OUT) / f"reviewer-{next(counter)}"
    said: list[str] = []
    code = 1
    with subprocess.Popen(
        ["node", str(PERSON), SITE, SERVER, str(out), "--follow", link,
         "--key", "Rosa's review", "--tilo", TILO_BIN, "--api", API_URL],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    ) as browser:
        assert browser.stderr is not None and browser.stdout is not None

        # What the reviewer's browser says goes to this helper's log as it
        # says it, so a reviewer that gets stopped has said how far they got.
        def echo() -> None:
            for line in browser.stderr:
                sys.stderr.write(line)
                sys.stderr.flush()
                said.append(line)
                del said[:-40]

        echoing = threading.Thread(target=echo, daemon=True)
        echoing.start()
        try:
            code = browser.wait(timeout=REVIEW_SECONDS)
        except subprocess.TimeoutExpired:
            # Stopping the person lets it answer what it was waiting for and
            # what its page showed, before the helper answers for it.
            browser.terminate()
            try:
                code = browser.wait(timeout=2 * STEP_SECONDS)
            except subprocess.TimeoutExpired:
                browser.kill()
                code = browser.wait()
        printed = browser.stdout.read()
    echoing.join(timeout=STEP_SECONDS)
    if code != 0:
        return {"error": f"the reviewer did not get in: {''.join(said)[-1500:]}"}
    person = json.loads(printed.strip().splitlines()[-1])
    followed = person.get("followed") or {}
    workspace = followed.get("workspace") or followed.get("scope")
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

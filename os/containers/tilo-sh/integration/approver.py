"""The person a check holds, answering what their hosts ask them.

`tilo-approve` (a program this tree builds for the checks, like `tilo-enroll`)
connects to the relay as one of the person's approving devices, from the root
their page's recovery key opened, and answers the next question a host puts to
them — allow, deny, or nothing at all. A host asks before what a person's
standing makes them answer for: an agent's action that reaches outside a
workspace's records (running a command in its VM among them), a page action a
key without the Browser's scope asks about, and a submission a page is held at.

    Person(APPROVER, ROOT, HOST).begin("allow")   before the call that asks
    await person.settle(started)                  once it answered

The questions it saw are printed as one JSON line each by `tilo-approve` and
kept here, so a check can hold what the person was asked to what it expected.
"""

from __future__ import annotations

import asyncio
import json
import os


class Person:
    """The person the page made, answering for themselves."""

    def __init__(self, approver: str, root: str, host: str, check=None) -> None:
        self.approver = approver
        self.root = root
        self.host = host
        self.asked: list[dict] = []
        self._check = check or (lambda condition, what: _raise(condition, what))
        self._check(
            self.approver and os.path.isfile(self.approver),
            f"the check holds its approver ({self.approver})",
        )
        self._check(
            self.root and os.path.isdir(self.root),
            f"the person's root is held ({self.root})",
        )
        self._check(self.host, f"the host's address is named ({self.host})")

    def begin(self, answer: str, within: int = 180) -> "asyncio.Future[asyncio.subprocess.Process]":
        """Start answering the next question, before the call that asks it:
        the call waits for the person, so they answer beside it."""
        return asyncio.ensure_future(self._start(answer, within))

    async def _start(self, answer: str, within: int) -> asyncio.subprocess.Process:
        return await asyncio.create_subprocess_exec(
            self.approver, "next", "--root", self.root, "--relay", self.host,
            "--answer", answer, "--within", str(within),
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
        )

    async def settle(self, started: "asyncio.Future[asyncio.subprocess.Process]", within: int = 180) -> list[dict]:
        """The questions the process saw, once it answered or gave up."""
        process = await started
        try:
            out, err = await asyncio.wait_for(process.communicate(), timeout=within + 30)
        except TimeoutError:
            process.kill()
            out, err = await process.communicate()
        questions = []
        for line in out.decode().splitlines():
            line = line.strip()
            if not line.startswith("{"):
                continue
            seen = json.loads(line)
            if seen.get("question"):
                questions.append(seen["question"])
            if "watched" in seen and seen["watched"]:
                raise AssertionError(f"the person was asked {seen['watched']} unexpected questions")
        if process.returncode not in (0, 1, 3):
            raise AssertionError(f"the approver stopped ({process.returncode}): {err.decode()[-800:]}")
        self.asked.extend(questions)
        return questions

    async def asked_nothing(self) -> None:
        """Say that the host has no question for the person: a question left
        waiting is still there to be seen, and the call answers nothing until
        the person decides."""
        questions = await self.settle(self.begin("none", within=2), within=2)
        self._check(not questions, f"the person was asked nothing ({questions})")

    def answered(self) -> int:
        return len(self.asked)


def _raise(condition: object, what: str) -> None:
    if not condition:
        raise AssertionError(what)

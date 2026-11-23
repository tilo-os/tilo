"""The person answering on their page's question sheet.

A person's page holds their identity, so the questions their agents' hosts
ask them are drawn on its sheet and answered there, in their browser
(integration/person.mjs --answer). A check drives the page through its FIFO
and reads what the page records:

    allow NAME   answer the next question, allowing it once
    deny NAME    answer the next question, refusing it
    none NAME    answer nothing; record whether a question came
    pair WORKSPACE
                 sign a browser of theirs in through a ticket; it opens WORKSPACE

Each recorded line carries `shown`: what the sheet drew, as the page's
accessibility tree names it (the question's title, its detail, its facts),
which is what the person reads.

    person = PagePerson(ASKS, ANSWERS)
    started = person.begin("allow", "submission-denied")
    await person.settle(started)   once the call that asked it answered
"""

from __future__ import annotations

import asyncio
import json
import time


class PagePerson:
    """The person a check holds, answering on their page's sheet."""

    def __init__(self, asks: str, answers: str, check=None) -> None:
        self.asks = asks
        self.answers = answers
        # Every answer the page recorded, and the named questions among them.
        self.seen: list[dict] = []
        self.asked: list[dict] = []
        self._check = check or _raise
        self._watches = 0
        self._check(
            self.asks and self.answers,
            "the check names the page's asks and answers (the person's page is open, answering)",
        )

    def begin(self, answer: str, name: str) -> tuple[str, float]:
        """Ask the page for `name`'s answer, before the call that asks it:
        the call waits for the person, so they answer beside it."""
        with open(self.asks, "w") as asks:
            asks.write(f"{answer} {name}\n")
        return name, time.perf_counter()

    async def settle(self, started: tuple[str, float], within: int = 180) -> list[dict]:
        """The question the page recorded for `name`, once it answered it. A
        `none` begun before a call is the question (if any) the page drew
        without answering: none at all is the answer to "asked nothing"."""
        name, began = started
        deadline = time.monotonic() + within
        while time.monotonic() < deadline:
            for seen in self._recorded():
                if seen.get("name") != name:
                    continue
                seen["seconds"] = round(time.perf_counter() - began, 2)
                self.seen.append(seen)
                if seen.get("answer") == "none":
                    return [seen] if seen.get("shown") else []
                self.asked.append(seen)
                return [seen]
            await asyncio.sleep(0.2)
        raise AssertionError(
            f"the person's page did not answer {name} in {within} s ({self.answers})"
        )

    async def asked_nothing(self) -> None:
        """Say that the host has no question for the person: the page watches
        two seconds and draws none."""
        self._watches += 1
        questions = await self.settle(self.begin("none", f"nothing-{self._watches}"), within=20)
        self._check(not questions, f"the person was asked nothing ({questions})")

    def answered(self) -> int:
        return len(self.asked)

    def sign_in(self, workspace: str, within: int = 180) -> dict:
        """The person signs a browser of theirs in through a ticket: the page
        that holds their root shows the bar naming the browser and what
        certifying it makes it, the person presses Sign in, and the browser
        then opens `workspace` (`person.mjs --pair`)."""
        self.begin("pair", workspace)
        deadline = time.monotonic() + within
        while time.monotonic() < deadline:
            for said in self._lines():
                made = said.get("pair") if isinstance(said, dict) else None
                if isinstance(made, dict) and made.get("workspace") == workspace:
                    self._check(
                        made.get("signedIn") and made.get("shown"),
                        f"the person's page signed the browser in for {workspace} ({made})",
                    )
                    return made
            time.sleep(0.2)
        raise AssertionError(
            f"the person's page signed no browser in for {workspace} in {within} s ({self.answers})"
        )

    def _lines(self) -> list[dict]:
        try:
            with open(self.answers) as answered:
                lines = answered.read().splitlines()
        except FileNotFoundError:
            return []
        said = []
        for line in lines:
            try:
                seen = json.loads(line)
            except ValueError:
                continue
            if isinstance(seen, dict):
                said.append(seen)
        return said

    def _recorded(self) -> list[dict]:
        return [seen for seen in self._lines() if seen.get("name")]


def _raise(condition: object, what: str) -> None:
    if not condition:
        raise AssertionError(what)

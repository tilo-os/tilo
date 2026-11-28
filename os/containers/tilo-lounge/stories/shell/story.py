"""A user story walked in Tilo shells: each person a headless Tilo session of their own.

The shell is where a person works, so a story is walked there: every person is one headless
session (`real_apps.Session`, the tree's nested-session build with the tree's apps, on a scratch
home), driven through the session's own control socket as a person drives it (its actions,
pointer and keys), and every claim a step makes is read back off the shells' own state (the state
dump, the controls each shell shows and their accessible names), never off a model or the thing
that asked.

    story = Story("invite-live", out)
    ada = story.person("Ada")
    story.step("Ada opens Tilo", lambda: ada.start())
    ...
    story.finish()          # story.json, timings.json, each person's video and its captions

Each step is timed against its budget (budgets.json, by the step's words). After a step, the
shells it named are snapshotted; when it fails, every shell is, beside each shell's state and the
end of its session's log. While the story walks, each running shell is snapshotted every
`FRAME_SECONDS`, and those frames become that person's video (webm, with the steps as WebVTT
captions), so a recording is watched like any other. What is written is the shape the results
page (page.py) and the judge (judge.py) read for every story.
"""

from __future__ import annotations

import contextlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import traceback
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
sys.path.insert(0, str(REPO / "scripts/headless-examples"))

from real_apps import BUILD, Failed, Session, address, prepare, run, until  # noqa: E402,F401

BUDGETS = json.loads((HERE.parent / "budgets.json").read_text())
# How often a running shell is filmed. A snapshot of a software-rendered shell takes a while of
# its own, so the pace is what a person watching needs, not a screen's frame rate.
FRAME_SECONDS = 0.4
# What a person's computer has before they open Tilo: the shell, which every session runs, and
# the apps the stories' steps open (their tilolang packages). Any other package a story's steps
# open is named by the story.
APPS = "files settings"
# How the shell names a person's own face on the bar: their name, then what pressing it opens —
# their own card, or on the canvas the menu that holds the card and the styles.
FACE_CARD = ": open your card"
FACE_MENU = ": style and workspaces"
# What the shell calls a workspace and a strip nobody named (session-core's `names`): the record
# stores no name for what a person left unnamed, so each person reads its place in their words.
UNNAMED_WORKSPACE = "Workspace"
UNNAMED_STRIP = "Strip {number}"


def slug(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")[:60]


def duration(ms: float) -> str:
    return f"{round(ms)} ms" if ms < 1000 else f"{ms / 1000:.1f} s"


class StoryStopped(Exception):
    """A step failed: it is recorded, and the story ends there."""


class Person:
    """One person: a headless session of their own, on a state directory of their own."""

    def __init__(self, story: "Story", name: str):
        self.story = story
        self.name = name
        self.key = slug(name)
        # A session's state is the files layer's, which keeps it only on a local POSIX filesystem
        # (a check's output directory may be a shared mount): it lives in the temporary directory,
        # and what a person reads the story by (its logs) is kept beside the story.
        self.state = Path(tempfile.mkdtemp(prefix=f"story-{self.key}-")) / "state"
        self.logs = story.out / "people" / self.key
        self.logs.mkdir(parents=True, exist_ok=True)
        self.session = Session(self.state)
        self.frames = story.out / "frames" / self.key
        self.frames.mkdir(parents=True, exist_ok=True)
        # Each kept frame: its file and the wall-clock second it was taken at.
        self.kept: list[tuple[str, float]] = []
        self.prepared = False
        self.packages: list[Path] = []
        self.starts = 0

    def prepare(self, apps: str = APPS, tilolang: tuple[str, ...] = ()) -> None:
        """Install the apps into this person's state, as their computer has them before they
        first open Tilo: those named in `apps` (directories under apps/) and the tilolang apps
        named in `tilolang` (directories under apps/tl), each built as its signed package against
        the runtime the state holds. The native apps are installed once; the tilolang packages
        are built the first time any are asked for, so a person prepared ahead of their start
        still gets the tilolang apps that start names."""
        if not self.prepared:
            prepare(self.state, apps=apps)
            self.prepared = True
        if tilolang and not self.packages:
            self.packages = self.built(tilolang)

    def built(self, tilolang: tuple[str, ...]) -> list[Path]:
        """The signed package of each tilolang app in `tilolang`, built against the runtime this
        person's state holds."""
        runtime = (self.state / "apps/runtimes/.version").read_text().strip()
        tool = next((p for p in (self.state / "bin/tilolang", BUILD / "bin/tilolang")
                     if p.exists()), None)
        if tool is None:
            raise Failed(f"no tilolang tool in {BUILD / 'bin'} to build {', '.join(tilolang)}")
        built, packages = self.state.parent / "packages", []
        for name in tilolang:
            # Locked: the tree is read-only here, and a build reads each app's ids as they are
            # committed (app.ids) rather than writing them.
            run([str(tool), "build", "--locked", str(REPO / "apps/tl" / name), "--search",
                 str(REPO / "apps/tl"), "--out", str(built), "--tilo-runtime",
                 f"tilo-runtime/{runtime}"])
            package = built / f"{name}.tapp"
            if not package.is_file():
                raise Failed(f"tilolang build made no {package}")
            packages.append(package)
        return packages

    def start(self, *, apps: str = APPS, tilolang: tuple[str, ...] = (), fresh: bool = True,
              env: dict | None = None) -> dict:
        """Open this person's Tilo: their session starts and answers with a frame. Answers
        `{"tookMs": …}`, the session's own time from its start until it answered, which is what a
        person waits for; `launcherMs` is the whole start, with what the development launcher
        installs and checks before the session runs, which no person's computer does."""
        self.prepare(apps, tilolang)
        # A session started again on the same state writes its logs over the last one's: what the
        # last session said is kept beside the story first.
        if self.starts and (self.state / "logs").is_dir():
            shutil.copytree(self.state / "logs", self.logs / f"session-logs-{self.starts}",
                            dirs_exist_ok=True)
        began = time.monotonic()
        self.starts += 1
        tag = self.key if self.starts == 1 else f"{self.key}-{self.starts}"
        self.session.start(apps=apps, fresh=fresh, out=self.logs, tag=tag, env=env,
                           tapps=self.packages)
        launcher = (time.monotonic() - began) * 1000
        said = self.state / "logs/ready.log"

        def answered():
            found = re.search(r"answers after (\d+) ms",
                              said.read_text(errors="replace") if said.is_file() else "")
            return found and int(found.group(1))

        return {"tookMs": until("the session to say how long it took to answer", answered, 10, 0.05),
                "launcherMs": launcher}

    def stop(self) -> None:
        self.session.stop()

    def forget(self) -> None:
        """Stop the session, keep its own logs beside the story, and remove its state."""
        self.stop()
        if (self.state / "logs").is_dir():
            shutil.copytree(self.state / "logs", self.logs / "session-logs", dirs_exist_ok=True)
        shutil.rmtree(self.state.parent, ignore_errors=True)

    @property
    def running(self) -> bool:
        return self.session.running()

    # -- what the shell shows, read off the shell itself --

    def targets(self) -> list[dict]:
        return self.session.targets()

    def names(self) -> list[str]:
        """The names of the controls this person's shell shows now."""
        return [target.get("name") or "" for target in self.targets()]

    def shows(self, text: str) -> bool:
        """Whether a control this shell shows is named with `text` in it."""
        return any(text in name for name in self.names())

    def window_rect(self, app: str) -> list | None:
        """Where this shell draws the window of `app` (its application id), or `None` while it
        has none."""
        found = next((w for w in self.session.windows() if w.get("app_id") == app), None)
        return found and found.get("rect")

    def target(self, name: str, exact: bool = True, inside: str | None = None) -> dict | None:
        """The control `name` this shell shows; with `inside`, the one drawn in the window of that
        app (by its application id), as a person looks for it in that window."""
        area = self.window_rect(inside) if inside else None
        if inside and not area:
            return None

        def within(target: dict) -> bool:
            if not area:
                return True
            x, y, _, _ = target.get("rect") or (0, 0, 0, 0)
            left, top, width, height = area
            return left <= x < left + width and top <= y < top + height

        return next((t for t in self.targets()
                     if (t.get("name") == name if exact else name in (t.get("name") or ""))
                     and within(t)), None)

    def press(self, name: str, what: str | None = None, deadline: float = 30.0,
              exact: bool = True, inside: str | None = None) -> dict:
        """Press the control `name` where this shell draws it, once the shell shows it, as a
        person points at it and clicks; with `inside`, the one in the window of that app."""
        what = what or f"{name!r} on {self.name}'s shell"
        target = until(what, lambda: self.target(name, exact, inside), deadline, 0.2)
        # A control that has just appeared may still be moving into place (a card sliding in):
        # a person clicks where it comes to rest, so its place is read until two looks agree.
        last = target.get("rect")

        def settled():
            nonlocal last
            now = self.target(name, exact, inside)
            rect = now and now.get("rect")
            if rect and rect == last:
                return now
            last = rect
            return None

        target = until(f"{what} to come to rest", settled, 5, 0.15)
        x, y, width, height = target["rect"]
        self.session.click_at(x + width / 2, y + height / 2)
        return target

    def name_self(self, name: str) -> None:
        """Give this person the name `name` as a person does: the session renames the person at
        this device, which is the one action their own card's name field runs. Read back off the
        bar and the identity record the rename keeps it in."""
        def rename() -> dict:
            rows = self.session.actions("set-name")
            found = next((row for row in rows if address(row) == ("session:people", "set-name")),
                         None)
            if found is None:
                raise Failed(f"{self.name}'s session offers no rename of the person at this "
                             f"device; it lists "
                             f"{[row.get('title') or row.get('name') for row in rows][:20]}")
            self.session.invoke_row(found, {"name": name})
            return found

        self.story.note(f"{self.key}-rename", str(rename().get("title") or "set-name"))
        until(f"{self.name}'s bar naming them",
              lambda: any(self.shows(named) for named in face_names(name)), 15, 0.2)
        record = self.state / "config/tilo/identity/primary.json"
        kept = json.loads(record.read_text()).get("display_name") if record.is_file() else None
        if kept != name:
            raise Failed(f"{self.name}'s identity keeps the name {kept!r}, not {name!r}")

    def snapshot(self, path: Path) -> Path | None:
        try:
            return self.session.snapshot(path)
        except Exception as error:  # noqa: BLE001 - a lost shot never stops the story
            print(f"  (no snapshot of {self.name}'s shell: {error})", file=sys.stderr)
            return None

    def evidence(self) -> dict:
        """What this shell says of itself now: its state, controls and windows."""
        seen: dict = {}
        for key, read in (("controls", self.session.targets), ("windows", self.session.windows),
                          ("state", self.session.state_dump)):
            try:
                seen[key] = read()
            except Exception as error:  # noqa: BLE001
                seen[key] = f"unreadable: {error}"
        return seen

    def log_tail(self, lines: int = 150) -> str:
        """The end of what this person's session said: its launcher's output and the session's
        own log."""
        said = []
        for log in [self.session.launcher_log, self.state / "logs/session.log"]:
            if log is not None and log.is_file():
                text = log.read_text(errors="replace").splitlines()[-lines:]
                said.append(f"=== {self.name}: {log.name}\n" + "\n".join(text))
        return "\n\n".join(said) or f"=== {self.name}: no session log"


# -- what every shell story reads off a shell, and does on it --

SHELL = "so.tilo.session"
CHROME = "desktop:chrome"


def tree(one: Person) -> dict:
    return (one.session.state_dump() or {}).get("workspace_tree") or {}

def workspaces(one: Person) -> list[dict]:
    return tree(one).get("workspaces") or []

def front(one: Person) -> dict | None:
    """The workspace in front on this person's shell."""
    held = workspaces(one)
    active = tree(one).get("active")
    return next((w for w in held if w.get("id") == active), held[0] if held else None)

def workspace_names(one: Person) -> dict[str, str]:
    """Each workspace this person's computer keeps, by the name their shell calls it. The shell's
    own layout carries no names (the workspace's record holds the one every device reads), so
    they are read from the session's answer about what this computer keeps."""
    answer = (one.session.control({"op": "keeping_devices"}) or {}).get("answer") or {}
    return {(row.get("workspace") or ""): (row.get("name") or "").strip() or UNNAMED_WORKSPACE
            for row in answer.get("workspaces") or []}

def front_name(one: Person) -> str | None:
    """The name this person's bar gives the workspace in front: the workspace control on the bar
    is named after it. None while the shell holds no workspace."""
    held = front(one)
    return workspace_names(one).get(held.get("id")) if held else None

def bar_workspace(one: Person) -> str | None:
    """The workspace the bar says this person is in, read off the control it draws for it."""
    name = front_name(one)
    return name if name and one.target(name) else None

def in_front_named(one: Person, name: str) -> bool:
    """Whether this person's shell says the workspace in front is `name`: its bar draws the
    workspace control under that name."""
    return bar_workspace(one) == name

def strips_in_front(one: Person) -> list[dict]:
    """Every strip of the workspace in front, by the name it was given and its place."""
    return (front(one) or {}).get("strips") or []

def front_strip(one: Person) -> dict | None:
    """The strip in front in the workspace in front, and its place among that workspace's
    strips."""
    held = front(one)
    if not held:
        return None
    strips = held.get("strips") or []
    current = (tree(one).get("active_strip") or {}).get(held.get("id"))
    at = next((index for index, strip in enumerate(strips) if strip.get("id") == current), None)
    if at is None:
        # The strip recorded for the workspace is gone or was never recorded: the shell shows
        # the first, as the layout's own rule does.
        if not strips:
            return None
        at = 0
    return {**strips[at], "at": at}

def front_strip_name(one: Person) -> str | None:
    """The name this person's bar gives the strip in front: the strip control on the bar is named
    after it, from the name it was given or the place it stands."""
    strip = front_strip(one)
    if strip is None:
        return None
    return (strip.get("name") or "").strip() or UNNAMED_STRIP.format(number=strip["at"] + 1)

def bar_strip(one: Person) -> str | None:
    """The strip the bar says this person is on, read off the control it draws for it."""
    name = front_strip_name(one)
    return name if name and one.target(name) else None

def face(one: Person) -> str | None:
    """What this person's own face on the bar is named: their name, then what pressing it
    opens."""
    return next((name for name in one.names()
                 if name.endswith(FACE_CARD) or name.endswith(FACE_MENU)), None)

def face_names(name: str) -> tuple[str, ...]:
    """What a person's face on the bar is named once they gave themselves `name`."""
    return (f"{name}{FACE_CARD}", f"{name}{FACE_MENU}")

def peers(one: Person, workspace: str) -> tuple[dict, list[dict]]:
    """This person's own row, and everyone else here in `workspace`, as their shell's people
    list draws them."""
    seen = (one.session.state_dump() or {}).get("peers") or {}
    own = seen.get("own") or {}
    for live in seen.get("workspaces") or []:
        if live.get("workspace") == workspace:
            members = [m for m in live.get("members") or []
                       if m.get("publisher") != own.get("publisher")]
            return own, members
    return own, []

def actions_matching(one: Person, query: str) -> list[dict]:
    """The actions this shell lists for `query`, each by provider, address and name."""
    return [{k: row.get(k) for k in ("provider", "id", "name", "title", "args", "contract")
             if row.get(k) is not None} for row in one.session.actions(query)]

def stand_in(one: Person, workspace: str) -> None:
    current = front(one)
    if current is None or current.get("id") != workspace:
        one.session.invoke(SHELL, CHROME, "switch-workspace", {"workspace": workspace})
    until(f"{one.name}'s shell to stand in the workspace",
          lambda: (front(one) or {}).get("id") == workspace, 60, 0.25)


def invite_link(one: Person) -> str:
    """A new link into the workspace in front, made as a person makes one: the session's own
    show-invite opens the invite sheet, Create link (Create another link, once it made one) makes
    it, and Copy link puts it on the clipboard, where it is read. The sheet stays open, showing
    the link, until the person is done with it. Failed, with what the sheet says, when it makes
    no link."""
    if not one.target("Create another link") and not one.target("Create link"):
        one.session.invoke(SHELL, "session:invite", "show-invite", {})

    def said() -> list[str]:
        shown = [n for n in one.names() if n.startswith("No link") or "can't invite" in n]
        if shown:
            return shown
        # The sheet's refusal is a line of text, no control: the session logs the same reason.
        try:
            log = one.session.log()
        except OSError:
            return []
        return ["No link was made: " + line.split("reason=", 1)[1].strip().strip('"')
                for line in log.splitlines()
                if "the invitation was not handed over" in line and "reason=" in line][-1:]

    def clipboard() -> str:
        return ((one.session.state_dump() or {}).get("clipboard") or "").strip()

    try:
        create = until(f"Create link on {one.name}'s invite sheet", lambda: next(
            (name for name in ("Create another link", "Create link") if one.target(name)), None),
            15, 0.2)
    except Failed:
        raise Failed(f"{one.name}'s invite sheet offers no Create link"
                     + (f": it says {said()}" if said() else "")) from None
    before = clipboard()
    one.press(create, f"{create} on {one.name}'s invite sheet")

    def copied():
        if said():
            return said()
        if one.target("Making the link…") or not one.target("Copy link"):
            return None
        # Copy link copies the link the sheet shows; until the new one is shown, that is the last.
        one.press("Copy link", f"Copy link under the link {one.name}'s sheet made")
        text = clipboard()
        return text if "://" in text and text != before else None

    try:
        got = until(f"a new link from {one.name}'s invite sheet on their clipboard", copied, 30, 0.3)
    except Failed:
        got = said()
    if isinstance(got, list) or not got:
        raise Failed(f"{one.name}'s invite sheet made no link to copy"
                     + (f": it says “{got[0]}”" if got else ""))
    return got


class Story:
    def __init__(self, story_id: str, out: Path):
        self.id = story_id
        self.out = Path(out).resolve()
        for sub in ("shots", "evidence"):
            (self.out / sub).mkdir(parents=True, exist_ok=True)
        self.began = time.time()
        self.steps: list[dict] = []
        self.timings: dict[str, int] = {}
        self.measures: list[dict] = []
        self.people: list[Person] = []
        self.stopped: dict | None = None
        self._filming = True
        self._film = threading.Thread(target=self._filmer, daemon=True)
        self._film.start()

    def person(self, name: str) -> Person:
        one = Person(self, name)
        self.people.append(one)
        return one

    def budget_of(self, title: str) -> int:
        budget = (BUDGETS.get("steps", {}).get(self.id) or {}).get(title)
        if not isinstance(budget, (int, float)):
            raise Failed(f'budgets.json gives the {self.id} step "{title}" no budget')
        return int(budget)

    def _filmer(self) -> None:
        """Every running shell, snapshotted in turn every FRAME_SECONDS, for its person's video."""
        while self._filming:
            began = time.monotonic()
            for one in list(self.people):
                if not self._filming or not one.running:
                    continue
                path = one.frames / f"{len(one.kept):05d}.png"
                at = time.time()
                try:
                    one.session.control({"op": "snapshot", "path": str(path)}, timeout=10)
                    until("the frame", lambda: path.is_file() and path.stat().st_size > 0, 5, 0.05)
                except Exception:  # noqa: BLE001 - a session starting or stopping draws none
                    path.unlink(missing_ok=True)
                    continue
                one.kept.append((path.name, at))
            time.sleep(max(0.0, FRAME_SECONDS - (time.monotonic() - began)))

    def step(self, title: str, run: Callable[[], object], *, people: list[Person] | None = None,
             budget_key: str | None = None, expected_errors: tuple[str, ...] = ()):
        """One step: `run` does what the person does and waits for what they would wait for, and
        raises Failed with what did not happen. It may answer `{"tookMs": ms}` when the time a
        person waited is not the step's whole time. `people` are the shells the step's shots
        show (default: every running one). All running shells are checked for error banners.
        An intentional error can be expected for this step by its exact text, title and body
        joined with " — " when both are present."""
        if self.stopped:
            raise StoryStopped(self.stopped.get("why", ""))
        budget = self.budget_of(budget_key or title)
        index = len(self.steps) + 1
        record = {"index": index, "title": title, "budgetMs": budget,
                  "atMs": round((time.time() - self.began) * 1000)}
        self.steps.append(record)
        began = time.monotonic()
        value, error = None, None
        try:
            value = run()
        except Exception as thrown:  # noqa: BLE001 - every failure is the step's to record
            error = thrown
        took = value.get("tookMs") if isinstance(value, dict) else None
        record["ms"] = round(took if isinstance(took, (int, float)) else
                             (time.monotonic() - began) * 1000)
        if error is None:
            try:
                self.check_error_banners(expected_errors)
            except Exception as thrown:  # noqa: BLE001 - an unreadable shell cannot pass
                error = thrown
        over = error is None and record["ms"] > budget
        record["status"] = "failed" if error else "over budget" if over else "ok"
        shown = [one for one in (self.people if error else people or self.people) if one.running]
        # Shots the step took while it ran ([`Story.shot`]) come before the one of where it ended.
        record.setdefault("shots", [])
        for one in shown:
            file = f"shots/{index:02d}-{slug(title)}-{one.key}.png"
            if one.snapshot(self.out / file):
                record["shots"].append({"person": one.name, "file": file})
        if error or over:
            record["why"] = (str(error) if error else
                             f"took {duration(record['ms'])} of a {duration(budget)} budget")
            base = f"evidence/{index:02d}-{slug(title)}"
            (self.out / f"{base}-shells.json").write_text(json.dumps(
                {one.name: one.evidence() for one in self.people if one.running}, indent=1))
            (self.out / f"{base}-sessions.log").write_text("\n\n".join(
                one.log_tail() for one in self.people))
            record["evidence"] = {"shells": f"{base}-shells.json",
                                  "sessions": f"{base}-sessions.log"}
            if error is not None:
                (self.out / f"{base}-error.txt").write_text(
                    "".join(traceback.format_exception(error)))
        print(f"STEP {index} {record['status'].upper()} {record['ms'] / 1000:.1f}s/"
              f"{budget / 1000:.1f}s {title}" + (f": {record['why']}" if record.get("why") else ""),
              flush=True)
        if error is not None:
            print("".join(traceback.format_exception(error)), flush=True)
            self.stopped = record
            raise StoryStopped(record["why"])
        return value

    def check_error_banners(self, expected: tuple[str, ...]) -> None:
        unexpected = []
        for one in self.people:
            if not one.running:
                continue
            for notice in one.session.state_dump()["notifications"]:
                if notice["banner"] and notice["severity"] == "error":
                    message = " — ".join(part for part in (notice["title"], notice["body"]) if part)
                    if message not in expected:
                        unexpected.append(f"{one.name}: {message}")
        if unexpected:
            raise Failed("Unexpected error notice: " + "; ".join(unexpected))

    def timing(self, name: str, ms: float) -> None:
        self.timings[name] = round(ms)

    def shot(self, person: Person, what: str) -> None:
        """A shot of `person`'s shell in the middle of the running step, as `what` names that
        moment: a step that ends somewhere else than what it shows on the way (a question, before
        it is answered) keeps both."""
        record = self.steps[-1]
        file = f"shots/{record['index']:02d}-{slug(record['title'])}-{slug(what)}-{person.key}.png"
        if person.snapshot(self.out / file):
            record.setdefault("shots", []).append({"person": person.name, "file": file})

    def note(self, name: str, value) -> None:
        """Something a step learned that whoever reads the recording wants: kept beside it."""
        (self.out / "evidence" / f"{slug(name)}.json").write_text(json.dumps(value, indent=1))

    def finish(self, error: BaseException | None = None) -> dict:
        """Stop filming and every shell, encode each person's video, and write story.json."""
        time.sleep(1.5)  # a last look at every shell, so the recording ends on what they saw
        self._filming = False
        self._film.join(15)
        for one in self.people:
            with contextlib.suppress(Exception):
                one.forget()
        videos = []
        for one in self.people:
            video = self._encode(one)
            if video:
                videos.append({"person": one.name, "file": video, "captions": self._captions(one, video)})
        shutil.rmtree(self.out / "frames", ignore_errors=True)
        failed = (next((s for s in self.steps if s["status"] == "failed"), None)
                  or next((s for s in self.steps if s["status"] != "ok"), None))
        story = {
            "id": self.id,
            "at": datetime.fromtimestamp(self.began, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "seconds": round(time.time() - self.began),
            "walk": "shell",
            "completed": self.stopped is None and error is None,
            "withinBudget": all(s["status"] == "ok" for s in self.steps)
            and all(m["ok"] for m in self.measures),
            "brokeAt": ({"index": failed["index"], "title": failed["title"], "why": failed.get("why")}
                        if failed else None),
            "error": "".join(traceback.format_exception(error))[:4000] if error else None,
            "steps": self.steps,
            "timings": self.timings,
            "measures": self.measures,
            "videos": videos,
        }
        (self.out / "story.json").write_text(json.dumps(story, indent=2, ensure_ascii=False))
        (self.out / "timings.json").write_text(json.dumps({
            "story": self.id,
            "at": story["at"],
            "revision": os.environ.get("T_COMMIT") or os.environ.get("SOURCE_REVISION"),
            "steps": [{k: s.get(k) for k in ("title", "ms", "budgetMs", "status")} for s in self.steps],
            "moments": self.timings,
        }, indent=2, ensure_ascii=False))
        return story

    def _encode(self, one: Person) -> str | None:
        """A person's frames as a video at the pace they were taken, or None without ffmpeg (the
        step shots still tell the story)."""
        if len(one.kept) < 2:
            return None
        if shutil.which("ffmpeg") is None:
            print("no ffmpeg: the recordings stay snapshots", file=sys.stderr)
            return None
        lines = ["ffconcat version 1.0"]
        for at, (file, when) in enumerate(one.kept):
            following = one.kept[at + 1][1] if at + 1 < len(one.kept) else when + 1
            lines += [f"file '{file}'", f"duration {max(0.01, following - when):.3f}"]
        lines.append(f"file '{one.kept[-1][0]}'")
        (one.frames / "frames.txt").write_text("\n".join(lines) + "\n")
        (self.out / "video").mkdir(exist_ok=True)
        file = f"video/{one.key}.webm"
        made = subprocess.run(
            ["ffmpeg", "-y", "-loglevel", "error", "-f", "concat", "-safe", "0",
             "-i", str(one.frames / "frames.txt"),
             # Snapshots carry an alpha channel, which libvpx refuses to encode beside its
             # default alternate reference frames: the video is opaque, as the screen is.
             "-vf", "fps=10,scale=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p",
             "-c:v", "libvpx", "-b:v", "800k",
             "-deadline", "realtime", "-cpu-used", "8", str(self.out / file)],
            capture_output=True, text=True)
        if made.returncode != 0:
            print(f"encoding {one.name}'s video failed: {made.stderr[-600:]}", file=sys.stderr)
            return None
        return file

    def _captions(self, one: Person, video: str) -> str | None:
        """The steps as captions over a person's video: each step's words from when it began
        until the next one began."""
        start = one.kept[0][1]
        end = one.kept[-1][1] - start + 1

        def clock(seconds: float) -> str:
            ms = round(max(0.0, seconds) * 1000)
            return (f"{ms // 3_600_000:02d}:{ms // 60_000 % 60:02d}:"
                    f"{ms // 1000 % 60:02d}.{ms % 1000:03d}")

        cues = []
        for at, step in enumerate(self.steps):
            begin = self.began + step["atMs"] / 1000 - start
            following = self.steps[at + 1] if at + 1 < len(self.steps) else None
            finish = self.began + following["atMs"] / 1000 - start if following else end
            result = "" if step["status"] == "ok" else f" ({step['status']})"
            if finish > 0 and begin < end and finish > begin:
                cues.append(f"{clock(begin)} --> {clock(finish)}\n{step['index']}. {step['title']}{result}")
        if not cues:
            return None
        file = video.removesuffix(".webm") + ".vtt"
        (self.out / file).write_text("WEBVTT\n\n" + "\n\n".join(cues) + "\n")
        return file


# The account a story's people use the machine as, when the story is walked by root (a check's
# VM): a session runs as the person signed in, never as root, as it does on a Tilo device.
PERSON_ACCOUNT = "tilo-person"


def become_person(out: Path) -> None:
    """Walk the rest of the story as an ordinary account, when it was started as root.

    Everything a person does (their session, the tools they run, the files they make) is that
    account's, so their programs run as they would on a person's computer: a web engine refuses
    to run as root with its sandbox, and the session answers only its own account's programs.
    The story keeps the means to act as the machine (`machine()`) for what stands in for the world
    around the person (a provider's names and certificate)."""
    if os.geteuid() != 0:
        return
    import pwd

    try:
        account = pwd.getpwnam(PERSON_ACCOUNT)
    except KeyError:
        subprocess.run(["useradd", "--create-home", "--user-group", PERSON_ACCOUNT], check=True)
        account = pwd.getpwnam(PERSON_ACCOUNT)
    # What this run made for whoever uses the machine is the person's: the story's own folder,
    # the test environment's fresh home, temporary and runtime directories, the run's audio
    # endpoint (its socket and cookie), and the shader cache the session draws its first frame
    # through (made by the runner as root, so a person's session cannot write it otherwise).
    owned = [Path(out)]
    for name in ("TILO_TEST_ENVIRONMENT", "PULSE_RUNTIME_PATH", "PULSE_STATE_PATH",
                 "PULSE_COOKIE", "MESA_SHADER_CACHE_DIR"):
        if os.environ.get(name):
            owned.append(Path(os.environ[name]))
    for top in owned:
        if not top.exists():
            continue
        for path in [top, *(top.rglob("*") if top.is_dir() else [])]:
            os.chown(path, account.pw_uid, account.pw_gid, follow_symlinks=False)
    # A person mounts their workspace and their cloud accounts' folders through FUSE, which
    # a Fedora machine's udev opens to every account. A check's machine runs no udev, so the
    # device keeps the kernel's root-only mode until the machine opens it the same way.
    fuse = Path("/dev/fuse")
    if fuse.exists():
        fuse.chmod(0o666)
    os.initgroups(PERSON_ACCOUNT, account.pw_gid)
    # Root stays the saved id, so `machine()` can take it back; what this starts runs as the
    # person alone, since a program started keeps only the id it runs as.
    os.setresgid(account.pw_gid, account.pw_gid, 0)
    os.setresuid(account.pw_uid, account.pw_uid, 0)
    os.environ.update(USER=PERSON_ACCOUNT, LOGNAME=PERSON_ACCOUNT)
    if not os.environ.get("TILO_TEST_ENVIRONMENT"):
        # Outside a test environment, root's home and login runtime are not the person's: theirs
        # is their own home, and the session picks its own runtime directory.
        os.environ["HOME"] = account.pw_dir
        if os.environ.get("XDG_RUNTIME_DIR", "").rstrip("/") in ("/run/user/0", ""):
            os.environ.pop("XDG_RUNTIME_DIR", None)


def can_act_as_machine() -> bool:
    """Whether the story may act as the machine, to stand in for the world around its people."""
    return os.geteuid() == 0 or os.getresuid()[2] == 0


@contextlib.contextmanager
def machine():
    """Act as the machine (root) for a moment, for what stands in for the world around the
    story's people, then as the person again. Nothing a person does runs inside it."""
    if os.geteuid() == 0:
        yield
        return
    if not can_act_as_machine():
        raise Failed("the story acts as this machine for the world around its people, which "
                     "takes root")
    uid, gid = os.geteuid(), os.getegid()
    os.seteuid(0)
    os.setegid(0)
    try:
        yield
    finally:
        os.setegid(gid)
        os.seteuid(uid)


def walk(story_id: str, out: Path, steps: Callable[[Story], None]) -> int:
    """Walk a story: `steps` takes its steps; the story is written whatever happens. Exits 0
    when every step finished within its budget."""
    become_person(Path(out))
    story = Story(story_id, out)
    error = None
    try:
        steps(story)
    except StoryStopped:
        pass
    except BaseException as thrown:  # noqa: BLE001 - written down, then the story ends
        error = thrown
        traceback.print_exc()
    result = story.finish(error)
    print(json.dumps({k: result[k] for k in ("completed", "withinBudget", "brokeAt", "timings")},
                     indent=2, ensure_ascii=False))
    return 0 if result["completed"] and result["withinBudget"] else 1

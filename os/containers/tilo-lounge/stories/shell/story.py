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
import threading
import time
import traceback
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
sys.path.insert(0, str(REPO / "scripts/headless-examples"))

from real_apps import APPS, Failed, Session, prepare, until  # noqa: E402,F401

BUDGETS = json.loads((HERE.parent / "budgets.json").read_text())
# How often a running shell is filmed. A snapshot of a software-rendered shell takes a while of
# its own, so the pace is what a person watching needs, not a screen's frame rate.
FRAME_SECONDS = 0.4


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
        self.state = story.out / "people" / self.key / "state"
        self.logs = story.out / "people" / self.key
        self.session = Session(self.state)
        self.frames = story.out / "frames" / self.key
        self.frames.mkdir(parents=True, exist_ok=True)
        # Each kept frame: its file and the wall-clock second it was taken at.
        self.kept: list[tuple[str, float]] = []
        self.prepared = False
        self.starts = 0

    def prepare(self, apps: str = APPS) -> None:
        """Install the apps into this person's state, as their computer has them before they
        first open Tilo."""
        if not self.prepared:
            prepare(self.state, apps=apps)
            self.prepared = True

    def start(self, *, apps: str = APPS, fresh: bool = True, env: dict | None = None) -> float:
        """Open this person's Tilo: their session starts and draws its first frame. Answers how
        long that took, in ms."""
        self.prepare(apps)
        began = time.monotonic()
        self.starts += 1
        tag = self.key if self.starts == 1 else f"{self.key}-{self.starts}"
        self.session.start(apps=apps, fresh=fresh, out=self.logs, tag=tag, env=env)
        return (time.monotonic() - began) * 1000

    def stop(self) -> None:
        self.session.stop()

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

    def target(self, name: str, exact: bool = True) -> dict | None:
        return next((t for t in self.targets()
                     if (t.get("name") == name if exact else name in (t.get("name") or ""))), None)

    def press(self, name: str, what: str | None = None, deadline: float = 30.0,
              exact: bool = True) -> dict:
        """Press the control `name` where this shell draws it, once the shell shows it, as a
        person points at it and clicks."""
        target = until(what or f"{name!r} on {self.name}'s shell",
                       lambda: self.target(name, exact), deadline, 0.2)
        x, y, width, height = target["rect"]
        self.session.click_at(x + width / 2, y + height / 2)
        return target

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
        logs = sorted(self.logs.glob("session-*.log"), key=lambda path: path.stat().st_mtime)
        if not logs:
            return "(no session log)"
        text = logs[-1].read_text(errors="replace").splitlines()[-lines:]
        return f"=== {logs[-1].name}\n" + "\n".join(text)


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
             budget_key: str | None = None):
        """One step: `run` does what the person does and waits for what they would wait for, and
        raises Failed with what did not happen. It may answer `{"tookMs": ms}` when the time a
        person waited is not the step's whole time. `people` are the shells the step's shots
        show (default: every running one)."""
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
        over = error is None and record["ms"] > budget
        record["status"] = "failed" if error else "over budget" if over else "ok"
        shown = [one for one in (self.people if error else people or self.people) if one.running]
        record["shots"] = []
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

    def timing(self, name: str, ms: float) -> None:
        self.timings[name] = round(ms)

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
                one.stop()
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
             "-vf", "fps=10,scale=trunc(iw/2)*2:trunc(ih/2)*2", "-c:v", "libvpx", "-b:v", "800k",
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


def walk(story_id: str, out: Path, steps: Callable[[Story], None]) -> int:
    """Walk a story: `steps` takes its steps; the story is written whatever happens. Exits 0
    when every step finished within its budget."""
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

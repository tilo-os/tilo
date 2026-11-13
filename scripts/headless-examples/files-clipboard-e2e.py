#!/usr/bin/env python3
"""Copy and cut files in Files, paste them into another folder, check the bytes.

Files reaches the session's file clipboard through its hosted lane: the desk
answers the capture, the publication and the paste, and the session's paste
engine writes the files. The steps wait for the bytes on disk, because a key
sent while the app is still reading a folder is one the app can only drop; a
step that does not land is taken again from a session that opens on the folder
it starts from.

A person's move is the one driven here: `End` picks the file the folder shows,
Ctrl+C or Ctrl+X takes it, a folder appears beside it as another device's write
would, `Home` and `Enter` open that folder, and Ctrl+V pastes into it.

Usage:
  ./files-clipboard-e2e.py [--build] [--state DIR] [--shots DIR] [--remote-exec PREFIX]

Builds nothing unless `--build` is given (or no nested-session build exists);
`TILO_NESTED_SESSION_BUILD_EXEC` is the build's command prefix, as for
`real_apps.py`.
"""
import argparse
import os
import shutil
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import real_apps as ra

APPS = "notes file-explorer"
TARGET = "0-target"
COPIED = "the file the person copied\n"
CUT = "the file the person cut\n"
# A step waits this many sessions for the paste to land: the first key after a
# window comes up can arrive before the app reads the folder it shows.
TRIES = 3


class Driver:
    """Keys into the desk's own control socket, aimed at the front window."""

    def __init__(self, session: "ra.Session"):
        self.session = session

    def settle(self, rounds: int = 10) -> None:
        """Round trips to the desk, so the app and the session have passed frames."""
        for _ in range(rounds):
            self.session.targets()
        time.sleep(0.3)

    def logs(self) -> str:
        """Everything the session and the script that runs it wrote."""
        path = self.session.state / "logs/session.log"
        text = path.read_text(errors="replace") if path.is_file() else ""
        for path in sorted(self.session.state.glob(f"session-{self.session.tag}.log")):
            text += path.read_text(errors="replace")
        return text

    def answered(self) -> int:
        """The keys the compositor measured an app answering."""
        return sum(1 for line in self.logs().splitlines()
                   if line.startswith("KEY-LATENCY") and "key_to_commit_us" in line)

    def press(self, key: str, *, label: str = "", tries: int = 3) -> bool:
        """One key, retried while the app has not answered it (the first key
        after a window takes focus can land before the app listens)."""
        for _ in range(tries):
            before = self.answered()
            self.session.key(key)
            self.settle()
            if self.answered() > before:
                print(f"--- key {key} {label}: answered")
                return True
            print(f"--- key {key} {label}: no commit")
        return False

    def chord(self, key: str) -> bool:
        """Ctrl held while `key` is pressed."""
        before = self.answered()
        self.session.control({"op": "modifiers", "ctrl": True})
        try:
            self.session.key(key)
        finally:
            self.session.control({"op": "modifiers", "ctrl": False})
        self.settle()
        answered = self.answered() > before
        print(f"--- chord ctrl+{key}: {'answered' if answered else 'no commit'}")
        return answered


def app_home(session: "ra.Session", state: Path) -> Path:
    """The folder a Files window opens on: the app's own home in the room the
    desk's workspace holds, where the launcher mounts it, so the file planted
    here is the file the window lists."""
    workspaces = session.state_dump()["workspace_tree"]["workspaces"]
    room = next((workspace["room"] for workspace in workspaces if workspace.get("room")), None)
    assert room, f"no workspace of the desk names its room: {workspaces!r}"
    home = state / "data/tilo-workspaces" / room / "so.tilo.file-explorer"
    home.mkdir(parents=True, exist_ok=True)
    return home


def clear(home: Path) -> None:
    """What an earlier step left, so the folder shows one file and nothing else
    of ours: the paste target, and a wrong-folder paste's conflict copy."""
    shutil.rmtree(home / TARGET, ignore_errors=True)
    for path in home.glob("zz-*"):
        path.unlink(missing_ok=True)


def land(homes: list[Path], name: str) -> list[Path]:
    return [home / TARGET / name for home in homes if (home / TARGET / name).is_file()]


def paste_into_folder(driver: Driver, state: Path, *, name: str, body: str,
                      cut: bool, shot: Path) -> Path:
    """Take `name` from the folder Files shows, paste it into `0-target`, and
    answer where it landed, with a capture of it there. A step that does not
    land is taken again from a session that opens on the folder it starts from."""
    home = None
    for attempt in range(1, TRIES + 1):
        try:
            driver.session.start(apps=APPS, fresh=True, tag="clipboard")
            if home is None:
                home = app_home(driver.session, state)
            # The folder as the window opens it: the file last in the listing,
            # so `End` is it, and the target folder first, so `Home` is it.
            clear(home)
            (home / name).write_text(body)
            (home / TARGET).mkdir(exist_ok=True)
            driver.session.launch("files")
            driver.session.answering("files")
            driver.session.click_at(640, 400)
            active = [window for window in driver.session.windows() if window.get("active")]
            assert any(window["app_id"] == "so.tilo.file-explorer" for window in active), active
            driver.settle()
            driver.press("End", label="the file shown")
            driver.chord("x" if cut else "c")
            driver.settle(20)
            driver.press("Home", label="the first row")
            driver.press("Enter", label="open the target folder", tries=2)
            driver.settle(20)
            # The folder the window shows is empty now, so a paste that did not
            # land is taken again here: only the paste is repeated, since the
            # keys that took the file and opened the folder have nothing left
            # to act on.
            for paste in range(1, 4):
                driver.chord("v")
                try:
                    landed = ra.until(f"{name} to land in {TARGET}",
                                      lambda: next(iter(land([home], name)), None), 15, 0.25)
                    break
                except ra.Failed:
                    print(f"--- attempt {attempt}: paste {paste} did not land")
            else:
                raise ra.Failed("no paste landed")
            driver.session.snapshot(shot)
            return landed
        except (ra.Failed, AssertionError) as error:
            print(f"--- attempt {attempt}: {error}; again")
        finally:
            driver.session.stop()
    raise ra.Failed(f"the paste of {name} never landed")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--build", action="store_true", help="build the session first")
    parser.add_argument("--state", default=str(Path.home() / ".cache/files-clipboard-e2e"))
    parser.add_argument("--shots", default=None, help="where the captures go")
    parser.add_argument("--remote-exec", default=os.environ.get("TILO_NESTED_SESSION_BUILD_EXEC"))
    options = parser.parse_args()

    state = Path(options.state)
    shots = Path(options.shots or state / "shots")
    shots.mkdir(parents=True, exist_ok=True)
    ra.prepare(state, apps=APPS, built=options.build, remote=options.remote_exec)
    with ra.Session(state) as session:
        driver = Driver(session)
        copied = paste_into_folder(driver, state, name="zz-notes.txt", body=COPIED,
                                   cut=False, shot=shots / "copied.png")
        assert copied.read_bytes() == COPIED.encode(), "the paste changed the bytes"
        kept = copied.parent.parent / "zz-notes.txt"
        assert kept.read_bytes() == COPIED.encode(), "the copy took its source"
        print("COPIED", copied, len(COPIED), "bytes; the source is kept")

        moved = paste_into_folder(driver, state, name="zz-other.txt", body=CUT,
                                  cut=True, shot=shots / "cut.png")
        assert moved.read_bytes() == CUT.encode(), "the paste changed the bytes"
        gone = moved.parent.parent / "zz-other.txt"
        ra.until("the cut to leave its source folder", lambda: not gone.exists(), 30, 0.25)
        print("CUT", moved, len(CUT), "bytes; the source is gone")
        print("PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())

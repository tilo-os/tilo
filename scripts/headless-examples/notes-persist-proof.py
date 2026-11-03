#!/usr/bin/env python3
"""What a person types into a Notes page is still there when the session comes
back.

A session starts on a scratch home with the tree's own apps, the person opens
Notes, makes a page with Ctrl+N and types into it. Nothing is read off the
editor: the proof looks at the Notes record the app keeps in the session's
state, bytes on disk, the way a bug report means "the text was lost". Then the
session stops and starts again on the same state, as it does when a person logs
back in.

  typed    the record the session has of the Notes workspace holds the text
           while the app runs
  stopped  and still holds it once the session has ended, so the writer
           finished rather than leaving the last words in memory
  again    the restarted session reads the same record, and the desk's
           workspace still makes Notes an author there, so the launch it
           made on the first start was the one that could write

It needs the session and the tree's apps built (dist/dev/nested-session-build.sh,
or `TILO_NESTED_SESSION_BUILD` naming where they were).

Usage:
  ./notes-persist-proof.py [--build]
"""
import json
import os
import pathlib
import sys
import tempfile
import uuid

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import real_apps  # noqa: E402
from real_apps import Failed, until  # noqa: E402

NOTES = "so.tilo.notes"
APPS = "notes"
WORKSPACE = pathlib.Path(os.environ.get("TILO_NOTES_PROOF_STATE") or tempfile.mkdtemp(
    prefix="tilo-notes-persist-"))
SHOTS = pathlib.Path(os.environ.get("TILO_NOTES_PROOF_SHOTS")
                     or tempfile.mkdtemp(prefix="tilo-notes-persist-shots-"))
TEXT = f"Kestrel {uuid.uuid4().hex[:8]}"
LINE = "the second line of the page"

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


def records() -> list[pathlib.Path]:
    """The Notes app's records, as the session keeps them in its state."""
    found = [path for path in WORKSPACE.rglob("*.automerge") if NOTES in str(path)]
    return sorted(found, key=lambda path: path.stat().st_mtime)


def written() -> bool:
    return any(TEXT.encode() in path.read_bytes() for path in records())


def record_sizes() -> str:
    return json.dumps({str(path.relative_to(WORKSPACE)): path.stat().st_size
                       for path in records()})


def front(session) -> dict | None:
    """The Notes window the desk shows in front: the one keys are typed into."""
    return next((window for window in session.windows()
                 if window.get("app_id") == NOTES and window.get("active")), None)


def authors(dump: dict) -> list[str]:
    """The apps the desk's workspaces make authors of their own records."""
    tree = (dump or {}).get("workspace_tree") or {}
    return sorted({grant.get("id", "") for workspace in tree.get("workspaces", [])
                   for grant in (workspace.get("policy") or {}).get("grants", [])
                   if str(grant.get("id", "")).startswith("app-author:")})


def write_a_page(session) -> None:
    """Ctrl+N, the page's name, Enter, the page's first line: what a person types."""
    session.chord("Control", "n")
    session.type(TEXT)
    session.key("Enter")
    session.type(LINE)
    session.key("Enter")


def main() -> int:
    SHOTS.mkdir(parents=True, exist_ok=True)
    if "--build" in sys.argv:
        real_apps.build(apps=APPS)
    real_apps.prepare(WORKSPACE, apps=APPS, built=False)
    print(f"==> state {WORKSPACE}")

    session = real_apps.Session(WORKSPACE)
    try:
        session.start(apps=APPS, fresh=True, out=SHOTS, tag="first")
        session.launch("notes")
        session.answering("notes", deadline=180)
        # The app keeps its notebook from the moment it starts, one record of
        # its own in the session's state; once it is there, its window takes
        # the keyboard.
        try:
            until("the Notes record the app keeps", lambda: bool(records()), 30, 0.5)
        except Failed as error:
            check("Notes keeps a record of the workspace", False, str(error))
            raise
        check("the Notes window is the one in front", front(session) is not None)
        check("the desk makes Notes an author of the workspace",
              f"app-author:{NOTES}" in authors(session.state_dump()),
              json.dumps(authors(session.state_dump())))
        session.snapshot(SHOTS / "01-notes.png")
        write_a_page(session)
        session.snapshot(SHOTS / "02-typed.png")
        try:
            until("the typed text in the Notes record", written, 20, 0.2)
            check("what was typed reaches the Notes record while the app runs", True)
        except Failed:
            check("what was typed reaches the Notes record while the app runs", False,
                  f"records {record_sizes()}")
    finally:
        session.stop()

    check("the record still holds the text after the session ended", written(),
          f"records {record_sizes()}")

    session = real_apps.Session(WORKSPACE)
    try:
        session.start(apps=APPS, fresh=False, out=SHOTS, tag="again")
        session.launch("notes")
        session.answering("notes", deadline=180)
        session.snapshot(SHOTS / "03-relaunched.png")
        check("the restarted session has the text back", written(),
              f"records {record_sizes()}")
        check("the restarted desk still makes Notes an author",
              f"app-author:{NOTES}" in authors(session.state_dump()),
              json.dumps(authors(session.state_dump())))
    finally:
        session.stop()

    print(f"==> shots in {SHOTS}")
    print("PASS" if not failures else f"FAIL ({len(failures)}): {', '.join(failures)}")
    return 0 if not failures else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Failed as error:
        print(f"FAIL — {error}")
        sys.exit(1)
    except Exception as error:  # noqa: BLE001 - a proof that crashed is a failure
        print(f"FAIL — {type(error).__name__}: {error}")
        sys.exit(1)

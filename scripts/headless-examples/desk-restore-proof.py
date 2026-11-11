#!/usr/bin/env python3
"""The windows a person left open come back after a restart, and only they.

A session starts on a scratch home with the tree's own apps. Notes opens from
the launcher, Terminal and Files from their shortcuts (Super+Enter, Super+E),
so one window fills a tab the desk held for it while it started. The session
then ends the way a logout ends it, its apps with it, and starts again on the
same state.

  again   the restarted desk holds as many windows as were left open, each
          one its app's window again: none is missing, and no tab stands
          for an app that is not there

It needs the session and the tree's apps built (dist/dev/nested-session-build.sh).

Usage:
  ./desk-restore-proof.py [STATE]
"""
import pathlib
import sys
import tempfile

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import real_apps  # noqa: E402
from real_apps import Failed, until  # noqa: E402

APPS = ["notes", "terminal", "file-explorer"]


def opened(session):
    """The windows on the desk as (kind, app) pairs, in desk order."""
    return [(row["kind"], row["app_id"]) for row in session.windows()]


def main() -> None:
    state = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else tempfile.mkdtemp(
        prefix="tilo-desk-restore-"))
    real_apps.prepare(state)
    session = real_apps.Session(state)
    session.start(fresh=True)
    try:
        session.launch("notes")
        session.chord("Super", "Enter")
        session.answering("terminal")
        session.chord("Super", "e")
        session.answering("file-explorer")
        left = sorted(app for _, app in opened(session))
        print(f"left open: {left}")
        session.restart()
        for app in APPS:
            session.answering(app)
        want = sorted(("Foreign", app) for app in left)
        until("the desk as it was left",
              lambda: sorted(opened(session)) == want, 60, 0.5)
        print(f"again: {opened(session)}")
    except Failed as error:
        print(f"desk after restart: {opened(session) if session.running() else '-'}")
        raise SystemExit(f"FAIL: {error}") from error
    finally:
        session.stop()
    print("PASS")


if __name__ == "__main__":
    main()

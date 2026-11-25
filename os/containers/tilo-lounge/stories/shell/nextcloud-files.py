#!/usr/bin/env python3
"""Keep files in Nextcloud, sign in from the shell's Settings, and see them in the workspace.

Ada adds her Nextcloud account in Settings' Accounts page, signs in and grants
this device access on the server's own pages in Tilo's Browser, picks the folder
she keeps her files in from the Files window, and the files are there. A file then
changed in Nextcloud, as another Nextcloud client would change it, changes in the
workspace too.

The Nextcloud is the check's own (crates/integrations/cloud-source-dav/tests/servers.sh,
seeded with Ada's Documents and Photos), so what she picks and what changes
afterwards are a real server's. Every step is decided off the shell itself (the
controls it draws, the folders it shows, the bytes it keeps) or off the server,
never off what an action answered.

    python3 nextcloud-files.py OUT_DIR
"""

from __future__ import annotations

import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import nextcloud  # noqa: E402
from story import Failed, Story, until, walk  # noqa: E402

# What Ada's computer has: Settings to connect the account with, Files to see
# the folder in, and the Browser the sign-in opens the server's page in.
APPS = "settings files browser"
# A file the seed put in Ada's Nextcloud, and what a change to it says.
HELD = "plan.txt"
CHANGED = f"Spring planting, changed {int(time.time()) % 100000:05d}"
# The folder Ada keeps her files in, in her Nextcloud.
FOLDER = "Documents"
FILES = "so.tilo.file-explorer"


def mount_files(one) -> list[Path]:
    """The files of Ada's cloud accounts on her own computer, as Files opens
    them: `~/Remote/<account>/…`, where the mount service keeps them."""
    remote = one.state / "home/Remote"
    return sorted(remote.rglob("*")) if remote.is_dir() else []


def why_not_mounted(one) -> str:
    """What Ada's shell says about her account's folder when the files are not
    there: the panel Files draws for an account it cannot open, or what she saw."""
    said = [name for name in one.names() if name and name not in (
        "Back", "Forward", "Up", "Home", "Recent", "Applications", "Search",
        "New folder", "New file")]
    return f"Files offers {said[:40]}"


def steps(story: Story) -> None:
    server = (os.environ.get("TILO_TEST_NEXTCLOUD_URL") or "").rstrip("/")
    user = os.environ.get("TILO_TEST_NEXTCLOUD_USER") or ""
    password = os.environ.get("TILO_TEST_NEXTCLOUD_PASSWORD") or ""
    files = (os.environ.get("TILO_TEST_NEXTCLOUD_FILES") or "").rstrip("/")
    if not (server and files and user and password):
        raise Failed("this story meets a real Nextcloud, and no seeded one is named: "
                     "TILO_TEST_NEXTCLOUD_URL, _USER, _PASSWORD and _FILES "
                     "(servers.sh start and seed); it runs as the check's user-stories-shell "
                     "story, whose servers come from stories.json's `servers`")

    ada = story.person("Ada")
    ada.prepare(APPS)

    def signs_in():
        took = ada.start(apps=APPS)
        story.timing("Ada's session start, to its first frame", took["tookMs"])
        story.timing("Ada's session start, to its first frame, "
                     "with the development launcher's installing", took["launcherMs"])
        nextcloud.connect_account(story, ada, server, user, password)

    story.step("Ada chooses Nextcloud and signs in", signs_in, people=[ada])

    def picks_folders():
        ada.session.launch("file-explorer")
        ada.session.answering("file-explorer", deadline=180)
        # A window beside another is narrow, and Files keeps its places in a sidebar there: she
        # opens it to find her account.
        if not ada.target("Nextcloud", inside=FILES) and ada.target("Show sidebar"):
            ada.press("Show sidebar", "the sidebar of Files, where its places are")
        until("Ada's account among Files' places",
              lambda: ada.target("Nextcloud", inside=FILES), 30, 0.25)
        ada.press("Nextcloud", "her Nextcloud account in Files' places", inside=FILES)

        def opened():
            if ada.target(FOLDER, inside=FILES):
                return "folder", []
            # An account Files cannot open is a panel that says why: its
            # status, and where its folder is, or that it is not mounted.
            said = [name for name in ada.names()
                    if any(part in name for part in (
                        "not been checked", "not mounted", "Needs attention", "Offline",
                        "Sign in required", "This account", "Nextcloud at"))]
            return ("shown", said) if said else None

        which, said = until(f"her {FOLDER} folder, or the panel saying why not",
                            opened, 30, 0.25)
        if which != "folder":
            raise Failed(f"Ada's Nextcloud account does not open on her files: the panel Files "
                         f"draws says {said}, and Files offers {ada.names()[:60]}")
        ada.press(FOLDER, f"{FOLDER}, where her files are kept", inside=FILES)

    story.step("Ada picks the folders to bring", picks_folders, people=[ada])

    def arrive():
        until(f"“{HELD}” in Ada's Files window", lambda: ada.shows(HELD), 60, 0.25)
        kept = [path for path in mount_files(ada) if path.name == HELD]
        if not kept:
            raise Failed(f"“{HELD}” is drawn in Files but no file is on Ada's computer: "
                         f"{why_not_mounted(ada)}")

    story.step("The files arrive in the workspace", arrive, people=[ada])

    def changed():
        nextcloud.change_file(files, f"{FOLDER}/{HELD}", f"{CHANGED}\n", user, password)
        began = time.monotonic()

        def here():
            return [path for path in mount_files(ada) if path.name == HELD
                    and CHANGED.encode() in path.read_bytes()]

        # What the workspace keeps is the folder's own bytes: the copy another
        # Nextcloud client wrote arrives there, and how long it took is kept.
        until(f"Nextcloud's change to “{HELD}” in the workspace", here, 60, 0.5)
        story.timing("Nextcloud's change to the workspace, after the server took it",
                     (time.monotonic() - began) * 1000)

    story.step("A file changed in Nextcloud changes in the workspace too", changed, people=[ada])


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: nextcloud-files.py OUT_DIR", file=sys.stderr)
        return 2
    return walk("nextcloud-files", Path(sys.argv[1]), steps)


if __name__ == "__main__":
    sys.exit(main())

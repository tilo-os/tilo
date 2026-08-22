#!/usr/bin/env python3
"""Build a self-contained HOME for the collaboration demo.

The shell's search providers read the ambient environment: the file
provider indexes `dirs::home_dir()` (i.e. `$HOME`), and the host-app
provider walks `$XDG_DATA_HOME` then each `$XDG_DATA_DIRS` entry looking
for `.desktop` files. Run the demo against a real login session and the
command palette fills with whatever happens to be on the machine —
`~/Downloads`, game directories, every installed Flatpak. That is noise
on camera and it puts the operator's own filesystem on the recording.

So the demo gets its own HOME. Nothing here is wired into the shell; it
is purely the environment the demo processes are launched with, which is
why it cannot regress a normal session.

    env $(scripts/demo-home.py --print-env) cargo run --bin shell ...

or from Python:

    from demo_home import stage
    env = stage()                       # -> dict of environment overrides

The staged tree holds a small, real corpus so the palette has something
to find. It is demo data behind a demo entrypoint, not content shipped
inside an app: the apps themselves still start empty.
"""
from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

# Deliberately small. Every one of these should be something a viewer can
# read off a search result in half a second without pausing the video.
CORPUS: dict[str, str] = {
    "Documents/Roadmap.md": (
        "# Roadmap\n\n"
        "## This quarter\n\n"
        "- Ship the collaborative editor\n"
        "- Land per-workspace sandboxing\n"
        "- Cut the first installable image\n"
    ),
    "Documents/Meeting notes.md": (
        "# Meeting notes\n\n"
        "Agreed the workspace is the unit of collaboration.\n"
        "Open question: how records move between rooms.\n"
    ),
    "Documents/Budget.md": "# Budget\n\nHosting, build machines, one conference.\n",
    "Projects/README.md": "# Projects\n\nScratch space for anything unfiled.\n",
    "Pictures/.keep": "",
    "Downloads/.keep": "",
}

# The document the two sessions co-edit. Both sides must open the SAME
# path: the path is the record id, which is the collaboration address.
SHARED_DOC = "Documents/Roadmap.md"


def stage(root: Path | None = None, *, fresh: bool = True) -> dict[str, str]:
    """Create the staged HOME and return the environment overrides for it.

    `fresh=True` removes any previous stage so a re-run of the demo starts
    from a known state rather than accumulating edits from the last take.
    """
    root = Path(root) if root else Path(__file__).resolve().parent.parent / "target" / "demo-home"

    if fresh and root.exists():
        shutil.rmtree(root)

    for rel, body in CORPUS.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body, encoding="utf-8")

    # An empty applications dir, and an XDG_DATA_DIRS that points only at
    # it, is what keeps the palette's Host Apps group from enumerating the
    # operator's installed software. Empty is correct here: the demo
    # installs its app through the palette, which is the thing being shown.
    data_home = root / ".local" / "share"
    (data_home / "applications").mkdir(parents=True, exist_ok=True)
    (root / ".config").mkdir(parents=True, exist_ok=True)
    (root / ".cache").mkdir(parents=True, exist_ok=True)

    return {
        "HOME": str(root),
        "XDG_DATA_HOME": str(data_home),
        "XDG_CONFIG_HOME": str(root / ".config"),
        "XDG_CACHE_HOME": str(root / ".cache"),
        # Not $XDG_DATA_HOME: listing it twice makes the scanner walk the
        # same tree twice. Point at the staged share root instead, so the
        # only .desktop files discoverable are ones the demo put there.
        "XDG_DATA_DIRS": str(data_home),
    }


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Build a self-contained HOME for the collaboration demo."
    )
    ap.add_argument("--root", help="where to stage (default: target/demo-home)")
    ap.add_argument("--keep", action="store_true",
                    help="reuse an existing stage instead of recreating it")
    ap.add_argument("--print-env", action="store_true",
                    help="print KEY=VALUE lines suitable for env(1)")
    ap.add_argument("--print-doc", action="store_true",
                    help="print the absolute path of the shared document")
    args = ap.parse_args()

    env = stage(Path(args.root) if args.root else None, fresh=not args.keep)

    if args.print_doc:
        print(Path(env["HOME"]) / SHARED_DOC)
        return 0

    if args.print_env:
        for k, v in env.items():
            print(f"{k}={v}")
        return 0

    print(f"staged {env['HOME']}", file=sys.stderr)
    for rel in CORPUS:
        if not rel.endswith(".keep"):
            print(f"  {rel}", file=sys.stderr)
    print(f"shared document: {Path(env['HOME']) / SHARED_DOC}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

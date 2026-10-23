#!/usr/bin/env python3
"""Build a self-contained HOME for a headless shell run.

The shell's search providers read the ambient environment: the file
provider indexes `dirs::home_dir()` (i.e. `$HOME`), and the host-app
provider walks `$XDG_DATA_HOME` then each `$XDG_DATA_DIRS` entry looking
for `.desktop` files. Run the shell headless against a real login session and the
command palette fills with whatever happens to be on the machine —
`~/Downloads`, game directories, every installed Flatpak. That adds noise
and puts the operator's own filesystem into captures.

So a headless run gets its own HOME. Nothing here is wired into the shell; it
is purely the environment the test processes are launched with, which is
why it cannot regress a normal session.

    env $(scripts/headless-examples/isolated-home.py --print-env) ./target/debug/shell ...

Note the environment has to go to the *compositor*, not to cargo: staging
cargo's HOME breaks the build shim. Build first, then run the binary.

From Python there is no plain import to reach for - the filename is
hyphenated, so `import isolated_home` cannot resolve it. Load it by path:

    import importlib.util, pathlib
    spec = importlib.util.spec_from_file_location(
        "isolated_home", pathlib.Path(__file__).parent / "isolated-home.py")
    isolated_home = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(isolated_home)
    env = isolated_home.stage()             # -> dict of environment overrides

The staged tree holds a small corpus so the palette has something to find.
It is fixture data behind a test entrypoint, not content shipped inside an app:
the apps themselves still start empty.
"""
from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

# Deliberately small. Every entry should be readable in a search result.
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

    `fresh=True` removes any previous stage so a re-run starts
    from a known state rather than accumulating edits from the last one.
    """
    root = Path(root) if root else Path(__file__).resolve().parents[2] / "target" / "isolated-home"

    if fresh and root.exists():
        shutil.rmtree(root)

    for rel, body in CORPUS.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body, encoding="utf-8")

    data_home = root / ".local" / "share"
    (data_home / "applications").mkdir(parents=True, exist_ok=True)
    (root / ".config").mkdir(parents=True, exist_ok=True)
    (root / ".cache").mkdir(parents=True, exist_ok=True)

    return {
        "HOME": str(root),
        "XDG_DATA_HOME": str(data_home),
        "XDG_CONFIG_HOME": str(root / ".config"),
        "XDG_CACHE_HOME": str(root / ".cache"),
        # Only the word processor currently translates its UI, so pin the
        # locale to avoid mixed-language output in captures.
        "LANG": "en_US.UTF-8",
        "LC_ALL": "en_US.UTF-8",
        "LANGUAGE": "en",
    }
    # Deliberately NOT overriding XDG_DATA_DIRS, for two reasons.
    #
    # It does not achieve the isolation it looks like it would: the
    # desktop-entry scanner always appends
    # /var/lib/flatpak/exports/share/applications regardless of the
    # environment (crates/system/desktop-entry/src/scan.rs, with a test
    # pinning that behaviour), so system Flatpaks appear in the palette's
    # Apps group whatever this variable says.
    #
    # And pointing it at the staged tree actively breaks rendering: the
    # Vulkan loader finds its ICD manifests through XDG_DATA_DIRS too, so
    # a staged-only value makes wgpu report "no adapter available" and the
    # compositor cannot draw at all. Measured, not theorised.
    #
    # Net: staging fixes the file leak completely - a "~" search drops from
    # the operator's whole home directory to this corpus - and does not
    # hide installed host applications. Choose queries that do not
    # match host app names, or run on a machine without them.


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Build a self-contained HOME for a headless shell run."
    )
    ap.add_argument("--root", help="where to stage (default: target/isolated-home)")
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

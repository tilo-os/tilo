#!/usr/bin/env python3
"""The build context of a Containerfile: the repository files it reads, staged for `podman build`.

    build-context.py CONTAINERFILE --epoch SECONDS [--repo DIR] [--dest DIR]
    build-context.py --glob PATTERN... --epoch SECONDS [--repo DIR] [--dest DIR]

Into DIR, at their repository paths, exactly what the Containerfile takes from its context: the
sources of its COPY and ADD instructions without --from, and of its RUN --mount=type=bind without
from=. A directory source goes in whole. Nothing else of the checkout is in the context, so
`podman build` neither reads nor hashes the rest of the tree: a file the image does not copy — a
test script, a document, another Containerfile — can change neither its layers nor the key of
the check that builds it. Given the whole repository as its context, every tracked file was an
input of every image build.

Every staged file is dated by EPOCH, the build's clock: a COPY keeps a file's own time where it
is older than the clock, and a checkout gives a file the time it was written, so an undated copy
would make a layer differ between two builds of the same tree.

The directory is DIR when given, else a new one under $TILO_CONTEXT_SCRATCH (/var/tmp), printed
on stdout: never inside the checkout, where the files would be inputs of the build as well.
"""

from __future__ import annotations

import argparse
import glob
import os
import re
import shlex
import shutil
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DEFAULT_SCRATCH = "/var/tmp"


def instructions(text: str) -> list[str]:
    """The Containerfile's instructions, continuation lines joined and comments dropped."""
    out: list[str] = []
    current = ""
    for line in text.splitlines():
        stripped = line.strip()
        if current and stripped.startswith("#"):
            continue
        if not current and (not stripped or stripped.startswith("#")):
            continue
        if stripped.endswith("\\"):
            current += stripped[:-1] + " "
            continue
        out.append(current + stripped)
        current = ""
    if current:
        out.append(current)
    return out


def copies(text: str) -> list[str]:
    """The sources a Containerfile takes from its own context, as written."""
    sources: list[str] = []
    for instruction in instructions(text):
        word, _, rest = instruction.partition(" ")
        word = word.upper()
        if word in {"COPY", "ADD"}:
            args = shlex.split(rest, posix=True)
            if any(arg.startswith("--from") for arg in args):
                continue
            sources += [arg for arg in args if not arg.startswith("--")][:-1]
        elif word == "RUN":
            for mount in re.findall(r"--mount=(\S+)", rest):
                fields = dict(f.split("=", 1) for f in mount.split(",") if "=" in f)
                if fields.get("type") == "bind" and "from" not in fields and "source" in fields:
                    sources.append(fields["source"])
    return sources


def patterns(sources: list[str]) -> list[str]:
    """Repository-relative globs matching the sources.

    A path chosen by a build argument (os/hardware/firmware/${TILO_HARDWARE_PROFILE}) is every
    choice of it: the check that builds the image then reads each, which no run overrides away.
    """
    out: list[str] = []
    for source in sources:
        path = re.sub(r"\$\{[^}]*\}", "*", source).strip("/")
        if path in {"", "."}:
            raise SystemExit(
                f"the Containerfile copies its whole build context ('{source}'): it cannot be "
                "staged from what it copies"
            )
        out.append(path)
    return out


def stage(pattern: str, repo: Path, dest: Path) -> int:
    """One pattern, into dest: its matches, a directory whole. How many files it wrote."""
    matches = glob.glob(pattern, root_dir=repo, recursive=True, include_hidden=True)
    if not matches and not glob.has_magic(pattern):
        raise SystemExit(f"the Containerfile copies {pattern}, which the repository does not have")
    written = 0
    for match in sorted(matches):
        source = repo / match
        target = dest / match
        if source.is_dir() and not source.is_symlink():
            for root, dirs, files in os.walk(source):
                relative = Path(root).relative_to(source)
                (target / relative).mkdir(parents=True, exist_ok=True)
                for name in files:
                    shutil.copy2(Path(root) / name, target / relative / name, follow_symlinks=False)
                    written += 1
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target, follow_symlinks=False)
            written += 1
    return written


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("containerfile", nargs="?", help="the Containerfile whose context to stage")
    parser.add_argument("--glob", action="append", default=[], help="a source pattern instead")
    parser.add_argument("--epoch", type=int, required=True, help="the build's clock, seconds")
    parser.add_argument("--repo", type=Path, default=REPO)
    parser.add_argument("--dest", type=Path, help="where to stage it (default: a new directory)")
    args = parser.parse_args(argv)
    if bool(args.containerfile) == bool(args.glob):
        parser.error("name one Containerfile or some --glob patterns")
    repo = args.repo.resolve()
    found = args.glob or patterns(copies(Path(args.containerfile).read_text()))
    if args.dest:
        dest = args.dest.resolve()
        shutil.rmtree(dest, ignore_errors=True)
        dest.mkdir(parents=True)
    else:
        scratch = os.environ.get("TILO_CONTEXT_SCRATCH") or DEFAULT_SCRATCH
        dest = Path(tempfile.mkdtemp(prefix="tilo-build-context.", dir=scratch)).resolve()
    written = sum(stage(pattern, repo, dest) for pattern in found)
    # Dated by the build's clock, which the layers are (SOURCE_DATE_EPOCH).
    for root, dirs, files in os.walk(dest):
        for entry in dirs + files:
            os.utime(Path(root) / entry, (args.epoch, args.epoch), follow_symlinks=False)
    os.utime(dest, (args.epoch, args.epoch))
    if written == 0:
        raise SystemExit(f"the Containerfile copies {', '.join(found)}, which match nothing")
    print(dest)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

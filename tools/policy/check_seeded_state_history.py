#!/usr/bin/env python3
"""Every file the image seeds into persistent state once keeps its history.

systemd-tmpfiles' ``C`` copies a shipped file to its target only when the
target does not exist yet, and ``f`` with an argument writes its content only
then. That is how the image hands an administrator a default they may edit
(``/var/lib/tilo/fs-capabilities.json``): bootc replaces ``/usr`` on every
update, but the copy in ``/var`` stays. It also means a machine installed from
an older image keeps the file in the schema that image shipped, forever. When
the capability list's schema changed, every machine installed before the
change kept the old list, fs.service refused it on each start and
``/var/workspace`` never mounted, while every test read only the current
default.

So for each such seed this check requires a history directory,
``os/tests/seeded-state/<target>/``, that

* holds a file byte-for-byte equal to what the image ships today, so changing
  the shipped default means adding its new version beside the old ones, and
* is named by a test outside this directory, which loads every version in it
  through the production reader.

The history only grows: a version stays as long as a machine installed with it
may still exist. ``C+`` and ``f+`` overwrite on every boot, ``f`` without an
argument creates an empty file, and the other line types create no content, so
none of them is checked.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path, PurePosixPath
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[2]
HISTORY = PurePosixPath("os/tests/seeded-state")
SCANNED = ("os", "dist")
POLICY = "docs/policies.md, Testing and verification"


@dataclass(frozen=True)
class Seed:
    conf: str
    line: int
    kind: str
    target: str
    # The repository file a `C` line copies, or the literal content of an `f` line.
    source: str | None
    content: bytes | None


def tracked_files(root: Path) -> list[str]:
    output = subprocess.run(
        ["git", "ls-files", "-z", "--", *SCANNED],
        cwd=root,
        check=True,
        capture_output=True,
    ).stdout
    return sorted(name.decode() for name in output.split(b"\0") if name)


def is_tmpfiles_conf(path: str) -> bool:
    parts = PurePosixPath(path).parts
    return path.endswith(".conf") and any(
        part in ("tmpfiles.d", "user-tmpfiles.d") for part in parts[:-1]
    )


def copy_once_seeds(root: Path, conf: str, tracked: set[str]) -> tuple[list[Seed], list[str]]:
    """The lines of one tmpfiles.d file that write content only when the target is absent."""
    seeds: list[Seed] = []
    problems: list[str] = []
    text = (root / conf).read_text(encoding="utf-8")
    for number, raw in enumerate(text.splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        fields = line.split(None, 6)
        kind = fields[0]
        letter, modifiers = kind[0], kind[1:]
        if letter not in "Cf" or "+" in modifiers or len(fields) < 2:
            continue
        target = fields[1]
        argument = fields[6] if len(fields) == 7 else "-"
        where = f"{conf}:{number}"
        if letter == "f":
            if argument == "-":
                continue
            seeds.append(Seed(conf, number, kind, target, None, argument.encode()))
            continue
        shipped = argument if argument != "-" else f"/usr/share/factory{target}"
        candidates = [f"os{shipped}", f"os/rootfs{shipped}"]
        source = next((path for path in candidates if path in tracked), None)
        if source is None:
            problems.append(
                f"{where}: `{kind} {target}` copies {shipped}, which no tracked file under os/ "
                "ships, so its history cannot be checked"
            )
            continue
        seeds.append(Seed(conf, number, kind, target, source, None))
    return seeds, problems


def history_dir(target: str) -> PurePosixPath:
    return HISTORY / target.lstrip("/")


def readers_of(root: Path, directory: PurePosixPath) -> list[str]:
    """Tracked files outside the history and this check that name the history directory."""
    output = subprocess.run(
        ["git", "grep", "-l", "-F", str(directory)],
        cwd=root,
        capture_output=True,
    ).stdout.decode()
    own = (str(HISTORY) + "/", "tools/policy/", "tools/tests/", "docs/")
    return [path for path in output.splitlines() if not path.startswith(own)]


def check(root: Path = ROOT) -> list[str]:
    tracked_list = tracked_files(root)
    tracked = set(tracked_list)
    problems: list[str] = []
    for conf in (path for path in tracked_list if is_tmpfiles_conf(path)):
        seeds, found = copy_once_seeds(root, conf, tracked)
        problems.extend(found)
        for seed in seeds:
            where = f"{seed.conf}:{seed.line}"
            directory = history_dir(seed.target)
            versions = sorted(
                path for path in tracked if PurePosixPath(path).parent == directory
            )
            if not versions:
                problems.append(
                    f"{where}: `{seed.kind} {seed.target}` is written once and kept across image "
                    f"updates, but {directory}/ holds none of the versions machines may still "
                    "have; add each version the image ever shipped there"
                )
                continue
            current = (
                (root / seed.source).read_bytes() if seed.source is not None else seed.content
            )
            if not any(
                (root / path).read_bytes().rstrip(b"\n") == current.rstrip(b"\n")
                for path in versions
            ):
                shipped = seed.source or "its inline content"
                problems.append(
                    f"{where}: {shipped} changed, but {directory}/ lacks this version; machines "
                    "installed earlier keep their older copy, so add the new version beside "
                    "the old ones and keep the old ones loading"
                )
            if not readers_of(root, directory):
                problems.append(
                    f"{where}: no test names {directory}; load every version in it through the "
                    f"reader of {seed.target}"
                )
    return problems


def main() -> int:
    problems = check(ROOT)
    for problem in problems:
        print(f"  {problem}")
    if problems:
        print(f"State seeded once must keep loading in every shipped version ({POLICY}).")
        return 1
    print("Every copy-once seed keeps the versions upgraded machines still hold.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

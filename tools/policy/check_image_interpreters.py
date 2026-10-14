#!/usr/bin/env python3
"""No new interpreter scripts in the OS image overlay.

``docs/policies.md`` keeps interpreted languages off the runtime path of the OS
where a compiled alternative exists. The image overlay (``os/usr`` and
``os/etc``, copied into the image as they stand) is where such a script would
ship, so this check reads every tracked file there and flags one that is
interpreted:

* a ``#!`` line naming Python, Node, Deno, Bun or Perl, directly or through
  ``env`` (``#!/usr/bin/env python3``, ``#!/usr/bin/python3 -I``,
  ``#!/usr/bin/env -S node --flag``);
* a ``.py``, ``.js``, ``.mjs``, ``.cjs`` or ``.pl`` file, with or without one.

``image_interpreter_baseline.txt`` lists the scripts that predate the rule, by
path, each with the reason it still ships. A flagged file missing from it
fails, and so does a listed path that no longer holds an interpreter script,
so the list only shrinks: porting or deleting a script removes its line.

Build stages and CI tooling may use Python, so Containerfiles, ``os/scripts``,
``os/dev``, ``os/tests`` and ``os/kernel`` are not scanned. Untracked files are
ignored.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path("tools/policy/image_interpreter_baseline.txt")
SCANNED = ("os/usr", "os/etc")
POLICY = "docs/policies.md, Dependencies and runtime"

INTERPRETER = re.compile(r"(python[0-9.]*|node|nodejs|deno|bun|perl[0-9.]*)")
EXTENSIONS = (".py", ".js", ".mjs", ".cjs", ".pl")


class BaselineError(ValueError):
    """The baseline file is malformed."""


@dataclass(frozen=True)
class Entry:
    line: int
    reason: str


def shebang_interpreter(first_line: bytes) -> str | None:
    """Return the program a ``#!`` line runs, looking through ``env``."""
    if not first_line.startswith(b"#!"):
        return None
    words = first_line[2:].decode("utf-8", "replace").split()
    if not words:
        return None
    program = Path(words[0]).name
    if program != "env":
        return program
    for word in words[1:]:
        # `env -S` splits the rest; other flags and VAR=value assignments precede the program.
        if word.startswith("-") or "=" in word:
            continue
        return Path(word).name
    return None


def is_interpreter_script(path: Path) -> bool:
    if path.name.endswith(EXTENSIONS):
        return True
    try:
        with path.open("rb") as handle:
            first_line = handle.readline(512)
    except (FileNotFoundError, IsADirectoryError):
        return False
    program = shebang_interpreter(first_line)
    return program is not None and INTERPRETER.fullmatch(program) is not None


def tracked_files(root: Path) -> list[str]:
    output = subprocess.run(
        ["git", "ls-files", "-z", "--", *SCANNED],
        cwd=root,
        check=True,
        capture_output=True,
    ).stdout
    return sorted(name.decode() for name in output.split(b"\0") if name)


def shipped_scripts(root: Path) -> list[str]:
    """Tracked files in the image overlay that an interpreter runs."""
    return [name for name in tracked_files(root) if is_interpreter_script(root / name)]


def load_baseline(root: Path) -> dict[str, Entry]:
    entries: dict[str, Entry] = {}
    text = (root / BASELINE).read_text(encoding="utf-8")
    for number, raw in enumerate(text.splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        path, _, reason = (part.strip() for part in line.partition("#"))
        if not reason:
            raise BaselineError(
                f"{BASELINE}:{number}: `{path}` needs a reason after `#` saying why it still ships"
            )
        if path in entries:
            raise BaselineError(
                f"{BASELINE}:{number}: `{path}` is already listed on line {entries[path].line}"
            )
        entries[path] = Entry(number, reason)
    return entries


def check(root: Path = ROOT) -> tuple[list[str], list[str]]:
    """Return interpreter scripts missing from the baseline and baseline paths no longer shipped."""
    shipped = shipped_scripts(root)
    baseline = load_baseline(root)
    new = [path for path in shipped if path not in baseline]
    stale = sorted(set(baseline) - set(shipped), key=lambda path: baseline[path].line)
    return new, stale


def main() -> int:
    try:
        new, stale = check(ROOT)
        baseline = load_baseline(ROOT)
    except BaselineError as error:
        print(f"  {error}")
        return 1
    for path in new:
        print(
            f"  {path} is an interpreter script in the OS image; ship compiled code instead "
            f"(an exception is a line in {BASELINE} with its reason, made in review)"
        )
    for path in stale:
        print(
            f"  {path} no longer ships an interpreter script: "
            f"remove line {baseline[path].line} of {BASELINE}"
        )
    if new or stale:
        print(f"No new interpreted code on the OS runtime path ({POLICY}).")
        return 1
    print(f"Image overlay interpreter scripts: {len(baseline)} baselined, none new.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

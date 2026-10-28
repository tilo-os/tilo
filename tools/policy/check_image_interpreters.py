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
* a ``.py``, ``.js``, ``.mjs``, ``.cjs`` or ``.pl`` file, with or without one;
* a shell script that runs Python (a line, comments aside, naming ``python``
  or ``python3`` as a command word, ``python3 -c`` and here-documents too).

The host packages' payload (``dist/session``, without its tests) is scanned
the same way. So is what the image builds take from elsewhere: a Containerfile
``COPY`` (``os/Containerfile``, ``os/Containerfile.installer``) or an
``os/components.toml`` file entry that installs a Python file into ``/usr`` or
``/etc``, listed as ``image:<destination>``.

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
SCANNED = ("os/usr", "os/etc", "dist/session")
# Test tooling next to the host payload; none of it is installed.
UNSCANNED = ("dist/session/tests/", "dist/session/vm-e2e/")
CONTAINERFILES = ("os/Containerfile", "os/Containerfile.installer")
COMPONENTS = Path("os/components.toml")
POLICY = "docs/policies.md, Dependencies and runtime"

INTERPRETER = re.compile(r"(python[0-9.]*|node|nodejs|deno|bun|perl[0-9.]*)")
SHELL = re.compile(r"(sh|bash|dash)")
RUNS_PYTHON = re.compile(r"(^|[\s;|&(`$]|/)python(3(\.[0-9]+)?)?([\s;|&)`]|$)")
# Build-stage and scratch destinations: nothing there ships.
BUILD_DESTINATIONS = ("/usr/local/", "/run/", "/tmp/", "/tilo", "/build", "/out", "/src", "/tree/", "/image/", "/repo")
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
    except OSError as error:
        raise OSError(f"Cannot inspect tracked overlay file {path}: {error}") from error
    program = shebang_interpreter(first_line)
    if program is not None and INTERPRETER.fullmatch(program) is not None:
        return True
    return program is not None and SHELL.fullmatch(program) is not None and runs_python(path)


def runs_python(path: Path) -> bool:
    """Whether a shell script names Python as a command on any non-comment line."""
    text = path.read_text(encoding="utf-8", errors="replace")
    for line in text.splitlines():
        code = line.split("#", 1)[0] if line.lstrip().startswith("#") else line
        if line.lstrip().startswith("#"):
            continue
        if RUNS_PYTHON.search(code):
            return True
    return False


def copy_instructions(text: str) -> list[list[str]]:
    """The word lists of every COPY instruction, continuation lines joined."""
    joined = re.sub(r"\\\n", " ", text)
    instructions = []
    for line in joined.splitlines():
        words = line.split()
        if words and words[0] == "COPY":
            instructions.append([word for word in words[1:] if not word.startswith("--")])
    return instructions


def copied_python(root: Path, containerfiles: tuple[str, ...] = CONTAINERFILES) -> list[str]:
    """`image:<destination>` for each Python file a Containerfile COPY installs into /usr or /etc."""
    found = []
    for name in containerfiles:
        path = root / name
        if not path.is_file():
            continue
        for words in copy_instructions(path.read_text(encoding="utf-8")):
            if len(words) < 2:
                continue
            *sources, destination = words
            if not destination.startswith(("/usr/", "/etc/")) or destination.startswith(BUILD_DESTINATIONS):
                continue
            for source in sources:
                if source.startswith(tuple(f"{prefix}/" for prefix in SCANNED)):
                    continue  # scanned as overlay files, under their own path
                python = source.endswith(".py")
                local = root / source
                if not python and local.is_file() and "/" in source:
                    python = is_interpreter_script(local)
                if python:
                    target = destination
                    if destination.endswith("/"):
                        target = destination + Path(source).name
                    found.append(f"image:{target}")
    return found


def component_python(root: Path) -> list[str]:
    """`image:<destination>` for each Python file an os/components.toml entry installs."""
    path = root / COMPONENTS
    if not path.is_file():
        return []
    import tomllib

    manifest = tomllib.loads(path.read_text(encoding="utf-8"))
    found = []
    for component in manifest.get("component", []):
        for entry in component.get("files", []):
            source = str(entry.get("from", ""))
            destination = str(entry.get("to", ""))
            if source.endswith(".py") and destination.startswith(("/usr/", "/etc/")):
                found.append(f"image:{destination}")
    return found


def tracked_files(root: Path) -> list[str]:
    output = subprocess.run(
        ["git", "ls-files", "-z", "--", *SCANNED],
        cwd=root,
        check=True,
        capture_output=True,
    ).stdout
    return sorted(name.decode() for name in output.split(b"\0") if name)


def shipped_scripts(root: Path) -> list[str]:
    """Tracked shipped files an interpreter runs, and Python the image builds install."""
    overlay = [
        name
        for name in tracked_files(root)
        if not name.startswith(UNSCANNED) and is_interpreter_script(root / name)
    ]
    return sorted(set(overlay + copied_python(root) + component_python(root)))


def declared_in(root: Path, path: str) -> str:
    """The repository file that puts a shipped script in the image: the overlay file itself, or
    for an `image:<destination>`, the Containerfile or os/components.toml that installs it."""
    if not path.startswith("image:"):
        return path
    for name in CONTAINERFILES:
        if path in copied_python(root, (name,)):
            return name
    return COMPONENTS.as_posix()


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
        print(error)
        return 1
    except (OSError, UnicodeError, subprocess.CalledProcessError) as error:
        print(f"  {error}", file=sys.stderr)
        return 1
    for path in new:
        source = declared_in(ROOT, path)
        what = "is" if source == path else f"{path} is"
        print(
            f"{source}: {what} an interpreter script in the OS image; ship compiled code instead "
            f"(an exception is a line in {BASELINE} with its reason, made in review)"
        )
    for path in stale:
        print(
            f"{BASELINE}:{baseline[path].line}: {path} no longer ships an interpreter script: "
            "remove this line"
        )
    if new or stale:
        print(f"No new interpreted code on the OS runtime path ({POLICY}).", file=sys.stderr)
        return 1
    print(f"Image overlay interpreter scripts: {len(baseline)} baselined, none new.", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

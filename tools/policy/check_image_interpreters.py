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

Any flagged file fails: there is no list of exceptions. A build tool that
would otherwise be copied into the image belongs in build scratch (``/run/``)
or is run from a bind mount, and the finished rootfs check
(``os/scripts/verify-no-python-runtime.sh``) holds the same line.

Build stages and CI tooling may use Python, so Containerfiles, ``os/scripts``,
``os/dev``, ``os/tests`` and ``os/kernel`` are not scanned. Untracked files are
ignored.
"""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[2]
SCANNED = ("os/usr", "os/etc", "dist/session")
# Test tooling next to the host payload; none of it is installed.
UNSCANNED = ("dist/session/tests/", "dist/session/vm-e2e/")
CONTAINERFILES = ("os/Containerfile", "os/Containerfile.installer")
COMPONENTS = Path("os/components.toml")
POLICY = "docs/policies.md, Dependencies and runtime"

INTERPRETER = re.compile(r"(python[0-9.]*|node|nodejs|deno|bun|perl[0-9.]*)")
SHELL = re.compile(r"(sh|bash|dash)")
PYTHON_COMMAND = re.compile(r"(^|[\s;|&(`$]|/)python(3(\.[0-9]+)?)?([\s;|&)`]|$)")
# Build-stage and scratch destinations: nothing there ships.
BUILD_DESTINATIONS = ("/usr/local/", "/run/", "/tmp/", "/tilo", "/build", "/out", "/src", "/tree/", "/image/", "/repo")
EXTENSIONS = (".py", ".js", ".mjs", ".cjs", ".pl")


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


def is_interpreter_script(path: Path, data: bytes | None = None) -> bool:
    """Whether an interpreter executes the shipped file at ``path``.

    ``data`` is the file's bytes from the index when a commit stages it; without
    it the file on disk is read.
    """
    if path.name.endswith(EXTENSIONS):
        return True
    if data is not None:
        first_line = data.split(b"\n", 1)[0][:512]
    else:
        try:
            with path.open("rb") as handle:
                first_line = handle.readline(512)
        except OSError as error:
            raise OSError(f"Cannot inspect tracked overlay file {path}: {error}") from error
    program = shebang_interpreter(first_line)
    if program is not None and INTERPRETER.fullmatch(program) is not None:
        return True
    if program is None or SHELL.fullmatch(program) is None:
        return False
    if data is not None:
        return names_python(data.decode("utf-8", "replace"))
    return names_python(path.read_text(encoding="utf-8", errors="replace"))


def names_python(text: str) -> bool:
    """Whether a shell script names Python as a command on any non-comment line."""
    for line in text.splitlines():
        if line.lstrip().startswith("#"):
            continue
        if PYTHON_COMMAND.search(line):
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


def staged_files(root: Path, *paths: str) -> list[str]:
    """The paths a commit adds or changes under ``paths``, read from the index."""
    output = subprocess.check_output(
        ["git", "-C", str(root), "diff", "--cached", "--name-only", "-z",
         "--diff-filter=ACMR", "--", *paths],
    )
    return sorted(name.decode() for name in output.split(b"\0") if name)


def index_bytes(root: Path, path: str) -> bytes:
    """The bytes the commit stages for ``path``."""
    return subprocess.check_output(["git", "-C", str(root), "show", ":" + path])


def staged_shipped_scripts(root: Path) -> list[str]:
    """Shipped scripts a commit adds or changes, read from the index.

    Only these can be new to the image: a file the commit leaves as HEAD has it
    was judged by the commit that added it, and the whole overlay by the policy
    suite and CI on every pushed head.
    """
    changed = set(staged_files(root, *SCANNED, *CONTAINERFILES, COMPONENTS.as_posix()))
    shipped = [
        name
        for name in sorted(changed)
        if name.startswith(SCANNED) and not name.startswith(UNSCANNED)
        and is_interpreter_script(root / name, index_bytes(root, name))
    ]
    containerfiles = tuple(name for name in CONTAINERFILES if name in changed)
    if containerfiles:
        shipped.extend(copied_python(root, containerfiles))
    if COMPONENTS.as_posix() in changed:
        shipped.extend(component_python(root))
    return sorted(set(shipped))


def check(root: Path = ROOT, staged: bool = False) -> list[str]:
    """The interpreter scripts the image would carry.

    ``staged`` judges the files the commit adds or changes, read from the index;
    the policy suite and CI judge every shipped file of the tree.
    """
    return staged_shipped_scripts(root) if staged else shipped_scripts(root)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--staged",
        action="store_true",
        help="judge only the shipped files this commit adds or changes, read from the "
        "index, as the pre-commit hook does; without it every file of the overlay is "
        "judged, as the policy suite and CI do",
    )
    staged = parser.parse_args(argv).staged
    try:
        flagged = check(ROOT, staged)
    except (OSError, UnicodeError, subprocess.CalledProcessError) as error:
        print(f"  {error}", file=sys.stderr)
        return 1
    for path in flagged:
        source = declared_in(ROOT, path)
        what = "is" if source == path else f"{path} is"
        print(
            f"{source}: {what} an interpreter script in the OS image; ship compiled code instead, "
            "or keep a build tool in build scratch under /run"
        )
    if flagged:
        print(f"No interpreted code on the OS runtime path ({POLICY}).", file=sys.stderr)
        return 1
    judged = "staged" if staged else "shipped"
    print(f"Image overlay interpreter scripts: none {judged}.", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

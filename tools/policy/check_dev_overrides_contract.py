#!/usr/bin/env python3
"""Keep developer overrides where they belong, and make every documented one real.

The shell's `dev-overrides` feature makes a release binary honour a fixed
lock-screen password (`TILO_LOCK_TEST_PASSWORD`), a replacement unlock helper
and a socket that drives the session as the user (`TILO_TEST_CONTROL`).
`dev-session` stamps the marker the host developer-session entry requires.
Both exist for dev images only. `agent-control` binds a socket that drives a
headless session for hosted agent machines; it is not for the desktop or OS
images either. `hosted-test-control` carries the shell's test seams and a
sampling heap allocator, for test builds alone. `--all-features` would turn
them on too.

Every `os/Containerfile*` is a production build except the dev image
(`os/Containerfile.dev`), which is the one place these features belong.

The same overrides are documented for people: `docs/performance-flags.md` is
the reference for the runtime's own environment controls, and a control it
documents that nothing reads is worse than undocumented — someone sets it,
measures nothing, and believes the flag did nothing. Each `TILO_*` the page
names must be read by the tree: a mention in Rust or in the embedder's C is
the runtime reading it, a Python line is a read when it goes through
`os.environ`, and a shell script reads one only by expanding it (`${FLAG}`) —
`export FLAG=1` in a launcher hands it to a program and is not one.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parents[2]
OS_DIR = ROOT / "os"
DEV_IMAGE_FILES = frozenset({"Containerfile.dev"})
FORBIDDEN = re.compile(
    r"(?<![\w-])(dev-overrides|dev-session|agent-control|hosted-test-control|--all-features)(?![\w-])"
)

DOCUMENTED_FLAGS = ROOT / "docs" / "performance-flags.md"
FLAG = re.compile(r"`(TILO_[A-Z0-9_]+)`")
# A file whose text can name an environment control the runtime reads.
SOURCE_SUFFIXES = frozenset({
    ".rs", ".py", ".sh", ".bash", ".c", ".h", ".cc", ".cpp", ".m", ".mm",
})
# Directories that describe or build the tree rather than run in it: a page
# naming a flag is what this check compares against, never a reader of it.
NOT_SOURCES = frozenset({"target", ".git", "docs", "node_modules"})


@dataclass(frozen=True)
class Violation:
    path: str
    line: int
    token: str

    def headline(self) -> str:
        """The finding, one line: what main prints on stdout."""
        return f"{self.path}:{self.line}: production image build enables `{self.token}`."

    def detail(self) -> str:
        """Why and the fix: what main prints on stderr after the headline."""
        return (
            "  Why: `dev-overrides` binaries accept a fixed lock-screen password and open a\n"
            "  socket that drives the session as the user; `dev-session` marks a binary as\n"
            "  loggable by the developer session entry; `agent-control` lets a local socket drive\n"
            "  the session; `hosted-test-control` carries test seams and a sampling heap\n"
            "  allocator. None may ship in a production image.\n"
            "  Fix: remove the feature from this build. Dev-only binaries belong in\n"
            "  os/Containerfile.dev (the dev image), never in a production Containerfile."
        )

    def __str__(self) -> str:
        return f"{self.headline()}\n{self.detail()}"


def production_containerfiles(os_dir: Path = OS_DIR) -> list[Path]:
    """Return every production Containerfile, and the compile the OS image packs
    (os/image/compile.sh), in a stable order."""
    recipes = [
        path
        for path in os_dir.glob("Containerfile*")
        if path.is_file() and path.name not in DEV_IMAGE_FILES
    ]
    compile_script = os_dir / "image" / "compile.sh"
    return sorted(recipes + ([compile_script] if compile_script.is_file() else []))


def violations_in(path: Path, root: Path = ROOT) -> list[Violation]:
    """Report forbidden feature tokens on non-comment lines of one file."""
    try:
        shown = str(path.relative_to(root))
    except ValueError:
        shown = str(path)
    found: list[Violation] = []
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if line.lstrip().startswith("#"):
            continue
        found.extend(
            Violation(shown, number, match.group(1)) for match in FORBIDDEN.finditer(line)
        )
    return found


def audit(os_dir: Path = OS_DIR, root: Path = ROOT) -> list[Violation]:
    violations: list[Violation] = []
    for path in production_containerfiles(os_dir):
        violations.extend(violations_in(path, root))
    return violations


@dataclass(frozen=True)
class UnreadFlag:
    """A documented environment control that no code in the tree reads."""

    path: str
    line: int
    flag: str

    def headline(self) -> str:
        return f"{self.path}:{self.line}: `{self.flag}` is documented and nothing reads it."

    def detail(self) -> str:
        return (
            "  Why: someone sets it, measures nothing, and believes the run said the flag\n"
            "  made no difference. An override is documented only while the code reads it.\n"
            "  Fix: delete its row (the behaviour or the override is gone), or restore the\n"
            "  read if the control is meant to exist."
        )

    def __str__(self) -> str:
        return f"{self.headline()}\n{self.detail()}"


def documented_flags(path: Path = DOCUMENTED_FLAGS) -> list[tuple[int, str]]:
    """Per (line, flag), every environment control the page names."""
    found = []
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        found.extend((number, flag) for flag in FLAG.findall(line))
    return found


def source_files(root: Path) -> list[Path]:
    """Every file in the tree that can read one: code, and the scripts that start it.

    A launcher or helper shipped in an image carries no suffix (`os/usr/libexec/
    tilo-session`), so its shebang decides, not its name."""
    found = []
    for path in root.rglob("*"):
        try:
            relative = path.relative_to(root)
        except ValueError:
            continue
        if not path.is_file() or NOT_SOURCES & set(relative.parts):
            continue
        if path.suffix in SOURCE_SUFFIXES:
            found.append(path)
        elif path.suffix == "" and path.read_bytes()[:2] == b"#!":
            found.append(path)
    return found


def reads(flag: str, path: Path, text: str) -> bool:
    """Whether `path`'s text reads `flag`'s value, rather than handing it on."""
    if flag not in text:
        return False
    # Rust and the embedder's C read their own controls through the environment.
    if path.suffix in {".rs", ".c", ".h", ".cc", ".cpp", ".m", ".mm"}:
        return True
    if path.suffix == ".py":
        return any(
            flag in line and ("environ" in line or "getenv" in line)
            for line in text.splitlines()
        )
    # A script reads one by expanding it; `export FLAG=1` only hands it to a child.
    return f"${flag}" in text or f"${{{flag}" in text


def unread_flags(root: Path = ROOT, doc: Path = DOCUMENTED_FLAGS) -> list[UnreadFlag]:
    """Every control `doc` documents that no source in `root` reads."""
    documented = documented_flags(doc)
    unread = {flag for _, flag in documented}
    for path in source_files(root):
        if not unread:
            break
        text = path.read_text(encoding="utf-8", errors="ignore")
        if "TILO_" not in text:
            continue
        unread = {flag for flag in unread if not reads(flag, path, text)}
    try:
        shown = doc.relative_to(root).as_posix()
    except ValueError:
        shown = str(doc)
    reported: set[str] = set()
    found = []
    for line, flag in documented:
        if flag in unread and flag not in reported:
            reported.add(flag)
            found.append(UnreadFlag(shown, line, flag))
    return found


def main() -> int:
    violations = audit()
    unread = unread_flags()
    if violations or unread:
        for violation in violations:
            sys.stderr.flush()
            print(violation.headline(), flush=True)
            print(violation.detail(), file=sys.stderr)
        for flag in unread:
            sys.stderr.flush()
            print(flag.headline(), flush=True)
            print(flag.detail(), file=sys.stderr)
        return 1
    count = len(production_containerfiles())
    controls = len({flag for _, flag in documented_flags()})
    print(
        f"Dev-override contract OK ({count} production Containerfiles, "
        f"{controls} documented controls read by the tree)",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

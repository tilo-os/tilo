#!/usr/bin/env python3
"""Keep developer-override cargo features out of production image builds.

The shell's `dev-overrides` feature makes a release binary honour a fixed
lock-screen password (`TILO_LOCK_TEST_PASSWORD`), a replacement unlock helper
and a socket that drives the session as the user (`TILO_TEST_CONTROL`).
`dev-session` stamps the marker the host developer-session entry requires.
Both exist for dev images only. `agent-control` binds a socket that drives a
headless session for hosted agent machines; it is not for the desktop or OS
images either. `--all-features` would turn them on too.

Every `os/Containerfile*` is a production build except the dev image
(`os/Containerfile.dev`), which is the one place these features belong.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import re


ROOT = Path(__file__).resolve().parents[2]
OS_DIR = ROOT / "os"
DEV_IMAGE_FILES = frozenset({"Containerfile.dev"})
FORBIDDEN = re.compile(
    r"(?<![\w-])(dev-overrides|dev-session|agent-control|--all-features)(?![\w-])"
)


@dataclass(frozen=True)
class Violation:
    path: str
    line: int
    token: str

    def __str__(self) -> str:
        return (
            f"{self.path}:{self.line}: production image build enables `{self.token}`.\n"
            "  Why: `dev-overrides` binaries accept a fixed lock-screen password and open a\n"
            "  socket that drives the session as the user; `dev-session` marks a binary as\n"
            "  loggable by the developer session entry; `agent-control` lets a local socket drive\n"
            "  the session. None may ship in a production image.\n"
            "  Fix: remove the feature from this build. Dev-only binaries belong in\n"
            "  os/Containerfile.dev (the dev image), never in a production Containerfile."
        )


def production_containerfiles(os_dir: Path = OS_DIR) -> list[Path]:
    """Return every production Containerfile in a stable order."""
    return sorted(
        path
        for path in os_dir.glob("Containerfile*")
        if path.is_file() and path.name not in DEV_IMAGE_FILES
    )


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


def main() -> int:
    violations = audit()
    if violations:
        for violation in violations:
            print(violation)
        return 1
    count = len(production_containerfiles())
    print(f"Dev-override feature contract OK ({count} production Containerfiles)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

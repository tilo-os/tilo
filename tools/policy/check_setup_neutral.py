#!/usr/bin/env python3
"""Keep one developer's machine out of the repository.

Docs, scripts and code describe generic mechanisms ("any cargo drop-in",
"build on another machine"), never one person's tooling or paths. Tracked
text files may not name:

* a private remote-cargo wrapper, or its environment variables and paths;
* a real user's home under `/var/home/`, macOS `/Users/` or Windows
  `C:\\Users\\`: examples use a placeholder home such as `/var/home/you`,
  `/Users/you` or `C:\\Users\\you`;
* a private scratch directory that only exists in one checkout.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[2]
# Split so this file does not name what it rejects.
WRAPPER = re.compile(r"(?<![A-Za-z0-9])r" r"cargo", re.IGNORECASE)
HOME = re.compile(r"/var/home/([A-Za-z0-9._-]+)")
PLACEHOLDER_HOMES = frozenset({"you", "user", "dev", "tilo", "account", "alice", "bob"})
# `/Users/...`, `C:\Users\...`, `C:/Users/...` and MSYS2's `/c/Users/...`; the
# lookbehind keeps URL paths such as `/api/v1/Users/...` out.
OTHER_HOME = re.compile(
    r"(?:(?<![A-Za-z0-9._-])/|\b[A-Za-z]:[\\/]+|(?<![A-Za-z0-9._-])/[a-z]/)Users[\\/]+([A-Za-z0-9][A-Za-z0-9._-]*)"
)
PLACEHOLDER_OTHER_HOMES = frozenset(
    {"you", "u", "user", "alice", "bob", "owner", "reader", "ordinary", "public", "default", "shared"}
)
SCRATCH = re.compile(r"\.super" r"powers/")
# Every violation contains one of these literal hints. This deliberately
# broader match only avoids splitting unrelated files into individual lines;
# the existing matchers below still determine every diagnostic.
REFERENCE_HINT = re.compile(
    r"r" r"cargo|/var/home/|Users|\.super" r"powers/", re.IGNORECASE
)


@dataclass(frozen=True)
class Violation:
    path: str
    line: int
    reason: str

    def __str__(self) -> str:
        return f"{self.path}:{self.line}: {self.reason}"


def line_violations(text: str, ignore_file: bool = False) -> list[str]:
    reasons = []
    if WRAPPER.search(text):
        reasons.append("names a private cargo wrapper; say 'any cargo drop-in' or $CARGO")
    for match in HOME.finditer(text):
        if match.group(1) not in PLACEHOLDER_HOMES:
            reasons.append(f"a real home ({match.group(0)}); use a placeholder such as /var/home/you")
    for match in OTHER_HOME.finditer(text):
        if match.group(1).lower() not in PLACEHOLDER_OTHER_HOMES:
            reasons.append(f"a real home ({match.group(0)}); use a placeholder such as /Users/you")
    # An ignore file naming the scratch directory is what keeps it out.
    if SCRATCH.search(text) and not ignore_file:
        reasons.append("points into a private scratch directory; link a tracked file or drop it")
    return reasons


def file_violations(path: Path, shown: str) -> list[Violation]:
    data = path.read_bytes()
    if b"\0" in data:
        return []
    text = data.decode("utf-8", errors="replace")
    if not REFERENCE_HINT.search(text):
        return []
    return [
        Violation(shown, number, reason)
        for number, line in enumerate(text.splitlines(), 1)
        for reason in line_violations(line, ignore_file=path.name == ".gitignore")
    ]


def tracked_files(root: Path = ROOT) -> list[str]:
    output = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"],
        check=True,
        capture_output=True,
    ).stdout
    return [name for name in output.decode().split("\0") if name]


def check(root: Path = ROOT) -> list[Violation]:
    found = []
    for name in tracked_files(root):
        path = root / name
        if path.is_file() and not path.is_symlink():
            found.extend(file_violations(path, name))
    return found


def main() -> int:
    found = check()
    if found:
        print("Setup-specific references in tracked files:")
        for violation in found:
            print(f"  {violation}")
        return 1
    print("No setup-specific references in tracked files.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

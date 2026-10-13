#!/usr/bin/env python3
"""Keep every ``AutoCommit`` apply path behind ``tilo_automerge::ApplyIncoming``.

An ``AutoCommit`` logs a patch event for every op it applies into its own
patch log, active or not, and only ever drains an active one; a replica that
nobody diffs keeps an event per remote op for as long as it lives.
``crates/doc/tilo-automerge`` wraps each apply path and drops the dead log.
Clippy's ``disallowed-methods`` enforces the rest, type-aware; this check keeps
that enforcement from being switched off quietly:

* Clippy reads only the nearest ``clippy.toml``, so every ``clippy.toml`` in
  the repository must ban each raw method;
* outside the wrapper crate, no file may both suppress
  ``clippy::disallowed_methods`` and call one of the raw methods.
"""

from __future__ import annotations

import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import line_number  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
WRAPPER_CRATE = Path("crates/doc/tilo-automerge")

BANNED = (
    "automerge::AutoCommit::apply_changes",
    "automerge::AutoCommit::apply_changes_batch",
    "automerge::AutoCommit::load_incremental",
    "automerge::AutoCommit::merge",
    "automerge::AutoCommit::isolate",
    "automerge::AutoCommit::integrate",
    "automerge::sync::SyncDoc::receive_sync_message",
)

SUPPRESSION = re.compile(r"#!?\[\s*(?:allow|expect)\s*\([^\]]*\bclippy::disallowed_methods\b")
# The raw calls by shape. `merge` takes `&mut AutoCommit`, which tells it apart from the many
# other `merge` methods; the rest are distinctive by name.
RAW_CALL = re.compile(
    r"\.(?:apply_changes(?:_batch)?|load_incremental|isolate|integrate)\s*\("
    r"|\.merge\s*\(\s*&mut\b"
    r"|\breceive_sync_message\s*\("
)


@dataclass(frozen=True)
class Violation:
    path: str
    line: int
    reason: str

    def __str__(self) -> str:
        location = f"{self.path}:{self.line}" if self.line else self.path
        return f"{location}: {self.reason}"


def tracked(root: Path, pattern: str) -> list[Path]:
    """Files git tracks (or would add) under `root` matching `pattern`."""
    listed = subprocess.run(
        ["git", "-C", str(root), "ls-files", "--cached", "--others", "--exclude-standard", pattern],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return [Path(line) for line in listed.splitlines() if (root / line).is_file()]


def banned_paths(config: dict) -> set[str]:
    paths = set()
    for entry in config.get("disallowed-methods", []):
        if isinstance(entry, str):
            paths.add(entry)
        elif isinstance(entry, dict) and isinstance(entry.get("path"), str):
            paths.add(entry["path"])
    return paths


def audit_clippy_configs(root: Path, configs: list[Path]) -> list[Violation]:
    violations = []
    for relative in configs:
        config = tomllib.loads((root / relative).read_text(encoding="utf-8"))
        missing = [path for path in BANNED if path not in banned_paths(config)]
        for path in missing:
            violations.append(
                Violation(str(relative), 0, f"disallowed-methods does not ban {path}")
            )
    return violations


def audit_sources(root: Path, sources: list[Path]) -> list[Violation]:
    violations = []
    for relative in sources:
        if relative.is_relative_to(WRAPPER_CRATE):
            continue
        text = (root / relative).read_text(encoding="utf-8")
        suppression = SUPPRESSION.search(text)
        if suppression is None:
            continue
        call = RAW_CALL.search(text)
        if call is None:
            continue
        violations.append(
            Violation(
                str(relative),
                line_number(text, call.start()),
                "calls a raw AutoCommit apply path in a file that suppresses "
                "clippy::disallowed_methods; use tilo_automerge::ApplyIncoming",
            )
        )
    return violations


def audit(root: Path = ROOT) -> list[Violation]:
    configs = [p for p in tracked(root, "*clippy.toml") if p.name == "clippy.toml"]
    if Path("clippy.toml") not in configs:
        return [Violation("clippy.toml", 0, "missing")]
    sources = tracked(root, "*.rs")
    return audit_clippy_configs(root, configs) + audit_sources(root, sources)


def main() -> int:
    violations = audit()
    if violations:
        print("Automerge apply contract FAILED", file=sys.stderr)
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        return 1
    print("Automerge apply contract OK (AutoCommit takes changes through ApplyIncoming)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

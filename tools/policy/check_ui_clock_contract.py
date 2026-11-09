#!/usr/bin/env python3
"""Keep the UI toolkit's clock behind ``ui::time``.

The toolkit (``crates/sdk/ui-core``), its host services
(``crates/sdk/tilo-host``) and the collab client under it
(``crates/collab/collaboration``) name their clock types through
``crate::time`` —
defined once, in ``tilo-host``'s ``src/time.rs``, and re-exported by the
toolkit — so that a host without a native process clock has one module to
provide. Clippy enforces the calls (``Instant::now``/``SystemTime::now``) from
each crate's own ``clippy.toml``, but it cannot tell ``crate::time::Instant``
from the ``std::time::Instant`` it re-exports, so this check covers the type
paths:

* no ``std::time::Instant``, ``std::time::SystemTime`` or
  ``std::time::UNIX_EPOCH`` in production sources outside the clock module;
  test modules and test files are exempt;
* Clippy reads only the nearest ``clippy.toml``, so every key of the root file
  must be repeated in each crate's file with the same value; a list (such as
  ``disallowed-methods``) may extend the root's list but must keep every entry.

It also keeps the wall clock's two pins in their places: ``pin_wall`` is a
capture's, held for the rest of the process because a capture draws every
frame of it from the thread that pins it, and ``hold_wall_everywhere`` is a
test's, which may hold the process only where ``tilo_test_env::in_own_process``
gave it one to itself. Either one reached from anywhere else stops the clock
under tests that never asked for it, so a file naming one must be a capture
launcher or an own-process test.
"""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from judged import Judged, add_staged_argument, judged  # noqa: E402
from rust_source import is_test_file, line_number, production_source  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
CRATES = (
    Path("crates/sdk/ui-core"),
    Path("crates/sdk/tilo-host"),
    Path("crates/collab/collaboration"),
)
CLOCK_MODULE = Path("crates/sdk/tilo-host/src/time.rs")

CLOCK_ITEM = r"(?:Instant|SystemTime|UNIX_EPOCH)"
DIRECT_PATH = re.compile(rf"\bstd::time::{CLOCK_ITEM}\b")
BRACED_PATH = re.compile(rf"\bstd::time::\{{[^}}]*\b{CLOCK_ITEM}\b")
GROUPED_USE = re.compile(r"\buse\s+std::\{(?P<body>.*?)\};", re.DOTALL)
GROUPED_TIME = re.compile(rf"\btime::(?:{CLOCK_ITEM}\b|\{{[^}}]*\b{CLOCK_ITEM}\b)")

# The wall clock's two pins, and what a caller must have gone through first:
# `pin_wall` answers `wall_frozen()`, so it only runs once a capture has
# pinned the process, and `hold_wall_everywhere` only runs beside the call
# that gave the test the process to itself.
PROCESS_PIN = re.compile(r"\bhold_wall_everywhere\s*\(")
CAPTURE_PIN = re.compile(r"\bpin_wall\s*\(")
OWN_PROCESS_TEST = "tilo_test_env::in_own_process("
CAPTURE_ASKS = "wall_frozen()"
# The capture launcher itself: it pins the clock for the process its frames
# are drawn in, and no test reaches it.
CAPTURE_LAUNCHER = Path("crates/sdk/ui-core/src/clock.rs")


@dataclass(frozen=True)
class Violation:
    path: str
    line: int
    reason: str

    def __str__(self) -> str:
        location = f"{self.path}:{self.line}" if self.line else self.path
        return f"{location}: {self.reason}"


def audit_sources(files: Judged, crate: Path) -> list[Violation]:
    violations: list[Violation] = []
    for relative, source in files.sources(crate / "src", errors="strict"):
        if relative == CLOCK_MODULE or is_test_file(relative.relative_to(crate / "src")):
            continue
        text = production_source(source)
        offsets = [m.start() for m in DIRECT_PATH.finditer(text)]
        offsets += [m.start() for m in BRACED_PATH.finditer(text)]
        for grouped in GROUPED_USE.finditer(text):
            if GROUPED_TIME.search(grouped.group("body")):
                offsets.append(grouped.start())
        for offset in sorted(set(offsets)):
            violations.append(
                Violation(
                    str(relative),
                    line_number(text, offset),
                    "names a std::time clock type; use crate::time instead",
                )
            )
    return violations


def audit_pins(files: Judged) -> list[Violation]:
    """Every wall-clock pin in the tree sits where only its caller can reach it."""
    violations: list[Violation] = []
    for scope in (Path("crates"), Path("apps")):
        for relative, text in files.sources(scope, errors="strict"):
            hits = list(CAPTURE_PIN.finditer(text)) + list(PROCESS_PIN.finditer(text))
            if not hits:
                continue
            if relative == CAPTURE_LAUNCHER:
                continue
            for hit in hits:
                if PROCESS_PIN.match(hit.group(0)):
                    if OWN_PROCESS_TEST in text:
                        continue
                    reason = (
                        "holds the wall clock of the whole process without "
                        "`tilo_test_env::in_own_process`; the other tests of the "
                        "process read the time it holds"
                    )
                else:
                    if CAPTURE_ASKS in text:
                        continue
                    reason = (
                        "pins the wall clock of the whole process; only a capture "
                        "may (`ui::clock::freeze_for_capture`), because every "
                        "thread reads the time it pins"
                    )
                violations.append(
                    Violation(str(relative), line_number(text, hit.start()), reason)
                )
    return violations


def extends(crate_value: object, root_value: object) -> bool:
    """A crate list may add entries to a root list but must keep all of them."""
    return (
        isinstance(crate_value, list)
        and isinstance(root_value, list)
        and all(entry in crate_value for entry in root_value)
    )


def audit_clippy_config(files: Judged, crate: Path) -> list[Violation]:
    crate_config_path = crate / "clippy.toml"
    root_config = tomllib.loads(files.read("clippy.toml", errors="strict"))
    if not (files.root / crate_config_path).is_file():
        return [Violation(str(crate_config_path), 0, "missing")]
    crate_config = tomllib.loads(files.read(crate_config_path, errors="strict"))
    violations = []
    for key, value in root_config.items():
        if key not in crate_config:
            reason = f"does not repeat root clippy.toml key {key!r}"
        elif crate_config[key] != value and not extends(crate_config[key], value):
            reason = f"sets {key!r} differently from the root clippy.toml"
        else:
            continue
        violations.append(Violation(str(crate_config_path), 0, reason))
    return violations


def audit(root: Path = ROOT, staged: bool = False) -> list[Violation]:
    """Audit every toolkit crate present under ``root``.

    The clippy configuration of each crate is judged every time: it is a
    handful of files, and a change to the root ``clippy.toml`` alone can leave
    a crate's own file wrong.
    """
    files = judged(root, staged)
    violations: list[Violation] = []
    for crate in CRATES:
        if (root / crate).is_dir():
            violations += audit_clippy_config(files, crate) + audit_sources(files, crate)
    return violations + audit_pins(files)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    add_staged_argument(parser)
    violations = audit(staged=parser.parse_args(argv).staged)
    if violations:
        print("UI clock contract FAILED", file=sys.stderr)
        for violation in violations:
            print(violation)
        return 1
    print("UI clock contract OK (the toolkit and its host services read time through ui::time)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

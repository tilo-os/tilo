#!/usr/bin/env python3
"""Keep the UI toolkit's clock behind ``ui::time``.

``crates/sdk/ui-core`` names its clock types through ``crate::time`` so that a
host without a native process clock has one module to provide. Clippy enforces
the calls (``Instant::now``/``SystemTime::now``) from the crate's own
``clippy.toml``, but it cannot tell ``crate::time::Instant`` from the
``std::time::Instant`` it re-exports, so this check covers the type paths:

* no ``std::time::Instant``, ``std::time::SystemTime`` or
  ``std::time::UNIX_EPOCH`` in production sources outside ``src/time.rs``;
  test modules and test files are exempt;
* Clippy reads only the nearest ``clippy.toml``, so every key of the root file
  must be repeated in the crate's file with the same value; a list (such as
  ``disallowed-methods``) may extend the root's list but must keep every entry.
"""

from __future__ import annotations

import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CRATE = Path("crates/sdk/ui-core")
CLOCK_MODULE = CRATE / "src" / "time.rs"

CLOCK_ITEM = r"(?:Instant|SystemTime|UNIX_EPOCH)"
DIRECT_PATH = re.compile(rf"\bstd::time::{CLOCK_ITEM}\b")
BRACED_PATH = re.compile(rf"\bstd::time::\{{[^}}]*\b{CLOCK_ITEM}\b")
GROUPED_USE = re.compile(r"\buse\s+std::\{(?P<body>.*?)\};", re.DOTALL)
GROUPED_TIME = re.compile(rf"\btime::(?:{CLOCK_ITEM}\b|\{{[^}}]*\b{CLOCK_ITEM}\b)")
TEST_MODULE_MARKERS = ("\n#[cfg(test)]", "\n#[cfg(all(test")


@dataclass(frozen=True)
class Violation:
    path: str
    line: int
    reason: str

    def __str__(self) -> str:
        location = f"{self.path}:{self.line}" if self.line else self.path
        return f"{location}: {self.reason}"


def is_test_file(relative: Path) -> bool:
    name = relative.name
    return "tests" in relative.parts or name == "tests.rs" or name.endswith("_tests.rs")


def production_source(text: str) -> str:
    """Drop the conventional trailing test module and full-line comments.

    Comments are blanked rather than removed so line numbers stay accurate.
    """
    ends = [text.find(marker) for marker in TEST_MODULE_MARKERS if marker in text]
    source = text[: min(ends)] if ends else text
    return re.sub(r"(?m)^\s*//.*$", "", source)


def line_number(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def audit_sources(root: Path) -> list[Violation]:
    violations: list[Violation] = []
    src = root / CRATE / "src"
    for path in sorted(src.rglob("*.rs")):
        relative = path.relative_to(root)
        if relative == CLOCK_MODULE or is_test_file(relative.relative_to(CRATE / "src")):
            continue
        text = production_source(path.read_text(encoding="utf-8"))
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


def extends(crate_value: object, root_value: object) -> bool:
    """A crate list may add entries to a root list but must keep all of them."""
    return (
        isinstance(crate_value, list)
        and isinstance(root_value, list)
        and all(entry in crate_value for entry in root_value)
    )


def audit_clippy_config(root: Path) -> list[Violation]:
    crate_config_path = root / CRATE / "clippy.toml"
    root_config = tomllib.loads((root / "clippy.toml").read_text(encoding="utf-8"))
    if not crate_config_path.is_file():
        return [Violation(str(CRATE / "clippy.toml"), 0, "missing")]
    crate_config = tomllib.loads(crate_config_path.read_text(encoding="utf-8"))
    violations = []
    for key, value in root_config.items():
        if key not in crate_config:
            reason = f"does not repeat root clippy.toml key {key!r}"
        elif crate_config[key] != value and not extends(crate_config[key], value):
            reason = f"sets {key!r} differently from the root clippy.toml"
        else:
            continue
        violations.append(Violation(str(CRATE / "clippy.toml"), 0, reason))
    return violations


def audit(root: Path = ROOT) -> list[Violation]:
    return audit_clippy_config(root) + audit_sources(root)


def main() -> int:
    violations = audit()
    if violations:
        print("UI clock contract FAILED", file=sys.stderr)
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        return 1
    print("UI clock contract OK (ui-core reads time through ui::time)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

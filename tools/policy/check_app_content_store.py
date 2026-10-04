#!/usr/bin/env python3
"""Keep app content on its record's route.

An app keeps the bytes its records name (pictures, attachments) in the content
store its host lends the window, ``cx.content()``. That store is joined to the
record's route: what the app stores reaches the record's other readers, and
what the record references and this device lacks is asked of them. A store an
app opens itself, ``ContentStore::for_app()``, bypasses the route, so
production code outside the hosts that lend the store may not call it:

* the content crate, which defines it;
* the framework's native process host, which lends it to the window;
* the filesystem daemon, which serves it.

Test modules and test files are exempt.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCANNED = (Path("apps"), Path("crates"))
ALLOWED = (
    Path("crates/system/content"),
    Path("crates/sdk/tilo-app/src/framework/app.rs"),
    Path("crates/system/fs/src/main.rs"),
)
CALL = re.compile(r"\bContentStore\s*::\s*for_app\b")
TEST_MODULE_MARKERS = ("\n#[cfg(test)]", "\n#[cfg(all(test")


@dataclass(frozen=True)
class Violation:
    path: str
    line: int

    def __str__(self) -> str:
        return (
            f"{self.path}:{self.line}: opens its own content store; "
            "keep content in cx.content(), which travels on the record's route"
        )


def is_test_file(relative: Path) -> bool:
    name = relative.name
    return (
        "tests" in relative.parts
        or "benches" in relative.parts
        or name == "tests.rs"
        or name.endswith("_tests.rs")
    )


def production_source(text: str) -> str:
    """Drop the conventional trailing test module and full-line comments,
    keeping line numbers."""
    ends = [text.find(marker) for marker in TEST_MODULE_MARKERS if marker in text]
    source = text[: min(ends)] if ends else text
    return re.sub(r"(?m)^[ \t]*//.*$", "", source)


def allowed(relative: Path) -> bool:
    return any(relative == entry or entry in relative.parents for entry in ALLOWED)


def audit(root: Path = ROOT) -> list[Violation]:
    violations: list[Violation] = []
    for top in SCANNED:
        for path in sorted((root / top).rglob("*.rs")):
            relative = path.relative_to(root)
            if "target" in relative.parts or allowed(relative) or is_test_file(relative):
                continue
            text = production_source(path.read_text(encoding="utf-8", errors="replace"))
            for match in CALL.finditer(text):
                violations.append(
                    Violation(str(relative), text.count("\n", 0, match.start()) + 1)
                )
    return violations


def main() -> int:
    violations = audit()
    if violations:
        print("App content store contract FAILED", file=sys.stderr)
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        return 1
    print("App content store contract OK (apps keep content in cx.content())")
    return 0


if __name__ == "__main__":
    sys.exit(main())

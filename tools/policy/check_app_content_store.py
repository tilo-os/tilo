#!/usr/bin/env python3
"""Keep app content on its record's route.

An app keeps the bytes its records name (pictures, attachments) in the content
store its host lends the window, ``cx.content()``. That store is joined to the
record's route: what the app stores reaches the record's other readers, and
what the record references and this device lacks is asked of them. A store an
app opens itself, ``tilo_content_device::for_app()`` (or the local store under
it, ``ContentStore::local_for_app()``), bypasses the route, so production code
outside the hosts that lend the store may not call it:

* the content crates, which define it;
* the framework's native process host, which lends it to the window;
* the filesystem daemon, which serves it;
* the app runtime, which hosts apps and decides what their records receive
  against the app surfaces kept in the same store.

Nor may app code keep a content store in a ``static`` (a ``OnceLock``, a
``thread_local!``): every window has its own store, and two windows of one app
in one process (the shell mounts apps in its own) would share it. Code no view
reaches reads the window's store from the framework —
``ActionCx::content()`` while an action applies,
``tilo_resource::content::current()`` while the window's code runs — and work
leaving the window's thread captures it when it starts.

Test modules and test files are exempt.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import is_test_file, production_source  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
SCANNED = (Path("apps"), Path("crates"))
ALLOWED = (
    Path("crates/system/content"),
    Path("crates/system/content-device"),
    Path("crates/sdk/tilo-app/src/framework/app.rs"),
    Path("crates/system/fs/src/main.rs"),
    Path("crates/runtime/tilo-runtime/src/cli.rs"),
    Path("crates/runtime/tilo-runtime/src/standalone.rs"),
)
CALL = re.compile(
    r"\b(?:tilo_content_device\s*::\s*for_app|ContentStore\s*::\s*local_for_app)\b"
)
# A `static` (or a `thread_local!` item) whose type names a content store.
HELD = re.compile(
    r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?\s+)?static\s+(?:mut\s+)?\w+\s*:(?!:)[^=;]*\bContentStore\b"
)


OPENS = "opens its own content store; keep content in cx.content(), which travels on the record's route"
HOLDS = (
    "keeps a content store for the whole process; read the window's from the framework "
    "(ActionCx::content, tilo_resource::content::current)"
)


@dataclass(frozen=True)
class Violation:
    path: str
    line: int
    reason: str = OPENS

    def __str__(self) -> str:
        return f"{self.path}:{self.line}: {self.reason}"


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
            if top == Path("apps"):
                for match in HELD.finditer(text):
                    violations.append(
                        Violation(str(relative), text.count("\n", 0, match.start()) + 1, HOLDS)
                    )
    return violations


def main() -> int:
    violations = audit()
    if violations:
        print("App content store contract FAILED", file=sys.stderr)
        for violation in violations:
            print(violation)
        return 1
    print("App content store contract OK (apps keep content in cx.content())", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

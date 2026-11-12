#!/usr/bin/env python3
"""App copy resolves through the app's catalogue, so a key is never read raw.

A window's words live in its own [`EmbeddedCatalog`]: `CATALOG.t` and
`CATALOG.t_args` load the catalogue onto the thread and then resolve. The
manager's own lookups skip that step, so a path that paints before anything
else loaded the copy reads `disks-select-whole-disk` where a person should
read "Select whole disk". Those lookups are the toolkit's --
`I18nManager::lookup`, `I18nManager::lookup_plain` and `I18nManager::format`
are `pub(crate)` in `tilo-ui`, and the widgets that share one manager across
every window keep using them -- so app code cannot call them at all: an app
names its copy through its crate's catalogue, and the toolkit's own words
through `widget_text`.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

import rust_source as rust

ROOT = Path(__file__).resolve().parents[2]

# App code: the crates under `apps/`, the examples people copy an app from, and
# the templates `tilo-app` generates one from. The toolkit is not app code.
SCOPES = ("apps", "examples/apps")
TEMPLATES = "crates/dev/tilo-app-cli/templates"

# The manager's own resolution, called from an app: no catalogue is loaded by it.
BARE = re.compile(r"\bI18nManager\s*::\s*(?:lookup_plain|lookup|format)\s*\(")


def sources(root: Path) -> list[tuple[Path, Path]]:
    """Production sources in scope, with the path each is reported by.

    A test describes copy a person never reads on a thread of its own, and
    loads the catalogue it needs; only production code is judged.
    """
    found = [path for scope in SCOPES for path in sorted((root / scope).rglob("*.rs"))]
    found.extend(sorted((root / TEMPLATES).glob("*.rs")))
    return [(path, path.relative_to(root)) for path in found
            if not rust.is_test_file(path.relative_to(root))]


def check(root: Path) -> list[str]:
    """Every bare manager lookup in app code, as an error."""
    errors = []
    for path, relative in sources(root):
        # A source only resolves copy if its text names a resolver at all;
        # reading the manager's methods is what the whole check is about.
        raw = path.read_text()
        if BARE.search(raw) is None:
            continue
        text = rust.production_source(raw)
        name = relative.as_posix()
        for match in BARE.finditer(text):
            line = text.count("\n", 0, match.start()) + 1
            errors.append(f"{name}:{line}: {match.group(0).rstrip('(').strip()} reads the thread's "
                          "manager, not the app's catalogue; use the crate's CATALOG.t or "
                          "CATALOG.t_args")
    return errors


def main(argv: list[str]) -> int:
    errors = check(ROOT)
    if errors:
        print("\n".join(errors))
        return 1
    print("app copy resolution OK (every app lookup goes through its catalogue)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))

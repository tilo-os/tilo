#!/usr/bin/env python3
"""App copy resolves through the app's catalogue, so a key is never read raw.

A window's words live in its own [`EmbeddedCatalog`]: `CATALOG.t` and
`CATALOG.t_args` load the catalogue onto the thread and then resolve. The
manager's own lookups skip that step, so a path that paints before anything
else loaded the copy reads `disks-select-whole-disk` where a person should
read "Select whole disk". Those lookups are the toolkit's --
`I18nManager::lookup_plain` and `I18nManager::format` are `pub(crate)` in
`tilo-ui`, and the widgets that share one manager across every window keep
using them -- so app code cannot call them at all.

The `i18n!` macro expands to the same bare lookup and cannot be closed the
same way, because its call sites say nothing about which catalogue a key
belongs to. The apps that still use it are listed in
`app_copy_resolution_baseline.txt`, one line per file with how many calls it
may still make. A count only shrinks: more than listed fails, fewer fails
until the line is lowered, so a converted file stays converted.
"""

from __future__ import annotations

import re
import sys
from collections import Counter
from pathlib import Path

import rust_source as rust

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).resolve().parent / "app_copy_resolution_baseline.txt"

# App code: the crates under `apps/`, the examples people copy an app from, and
# the templates `tilo-app` generates one from. The toolkit is not app code.
SCOPES = ("apps", "examples/apps")
TEMPLATES = "crates/dev/tilo-app-cli/templates"

# The manager's own resolution, called from an app: no catalogue is loaded by it.
BARE = re.compile(r"\bI18nManager\s*::\s*(?:lookup_plain|lookup|format)\s*\(")
MACRO = re.compile(r"\bi18n\s*!\s*\(")


def sources(root: Path) -> list[tuple[Path, Path]]:
    """Production sources in scope, with the path each is reported by.

    A test describes copy a person never reads on a thread of its own, and
    loads the catalogue it needs; only production code is judged.
    """
    found = [path for scope in SCOPES for path in sorted((root / scope).rglob("*.rs"))]
    found.extend(sorted((root / TEMPLATES).glob("*.rs")))
    return [(path, path.relative_to(root)) for path in found
            if not rust.is_test_file(path.relative_to(root))]


def scan(root: Path) -> tuple[list[str], Counter[str]]:
    """Bare manager lookups, and the `i18n!` calls each file still makes."""
    bare, macro = [], Counter()
    for path, relative in sources(root):
        text = rust.production_source(path.read_text())
        name = relative.as_posix()
        for match in BARE.finditer(text):
            line = text.count("\n", 0, match.start()) + 1
            bare.append(f"{name}:{line}: {match.group(0).rstrip('(').strip()} reads the thread's "
                        "manager, not the app's catalogue; use the crate's CATALOG.t or "
                        "CATALOG.t_args")
        count = len(MACRO.findall(text))
        if count:
            macro[name] = count
    return bare, macro


def load_baseline(path: Path) -> dict[str, int]:
    allowed: dict[str, int] = {}
    if not path.exists():
        return allowed
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        count, file = line.split(maxsplit=1)
        allowed[file.strip()] = int(count)
    return allowed


def print_baseline(root: Path) -> None:
    print("# App files whose copy still goes through the `i18n!` macro, which")
    print("# resolves on the thread's manager without loading the app's catalogue")
    print("# (tools/policy/check_app_copy_resolution.py). Each file converts to")
    print("# the crate's CATALOG.t / CATALOG.t_args; counts only shrink.")
    _, macro = scan(root)
    for file, count in sorted(macro.items()):
        print(f"{count} {file}")


def check(root: Path, baseline_path: Path) -> list[str]:
    bare, macro = scan(root)
    allowed = load_baseline(baseline_path)
    shown = (baseline_path.relative_to(root).as_posix()
             if baseline_path.is_relative_to(root) else baseline_path.name)
    errors = list(bare)
    for file in sorted(set(macro) | set(allowed)):
        have, may = macro.get(file, 0), allowed.get(file, 0)
        if have > may:
            errors.append(f"{file}: {have} i18n! calls (allowed {may}); resolve through the "
                          "crate's CATALOG.t / CATALOG.t_args so the catalogue is loaded")
        elif have < may:
            errors.append(f"{shown}: {may} {file} is now {have}; lower the count "
                          "(or delete the line at 0)")
    return errors


def main(argv: list[str]) -> int:
    if argv[1:] == ["--print-baseline"]:
        print_baseline(ROOT)
        return 0
    errors = check(ROOT, BASELINE)
    if errors:
        print("\n".join(errors))
        return 1
    print("app copy resolution OK (every app lookup goes through its catalogue)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))

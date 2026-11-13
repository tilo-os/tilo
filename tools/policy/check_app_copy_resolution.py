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

An app names itself there too. `ResourceApp::TITLE_KEY` is the message a
window, a launcher entry and a dock row read, resolved through the app's
catalogue by the framework, so a German window is titled "Kalender" and not in
the source language: every app that ships a catalogue sets it, and the message
is in the catalogue the crate ships.
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

# An app's implementation of its resource: what the framework builds a window,
# a launcher entry and a dock row from.
APP = re.compile(r"impl(?:\s+[\w:]+)?\s+ResourceApp\s+for\s+\w+\s*\{")
TITLE_KEY = re.compile(
    r"""const\s+TITLE_KEY:\s*&'static\s+str\s*=\s*"([A-Za-z][\w-]*)"\s*;"""
)
# A catalogue's message keys, as Fluent defines them.
MESSAGE = re.compile(r"^([A-Za-z][\w-]*)\s*=", re.MULTILINE)


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


def titles(root: Path) -> list[str]:
    """Every app's title named in its own catalogue, as an error.

    The message is resolved against the catalogue the crate ships, so an app
    whose crate has no catalogue is left alone: a fixture, a package the host
    runs without one.
    """
    errors = []
    for app in sorted((root / "apps").iterdir()):
        catalogue = app / "locales" / "en-US.ftl"
        if not catalogue.exists() or not app.is_dir():
            continue
        messages = set(MESSAGE.findall(catalogue.read_text()))
        for source in sorted((app / "src").rglob("*.rs")):
            relative = source.relative_to(root)
            if rust.is_test_file(relative):
                continue
            text = rust.production_source(source.read_text())
            for match in APP.finditer(text):
                impl = text[match.end() - 1:impl_end(text, match.end() - 1)]
                name = match.group(0).split(" for ")[-1].split("{")[0].strip()
                line = text.count("\n", 0, match.start()) + 1
                key = TITLE_KEY.search(impl)
                if key is None:
                    errors.append(
                        f"{relative.as_posix()}:{line}: {name} names no TITLE_KEY, so its window "
                        "is titled in the source language alone"
                    )
                elif key.group(1) not in messages:
                    errors.append(
                        f"{relative.as_posix()}:{line}: {name}'s TITLE_KEY {key.group(1)!r} is not "
                        f"in {catalogue.relative_to(root).as_posix()}"
                    )
    return errors


def impl_end(code: str, at: int) -> int:
    """The offset of the `}` closing the block whose `{` is at `at`."""
    depth = 0
    for index in range(at, len(code)):
        if code[index] == "{":
            depth += 1
        elif code[index] == "}":
            depth -= 1
            if depth == 0:
                return index
    return len(code)


def main(argv: list[str]) -> int:
    errors = check(ROOT) + titles(ROOT)
    if errors:
        print("\n".join(errors))
        return 1
    print("app copy resolution OK (every app lookup goes through its catalogue)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))

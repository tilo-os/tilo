#!/usr/bin/env python3
"""Every record an app publishes answers a read.

A record is a resource type, and a resource type is what an agent, a script or
the SDK addresses by path (`record:<app>:<record>`, `tilo_read`,
`Document.load()`). Reading one is the framework's `read` action, which a
resource publishes when its `#[resource(...)]` declaration names a document
read (`read_content`, or `read_content_in_doc` for a read that looks at the
document too). Without one, a caller with no window open is told the record
offers no read at all: it can neither see what the record holds nor learn which
actions to take on it.

So every record this tree publishes names its read: the apps under `apps/`, and
the shared crates that declare an app's record for it (`model-providers`, the
Contact identities and workspace agents and emoji an app keeps beside its own,
the person's accounts, a shell's environment). A record that truly has no
content to answer is still not left out silently: it answers with what it holds
(nothing), so a caller learns that instead of a missing action.

Two kinds of declaration are left out. A template or an example is sample text
a tool copies, not code this tree builds. The session's own surfaces
(`crates/session/`) are drawn by the session process itself; no record path
names one, so no caller can hold one to read.

An exception is an entry in `EXCEPTIONS` below, with the reason it is one, and
each entry is checked to still have a declaration and still lack a read.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]

# `apps/<app>/src/<file>.rs::<kind>` -> why this record answers no read.
# Keep this empty: a record without a read cannot be read by path, and no
# present record is better off unreadable.
EXCEPTIONS: dict[str, str] = {}

# Where the records this tree publishes are declared.
SCOPES = ("apps", "crates")
# Sample text a tool copies rather than code this tree builds.
SAMPLES = ("templates", "examples")
# The session's own surfaces: their records are the session process's, and no
# record path names one.
UNREAD = ("crates/session",)

DECLARATION = re.compile(r"#\[resource\(")
KIND = re.compile(r'\bkind\s*=\s*"([^"]*)"')
# `read_content`, and `read_content_in_doc` for a read that looks at the
# document too.
READS = re.compile(r"\bread_content(?:_in_doc)?\b")
ADVICE = ("name the read the record answers, `read_content = "
          "<Type>::read_document` (tilo_resource::read::fields is the body for "
          "a record whose fields are all it holds)")

# A record that answers no read is a record no caller can read by path, so
# every exception is one that has to be argued for in the review that adds it.


def declarations(text: str) -> list[tuple[int, str, str]]:
    """The `#[resource(...)]` declarations of `text`: where each starts, its
    kind (empty when it names none), and its arguments."""
    code = rust_source.code_only(text)
    produced = []
    for match in DECLARATION.finditer(code):
        end = rust_source.matching(code, match.end() - 1)
        arguments = text[match.end() - 1:end]
        kind = KIND.search(arguments)
        produced.append((match.start(), kind.group(1) if kind else "", arguments))
    return produced


def published(relative: Path) -> bool:
    """Whether `relative` is code this tree builds and a record path can name."""
    if ("target" in relative.parts or "src" not in relative.parts
            or any(part.startswith(".") for part in relative.parts)
            or rust_source.is_test_file(relative)):
        return False
    if any(part in SAMPLES for part in relative.parts):
        return False
    return not any(
        relative.as_posix().startswith(unread) for unread in UNREAD
    )


def scan(root: Path) -> dict[str, list[tuple[str, int, bool]]]:
    """Every record declaration this tree publishes, keyed by `path::kind`, as
    `(path, line, answers a read?)`."""
    found: dict[str, list[tuple[str, int, bool]]] = {}
    paths = [path for scope in SCOPES if (root / scope).is_dir()
             for path in sorted((root / scope).rglob("*.rs"))]
    for path in paths:
        relative = path.relative_to(root)
        if not published(relative):
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        if "#[resource(" not in text:
            continue
        source = rust_source.production_source(text)
        for offset, kind, arguments in declarations(source):
            if not kind:
                continue
            line = rust_source.line_number(source, offset)
            key = f"{relative.as_posix()}::{kind}"
            found.setdefault(key, []).append(
                (relative.as_posix(), line, bool(READS.search(arguments)))
            )
    return found


def check(root: Path) -> list[str]:
    errors = []
    found = scan(root)
    for key, entries in sorted(found.items()):
        for path, line, readable in entries:
            if readable or key in EXCEPTIONS:
                continue
            errors.append(f"{path}:{line}: a record publishes no read ({key.split('::')[1]}); "
                          f"{ADVICE}")
    for key, reason in sorted(EXCEPTIONS.items()):
        entries = found.get(key)
        if not entries:
            errors.append(f"{key}: the exception names a record that is not declared; "
                          "remove it")
            continue
        if all(readable for _, _, readable in entries):
            errors.append(f"{key}: the exception is not needed, the record reads "
                          f"({reason}); remove it")
        elif not reason.strip():
            errors.append(f"{key}: the exception gives no reason")
    return errors


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args(argv[1:])
    errors = check(ROOT)
    if errors:
        print("\n".join(errors))
        return 1
    print("record reads OK (every record an app publishes answers a read)",
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

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

So every record this tree publishes answers a read, or declares that it does
not: the apps under `apps/`, and
the shared crates that declare an app's record for it (`messages`, the
Contact identities and workspace agents and emoji an app keeps beside its own,
the person's accounts, a shell's environment). A record that truly has no
content to answer is still not left out silently: it answers with what it holds
(nothing), so a caller learns that instead of a missing action.

Two kinds of declaration are left out. A template or an example is sample text
a tool copies, not code this tree builds. The session's own surfaces
(`crates/session/`) are drawn by the session process itself; no record path
names one, so no caller can hold one to read.

A record that must not answer a read says so in its `#[resource(...)]`
declaration with `read = false`, beside the reason in its doc comment: what it
holds must not reach anyone holding its path (what a person typed). This check
refuses a record that answers no read without declaring it.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]

# Where the records this tree publishes are declared.
SCOPES = ("apps", "crates", "tools")
# Sample text a tool copies rather than code this tree builds.
SAMPLES = ("templates", "examples")
# The session's own surfaces: their records are the session process's, and no
# record path names one.
UNREAD = ("crates/session",)
# Crates of the same kind that live in other groups.
UNREAD += (
    "crates/services/media-controls",
)
# Crates of the same kind that live in other groups.
UNREAD += (
    "crates/desktop",
    "crates/os/audio-service",
    "crates/os/bluetooth-agent",
    "crates/core/session-client",
    "crates/os/connectivity-service",
    "crates/services/crash-reports",
    "crates/core/identity-authority",
    "crates/os/network-secret-agent",
    "crates/os/polkit-agent",
    "crates/os/power-service",
    "crates/os/prepared-setup-model",
    "crates/core/session-actions",
    "crates/os/system-service",
    "crates/services/vision-service",
    "crates/core/workspace-directory",
)
# Crates of the same kind that live in other groups.
UNREAD += (
    "crates/os/console-control",
    "crates/os/greetd-client",
    "crates/os/greeter",
    "crates/os/install-plan",
    "crates/os/install-profile-model",
    "crates/integrations/messaging-service",
    "crates/os/tilo-ntfs-helper",
    "crates/os/tilo-ntfs-protocol",
)

DECLARATION = re.compile(r"#\[resource\(")
KIND = re.compile(r'\bkind\s*=\s*"([^"]*)"')
# `read_content`, and `read_content_in_doc` for a read that looks at the
# document too.
READS = re.compile(r"\bread_content(?:_in_doc)?\b")
# `read = false`: the record answers no read on purpose, where its declaration
# says so.
UNREADABLE = re.compile(r"\bread\s*=\s*false\b")
ADVICE = ("name the read the record answers, `read_content = "
          "tilo_resource::read::fields` (for a record whose fields are all "
          "it holds) or a custom read function, or declare `read = false` "
          "where what it holds must not be read by anyone holding its path")


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


def scan(root: Path) -> dict[str, list[tuple[str, int, bool, bool]]]:
    """Every record declaration this tree publishes, keyed by `path::kind`, as
    `(path, line, answers a read?, declares it answers none)`."""
    found: dict[str, list[tuple[str, int, bool, bool]]] = {}
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
            found.setdefault(key, []).append((
                relative.as_posix(),
                line,
                bool(READS.search(arguments)),
                bool(UNREADABLE.search(arguments)),
            ))
    return found


def check(root: Path) -> list[str]:
    errors = []
    for key, entries in sorted(scan(root).items()):
        for path, line, readable, declared in entries:
            if readable or declared:
                continue
            errors.append(f"{path}:{line}: a record publishes no read ({key.split('::')[1]}); "
                          f"{ADVICE}")
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

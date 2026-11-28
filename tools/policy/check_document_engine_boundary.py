#!/usr/bin/env python3
"""Keep document content behind the document layer, not the CRDT engine.

Rich-text and spreadsheet content lives in tilo-doc resources. tilo-doc stores it on
Automerge today, but that is tilo-doc's business: an app that edits document
content through Automerge directly ties itself to the engine and to tilo-doc's
private schema, and blocks any later change of either. Two things are
therefore reserved to the crates that own a CRDT engine:

* ``rich_text_engine`` — Automerge's rich-text primitives (``automerge::marks``,
  ``ExpandMark``). Apps format text through the tilo-doc document API
  (``DocMut::add_mark``/``remove_mark``/``…_in``), not by marking Automerge
  text objects themselves.
* ``document_schema`` — tilo-doc's storage keys (``tilo_doc::schema::KEY_*``, the block
  fields ``BLOCK_TEXT``/``BLOCK_KIND``/``BLOCK_ATTRS``/``BLOCK_ORDER``/
  ``BLOCK_CHILDREN``, and the preservation and binary-descriptor fields).
  Outside tilo-doc they only serve to read or write tilo-doc's CRDT by hand.

Test modules and test files are exempt: tests may build a malformed document on
purpose. There is no baseline: any production use outside the crates that own
an engine fails.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import is_test_file, production_source  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]

# Crates that own a CRDT engine or the document layer itself.
ENGINE_OWNERS = (
    "crates/core/tilo-automerge/src/lib.rs",
    "crates/sdk/tilo-doc/",
    "crates/engines/tilo-doc-codecs/",
    # The rich-text resource: tilo-doc's rich text as a tilo-resource resource,
    # which places changes by tilo-doc's storage keys.
    "crates/sdk/tilo-document/",
    # The resource layer stores every resource type on the engine; its
    # generic machinery (history, recordings) copies any document's text
    # and marks.
    "crates/core/tilo-resource/",
    "crates/ui/ui-core/",
    "crates/ui/ui-charts/",
    "crates/ui/ui-math/",
    # Toolkit lab/shots scenarios: lived under ui-core (an owner) until moved
    # into this non-default member; they still exercise the same surfaces.
    "tools/ui-examples/",
    "crates/core/actions/",
    # A document's history, revert and undo log, moved below the toolkit: a
    # revert writes any document's text and marks back as they were.
    "crates/core/collab-types/src/history/",
    # The one store for every app's resources: it places resources and their
    # content documents itself, so it writes their text and marks.
    "crates/core/resource-store/",
)
SCANNED_ROOTS = ("apps", "crates", "tools", "examples")

RULES = {
    "rich_text_engine": re.compile(r"\bautomerge::marks\b|\bExpandMark\b"),
    "document_schema": re.compile(
        r"\btilo_doc::schema::(?:KEY_[A-Z_]+|BLOCK_(?:TEXT|KIND|ATTRS|ORDER|CHILDREN)"
        r"|PRESERVED_[A-Z_]+|BLOB_(?:MIME|LEN|W|H))\b"
        r"|\buse\s+tilo_doc::schema::\{[^}]*\b(?:KEY_[A-Z_]+|BLOCK_(?:TEXT|KIND|ATTRS|ORDER|CHILDREN)"
        r"|PRESERVED_[A-Z_]+|BLOB_(?:MIME|LEN|W|H))\b[^}]*\}"
    ),
}


@dataclass(frozen=True)
class Violation:
    path: str
    reason: str

    def __str__(self) -> str:
        return f"{self.path}: {self.reason}"


def count_uses(root: Path) -> dict[str, dict[str, int]]:
    counts: dict[str, dict[str, int]] = {rule: {} for rule in RULES}
    for top in SCANNED_ROOTS:
        base = root / top
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*.rs")):
            relative = path.relative_to(root)
            posix = relative.as_posix()
            if "/target/" in f"/{posix}" or posix.startswith(ENGINE_OWNERS):
                continue
            if is_test_file(relative):
                continue
            source = production_source(path.read_text(encoding="utf-8", errors="replace"))
            for rule, pattern in RULES.items():
                found = len(pattern.findall(source))
                if found:
                    counts[rule][posix] = found
    return counts


def audit(root: Path = ROOT) -> list[Violation]:
    counts = count_uses(root)
    return [
        Violation(
            path,
            f"{rule}: {found} use(s); go through the tilo-doc document API instead",
        )
        for rule in RULES
        for path, found in sorted(counts[rule].items())
    ]


def main() -> int:
    violations = audit()
    for violation in violations:
        print(violation)
    if violations:
        print(
            f"document engine boundary: {len(violations)} violation(s); see "
            "tools/policy/check_document_engine_boundary.py",
            file=sys.stderr,
        )
        return 1
    print("document engine boundary OK", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

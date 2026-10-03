#!/usr/bin/env python3
"""Keep document content behind the document layer, not the CRDT engine.

Rich-text and spreadsheet content lives in qdoc resources. qdoc stores it on
Automerge today, but that is qdoc's business: an app that edits document
content through Automerge directly ties itself to the engine and to qdoc's
private schema, and blocks any later change of either. Two things are
therefore reserved to the crates that own a CRDT engine:

* ``rich_text_engine`` — Automerge's rich-text primitives (``automerge::marks``,
  ``ExpandMark``). Apps format text through the qdoc document API
  (``DocMut::add_mark``/``remove_mark``/``…_in``), not by marking Automerge
  text objects themselves.
* ``document_schema`` — qdoc's storage keys (``qdoc::schema::KEY_*``, the block
  fields ``BLOCK_TEXT``/``BLOCK_KIND``/``BLOCK_ATTRS``/``BLOCK_ORDER``/
  ``BLOCK_CHILDREN``, and the preservation and binary-descriptor fields).
  Outside qdoc they only serve to read or write qdoc's CRDT by hand.

Test modules and test files are exempt: tests may build a malformed document on
purpose. Existing production uses are listed in
``document_engine_baseline.toml`` as a ratchet: a count may only go down (and
the table must follow it down), and a file not listed may not start.
"""

from __future__ import annotations

import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).with_name("document_engine_baseline.toml")

# Crates that own a CRDT engine or the document layer itself.
ENGINE_OWNERS = (
    "crates/doc/tilo-doc/",
    "crates/doc/tilo-doc-codecs/",
    # The rich-text resource: tilo-doc's rich text as a tilo-resource resource,
    # which places changes by tilo-doc's storage keys.
    "crates/doc/tilo-document/",
    # Pre-rename paths kept so a partial tree still classifies correctly.
    "crates/doc/qdoc/",
    "crates/doc/qdoc-codecs/",
    "crates/doc/qdoc-rich-text/",
    "crates/sdk/ui-core/",
    # Toolkit lab/shots scenarios: lived under ui-core (an owner) until moved
    # into this non-default member; they still exercise the same surfaces.
    "crates/dev/ui-examples/",
    "crates/collab/actions/",
    "crates/system/app-state/",
)
SCANNED_ROOTS = ("apps", "crates", "examples")

RULES = {
    "rich_text_engine": re.compile(r"\bautomerge::marks\b|\bExpandMark\b"),
    "document_schema": re.compile(
        r"\bqdoc::schema::(?:KEY_[A-Z_]+|BLOCK_(?:TEXT|KIND|ATTRS|ORDER|CHILDREN)"
        r"|PRESERVED_[A-Z_]+|BLOB_(?:MIME|LEN|W|H))\b"
        r"|\buse\s+qdoc::schema::\{[^}]*\b(?:KEY_[A-Z_]+|BLOCK_(?:TEXT|KIND|ATTRS|ORDER|CHILDREN)"
        r"|PRESERVED_[A-Z_]+|BLOB_(?:MIME|LEN|W|H))\b[^}]*\}"
    ),
}
TEST_MODULE_MARKERS = ("\n#[cfg(test)]", "\n#[cfg(all(test")
LINE_COMMENT = re.compile(r"^\s*//.*$", re.MULTILINE)


@dataclass(frozen=True)
class Violation:
    path: str
    reason: str

    def __str__(self) -> str:
        return f"{self.path}: {self.reason}"


def is_test_file(relative: Path) -> bool:
    name = relative.name
    return (
        "tests" in relative.parts
        or "benches" in relative.parts
        or name == "tests.rs"
        or name.endswith("_tests.rs")
    )


def production_source(text: str) -> str:
    """The source up to its conventional trailing test module, without line comments."""
    ends = [text.find(marker) for marker in TEST_MODULE_MARKERS if marker in text]
    if ends:
        text = text[: min(ends)]
    return LINE_COMMENT.sub("", text)


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


def load_baseline(path: Path) -> dict[str, dict[str, int]]:
    data = tomllib.loads(path.read_text(encoding="utf-8")) if path.exists() else {}
    ratchets = data.get("ratchets", {})
    return {rule: dict(ratchets.get(rule, {})) for rule in RULES}


def audit(root: Path = ROOT, baseline_path: Path = BASELINE) -> list[Violation]:
    counts = count_uses(root)
    baseline = load_baseline(baseline_path)
    violations: list[Violation] = []
    for rule in RULES:
        allowed = baseline[rule]
        actual = counts[rule]
        for path, found in sorted(actual.items()):
            limit = allowed.get(path, 0)
            if found > limit:
                violations.append(
                    Violation(
                        path,
                        f"{rule}: {found} use(s), baseline allows {limit}; "
                        "go through the qdoc document API instead",
                    )
                )
        for path, limit in sorted(allowed.items()):
            found = actual.get(path, 0)
            if found < limit:
                violations.append(
                    Violation(
                        path,
                        f"{rule}: baseline allows {limit} but only {found} remain; "
                        f"lower the entry in {baseline_path.name}",
                    )
                )
    return violations


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
    print("document engine boundary OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

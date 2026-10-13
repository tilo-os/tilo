#!/usr/bin/env python3
"""Widgets never write the document.

A widget in the toolkit (``crates/sdk/ui-core/src/widget``) shows the
document and keeps what the user does local; its owner takes the edit and
submits it as a resource action, which is what history attributes, undo
takes back, permissions check and a collaborator replays. A write from widget
code bypasses all of that, so production widget sources may not:

* call an Automerge write (``put``, ``put_object``, ``insert_object``,
  ``splice_text``, ``mark``/``unmark``, ``delete``, ``increment``) on a
  document handle;
* use the toolkit's write helpers (``crate::doc::put_*``/``splice_*``,
  ``put_if_changed``) or bind a value to a document key
  (``crate::doc_binding``).

Model helpers an owner applies inside its own action (the editable table's
``table::apply``) live outside ``widget/``. Test modules and test files are
exempt.

Writes that remain are counted per file in
``widget_document_writes_baseline.toml``: a count may only fall, and a file
that sheds writes records it (``--update``). The goal is an empty baseline.
"""

from __future__ import annotations

import re
import sys
import tomllib
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import is_test_file, production_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
WIDGETS = Path("crates/sdk/ui-core/src/widget")
BASELINE = Path("tools/policy/widget_document_writes_baseline.toml")

WRITES = re.compile(
    r"(?:\b(?:doc|state|document|model)\b(?:\.\w+)*|ctx\.state)\s*"
    r"\.(?:put|put_object|insert_object|splice_text|mark|unmark|delete|increment)\s*\("
)
HELPERS = re.compile(
    r"\bdoc::(?:put_\w+|splice_\w+|put_if_changed)\b|\bdoc_binding::|\bTransactable\b"
)


def violations(root: Path) -> list[str]:
    found: list[str] = []
    for path in sorted((root / WIDGETS).rglob("*.rs")):
        relative = path.relative_to(root)
        if is_test_file(path.relative_to(root / WIDGETS)):
            continue
        text = production_source(path.read_text(encoding="utf-8"))
        for pattern in (WRITES, HELPERS):
            for match in pattern.finditer(text):
                line = text.count("\n", 0, match.start()) + 1
                found.append(f"{relative}:{line}: widget writes the document ({match.group(0).strip()})")
    return found


def load_baseline(root: Path) -> dict[str, int]:
    path = root / BASELINE
    if not path.exists():
        return {}
    return {name: int(count) for name, count in tomllib.loads(path.read_text()).get("files", {}).items()}


def write_baseline(root: Path, counts: Counter[str]) -> None:
    lines = [
        "# Document writes left in toolkit widget code, per file.",
        "# Counts may only fall; tools/policy/check_widget_document_writes.py --update",
        "# records a file that shed some. The goal is an empty table.",
        "[files]",
    ]
    lines += [f'"{name}" = {count}' for name, count in sorted(counts.items())]
    (root / BASELINE).write_text("\n".join(lines) + "\n")


def main() -> int:
    found = violations(ROOT)
    counts = Counter(entry.split(":", 1)[0] for entry in found)
    if "--update" in sys.argv[1:]:
        write_baseline(ROOT, counts)
        print(f"recorded {sum(counts.values())} widget document write(s)")
        return 0
    baseline = load_baseline(ROOT)
    grown = [entry for entry in found if counts[entry.split(":", 1)[0]] > baseline.get(entry.split(":", 1)[0], 0)]
    shed = sorted(name for name, count in baseline.items() if counts.get(name, 0) < count)
    if grown:
        print("\n".join(grown))
        print(
            "\nwidget code writes the document: keep the edit local and let the owner "
            "submit it as a resource action (TextControl, take_edit_ops, "
            "take_edits/table::apply).",
            file=sys.stderr,
        )
        return 1
    if shed:
        print(
            "widget document writes fell in "
            + ", ".join(shed)
            + "; record it with tools/policy/check_widget_document_writes.py --update",
            file=sys.stderr,
        )
        return 1
    print(f"widget document writes OK ({sum(counts.values())} left, none new)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Widgets never write the document.

A widget in the toolkit (``crates/ui/ui-core/src/widget``) shows the
document and keeps what the user does local; its owner takes the edit and
submits it as a resource action, which is what history attributes, undo
takes back, permissions check and a member replays. A write from widget
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

Every such write fails the check.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import is_test_file, production_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
WIDGETS = (Path("crates/ui/ui-base/src/widget"), Path("crates/ui/ui-core/src/widget"))

WRITES = re.compile(
    r"(?:\b(?:doc|state|document|model)\b(?:\.\w+)*|ctx\.state)\s*"
    r"\.(?:put|put_object|insert_object|splice_text|mark|unmark|delete|increment)\s*\("
)
HELPERS = re.compile(
    r"\bdoc::(?:put_\w+|splice_\w+|put_if_changed)\b|\bdoc_binding::|\bTransactable\b"
)


def violations(root: Path) -> list[str]:
    found: list[str] = []
    for path in sorted(p for widgets in WIDGETS for p in (root / widgets).rglob("*.rs")):
        relative = path.relative_to(root)
        if is_test_file(path.relative_to(root / next(w for w in WIDGETS if path.is_relative_to(root / w)))):
            continue
        text = production_source(path.read_text(encoding="utf-8"))
        for pattern in (WRITES, HELPERS):
            for match in pattern.finditer(text):
                line = text.count("\n", 0, match.start()) + 1
                found.append(f"{relative}:{line}: widget writes the document ({match.group(0).strip()})")
    return found


def main() -> int:
    found = violations(ROOT)
    if found:
        print("\n".join(found))
        print(
            "\nwidget code writes the document: keep the edit local and let the owner "
            "submit it as a resource action (TextControl, take_edit_ops, "
            "take_edits/table::apply).",
            file=sys.stderr,
        )
        return 1
    print("widget document writes OK (no widget writes the document)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

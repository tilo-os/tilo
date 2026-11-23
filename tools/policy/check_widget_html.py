#!/usr/bin/env python3
"""Every widget renders itself as HTML too, next to its paint.

A window shown as a page renders each widget through its own
`Widget::html` (crates/ui/ui-core/src/composition.rs): the element it is
(`<button>`, `<input>`, `<li>`, a heading), what it shows of its own, and
where its children go, so the page has real text, fields, lists and
scrollers rather than a picture of the frame. A widget that is genuinely
drawn (a chart, a canvas, a map) renders as a `<canvas>` of its paint
(`HtmlCx::picture`), and says so in its own `html`.

The trait's default (`HtmlCx::from_layout`) stands in for nothing: this
refuses a production `impl Widget for T` block in `crates/` or `apps/` that
does not define `fn html`, and every widget renders itself.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]

SCOPES = ("crates", "tools", "apps")
IMPL = re.compile(
    r"\bimpl\b(?:\s*<[^{;]*?>)?\s+(?:[A-Za-z_][\w:]*::)?Widget\s+for\s+[^{;]+\{")
HTML = re.compile(r"\bfn\s+html\s*\(")
ADVICE = ("give the widget its own `fn html(&self, cx: &mut HtmlCx)`: the element "
          "it is, what it shows and its children (cx.children()), or "
          "cx.picture() for a widget that is genuinely drawn "
          "(crates/ui/ui-core/src/composition.rs)")


def sources(root: Path) -> list[Path]:
    found = []
    for scope in SCOPES:
        base = root / scope
        if not base.is_dir():
            continue
        for path in base.rglob("*.rs"):
            relative = path.relative_to(root)
            if ("target" in relative.parts
                    or any(part.startswith(".") for part in relative.parts)
                    or rust_source.is_test_file(relative)
                    or "examples" in relative.parts
                    or "tests" in relative.parts):
                continue
            found.append(path)
    return sorted(found)


def count_file(text: str) -> int:
    lexed, _ = rust_source.lex(text)
    code = rust_source.blank_test_items(lexed)
    missing = 0
    for match in IMPL.finditer(code):
        open_index = match.end() - 1
        close = rust_source.matching(code, open_index)
        body = code[open_index:close]
        # An impl written in a `macro_rules!` body takes its items from the
        # invocation (`$impl_items`): there is no widget here, only the item
        # list the expansion fills, and the invocations give it `html`.
        if "$" in body:
            continue
        # Only the impl's own items: a nested impl's `fn html` is not this
        # widget's.
        depth, own = 0, []
        for char in body:
            if char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
            own.append(char if depth <= 1 else " ")
        if not HTML.search("".join(own)):
            missing += 1
    return missing


def scan(root: Path) -> dict[str, int]:
    found: dict[str, int] = {}
    for path in sources(root):
        count = count_file(path.read_text(errors="replace"))
        if count:
            found[path.relative_to(root).as_posix()] = count
    return found


def main() -> int:
    found = scan(ROOT)
    if found:
        for file, count in sorted(found.items()):
            print(f"{file}: {count} widget impls without html; {ADVICE}")
        return 1
    print("widget html OK (every widget renders itself)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

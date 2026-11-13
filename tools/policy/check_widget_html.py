#!/usr/bin/env python3
"""Every widget renders itself as HTML too, next to its paint.

A window shown as a page renders each widget through its own
`Widget::html` (crates/sdk/ui-core/src/composition.rs): the element it is
(`<button>`, `<input>`, `<li>`, a heading), what it shows of its own, and
where its children go, so the page has real text, fields, lists and
scrollers rather than a picture of the frame. A widget that is genuinely
drawn (a chart, a canvas, a map) renders as a `<canvas>` of its paint
(`HtmlCx::picture`), and says so in its own `html`.

The trait's default (`HtmlCx::from_layout`) only stands in for widgets not yet
given their own rendering. This refuses a production `impl Widget for T`
block in `crates/` or `apps/` that does not define `fn html`. The ones still
on the default are listed in `widget_html_baseline.txt`, one line per file
with how many such impls it holds. A count only shrinks: more than listed
fails, fewer fails until the line is lowered, so a widget given its HTML
cannot lose it and a new widget cannot skip it.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).resolve().parent / "widget_html_baseline.txt"

SCOPES = ("crates", "apps")
RULE = "widget-without-html"
IMPL = re.compile(
    r"\bimpl\b(?:\s*<[^{;]*?>)?\s+(?:[A-Za-z_][\w:]*::)?Widget\s+for\s+[^{;]+\{")
HTML = re.compile(r"\bfn\s+html\s*\(")
ADVICE = ("give the widget its own `fn html(&self, cx: &mut HtmlCx)`: the element "
          "it is, what it shows and its children (cx.children()), or "
          "cx.picture() for a widget that is genuinely drawn "
          "(crates/sdk/ui-core/src/composition.rs)")


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


def load_baseline(path: Path) -> dict[str, int]:
    allowed: dict[str, int] = {}
    if not path.exists():
        return allowed
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        rule, count, file = line.split(maxsplit=2)
        if rule != RULE:
            raise ValueError(f"{path.name}: unknown rule {rule!r}")
        allowed[file] = int(count)
    return allowed


def check(root: Path, baseline_path: Path) -> list[str]:
    found = scan(root)
    allowed = load_baseline(baseline_path)
    listed = (baseline_path.relative_to(root).as_posix()
              if baseline_path.is_relative_to(root) else baseline_path.name)
    errors = []
    for file in sorted(set(found) | set(allowed)):
        have, may = found.get(file, 0), allowed.get(file, 0)
        if have > may:
            errors.append(f"{file}: {have} {RULE} (allowed {may}); {ADVICE}")
        elif have < may:
            errors.append(f"{listed}: {RULE} {may} {file} is now {have}; "
                          "lower the count (or delete the line at 0)")
    return errors


def print_baseline(root: Path) -> None:
    print("# Widgets that still render as HTML through the toolkit's default")
    print("# (tools/policy/check_widget_html.py). Counts only shrink.")
    for file, count in sorted(scan(root).items()):
        print(f"{RULE} {count} {file}")


def main(argv: list[str]) -> int:
    if argv[1:] == ["--print-baseline"]:
        print_baseline(ROOT)
        return 0
    errors = check(ROOT, BASELINE)
    if errors:
        print("\n".join(errors))
        return 1
    found = scan(ROOT)
    print(f"widget html OK ({sum(found.values())} widget impls still on the default "
          f"in {len(found)} files; none new)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

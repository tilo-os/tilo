#!/usr/bin/env python3
"""Apps draw no search field of their own.

The only search entry in a window is the window bar's: typing into it turns
the window's view into a query view of the app's resources, narrows a list or
finds in a document (docs/policies.md, "Search"; the framework's
``tilo_app::framework::views`` and the toolkit's ``WindowSearch``). An app
that draws its own field (a full-width "Search apps" box, a "Search settings"
field above a sidebar, a list's own filter box, an in-app command palette)
gives the window two searches that disagree.

This check reads the production sources of every app under ``apps/`` and
refuses what draws a search or filter field:

* ``SearchField::new``, ``SearchField::filter`` and ``Filtered::new``;
* a text input of ``TextInputKind::Search``;
* a navigation list or section made ``.filterable()``, which draws its filter
  field;
* an in-app palette given a query placeholder (``set_query_placeholder``);
* any field whose placeholder asks to search ("Search cities", a
  ``…-search-placeholder`` message).

Apps that still draw one are listed with their count in
``app_search_fields_baseline.toml``, which only shrinks: a file not listed, or
more fields than listed, fails; a listed file with fewer fails until its line
is lowered (``--write-baseline`` writes the counts as they are).
"""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import is_test_file, line_number, production_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path("tools/policy/app_search_fields_baseline.toml")

PATTERNS = (
    (re.compile(r"\bSearchField::(?:new|filter)\s*\("), "draws a search field"),
    (re.compile(r"\bFiltered::new\s*\("), "draws a filter field over its content"),
    (re.compile(r"\bTextInputKind::Search\b"), "draws a search text input"),
    (re.compile(r"\.filterable\s*\(\s*\)"), "draws a list's own filter field"),
    (re.compile(r"\.set_query_placeholder\s*\("), "draws an in-app palette's query field"),
    (
        re.compile(r"\.with_placeholder\s*\([^;]*?(?i:\bsearch|-search\b|search-)", re.DOTALL),
        "draws a field that asks to search",
    ),
)


@dataclass(frozen=True)
class Finding:
    path: str
    line: int
    reason: str

    def __str__(self) -> str:
        return (
            f"{self.path}:{self.line}: {self.reason}; the window bar's search is the "
            "only search entry (a query view, a narrowed list or a find)"
        )


def findings(root: Path) -> list[Finding]:
    found: list[Finding] = []
    apps = root / "apps"
    if not apps.is_dir():
        return found
    for path in sorted(apps.rglob("*.rs")):
        relative = path.relative_to(root)
        parts = relative.parts
        if len(parts) < 3 or "src" not in parts or is_test_file(relative):
            continue
        text = production_source(path.read_text(encoding="utf-8"))
        for pattern, reason in PATTERNS:
            for match in pattern.finditer(text):
                found.append(Finding(str(relative), line_number(text, match.start()), reason))
    return found


def counts(found: list[Finding]) -> dict[str, int]:
    out: dict[str, int] = {}
    for finding in found:
        out[finding.path] = out.get(finding.path, 0) + 1
    return out


def read_baseline(root: Path) -> dict[str, int]:
    path = root / BASELINE
    if not path.exists():
        return {}
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    return {str(key): int(value) for key, value in data.get("files", {}).items()}


def write_baseline(root: Path, current: dict[str, int]) -> None:
    lines = [
        "# Apps that still draw a search field of their own, and how many.",
        "# Written by `python3 tools/policy/check_app_search_fields.py --write-baseline`.",
        "# It only shrinks: move each onto the window bar's search, then lower its line.",
        "",
        "[files]",
    ]
    lines += [f'"{path}" = {count}' for path, count in sorted(current.items())]
    (root / BASELINE).write_text("\n".join(lines) + "\n", encoding="utf-8")


def audit(root: Path) -> list[str]:
    found = findings(root)
    current = counts(found)
    baseline = read_baseline(root)
    problems: list[str] = []
    for path, count in sorted(current.items()):
        allowed = baseline.get(path, 0)
        if count > allowed:
            problems.extend(str(finding) for finding in found if finding.path == path)
            if allowed:
                problems.append(f"{path}: {count} search fields, the baseline allows {allowed}")
    for path, allowed in sorted(baseline.items()):
        count = current.get(path, 0)
        if count < allowed:
            problems.append(
                f"{BASELINE}: {path} draws {count} search fields now, not {allowed}; "
                "lower its line (--write-baseline)"
            )
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--write-baseline", action="store_true")
    args = parser.parse_args(argv)
    if args.write_baseline:
        write_baseline(args.root, counts(findings(args.root)))
        return 0
    problems = audit(args.root)
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())

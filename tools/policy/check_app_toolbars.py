#!/usr/bin/env python3
"""An app's primary controls live in the window bar's app slots, never in a
toolbar row the app draws under it (docs/design-language.md; the window bar's
slots are `ui::window_toolbar`).

An app declares its sidebar toggle, navigation, location, view switcher,
actions and menus through `ui::window_toolbar::declare` (a `NavShell` app
through `NavShell::with_window_toolbar`), and the desktop's window bar shows
them; outside a session the native runner draws the same slots. A second row
under the bar is what this refuses, in production Rust under `apps/`:

- window-header: `impl WindowHeader for ...`, an app hosting the window's
  controls (History, search) in a header row it paints;
- history-in-row: the window's History control placed in an app's row
  (`HistoryOpener`, `header_with_history`, `with_history_if`);
- header-row: a header row painted by hand (`HeaderRowFace::new`);
- toolbar-row: a horizontal toolbar (`Toolbar::new(Orientation::Horizontal)`);
- toolbar-band: the band behind a toolbar row painted by the app
  (`toolbar::paint_band`), the row of an app that lays out its own controls
  in it (Books' shelf header: sort, Hide finished, Settings, Open book);
- nav-header-actions: a `NavShell` header row given actions
  (`set_content_header`, `set_overview_header`, `set_sidebar_header` with
  anything but an empty list);
- nav-header-leading: a `NavShell` column header given a leading widget
  (`content_header_leading`), which the bar has no slot for, so the shell
  keeps its own header row;
- own-back: a subpage header with its own Back (`subpage_header`);
- own-search: a search or find entry of the app's own, where the window bar's
  search is the only one (a query view of the app's resources, a narrowed
  list, a find in the document): `SearchField::new`/`filter`, `Filtered::new`,
  `TextInputKind::Search`, `set_query_placeholder`, `search_button`,
  `FindBar::new`, `FindScope::new`, `CommandPalette::commands`, and a
  `with_placeholder`/`set_placeholder` call whose own arguments say search,
  filter or find: a literal, or a Fluent key named so or whose English
  message says so.

A line may be exempt, with its reason, in a comment after its code, or a
statement in a comment on its own line before it: `// window-bar: <reason>`. The reasons are a closed set
(`EXEMPT`); any other fails.

Code that still does any of this is listed in `app_toolbars_baseline.txt`,
one line per file and rule with how many it may still have. A count only
shrinks: more than listed fails, fewer fails until the line is lowered, so a
migrated app cannot grow its row back.
"""

from __future__ import annotations

import re
import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).resolve().parent / "app_toolbars_baseline.txt"

SCOPES = ("apps",)

RULES = (
    "window-header",
    "history-in-row",
    "header-row",
    "toolbar-row",
    "toolbar-band",
    "nav-header-actions",
    "nav-header-leading",
    "own-back",
    "own-search",
)

# Why a line may keep a control of its own: the reasons
# `// window-bar: <reason>` accepts (docs/policies.md, "Exceptions").
EXEMPT = {
    "picker": "type-ahead inside a popover or dialog (emoji, keyboard layout, Open with, a city lookup)",
    "form": "a field that enters data",
    "service": "no window bar exists (the on-screen keyboard)",
    "fullscreen": "presenting or playing back with the window's chrome hidden",
    "fallback": "drawn only when no window bar hosts the window",
    "wizard": "step navigation inside a flow",
}
EXEMPTION = re.compile(r"//\s*window-bar:\s*([A-Za-z-]*)")

PATTERNS = {
    "window-header": re.compile(r"\bimpl\b[^{;]*\bWindowHeader\s+for\b"),
    "history-in-row": re.compile(r"\bHistoryOpener\b|\.header_with_history\s*\(|\.with_history_if\s*\("),
    "header-row": re.compile(r"\bHeaderRowFace::new\s*\("),
    "toolbar-row": re.compile(
        r"\bToolbar::new\s*\(\s*(?:[A-Za-z_][A-Za-z0-9_]*::)*Orientation::Horizontal\s*\)"
    ),
    "toolbar-band": re.compile(r"\btoolbar::paint_band\s*\("),
    "nav-header-leading": re.compile(r"\b(?:set_)?content_header_leading\s*\(\s*(?!None\s*\))"),
    "own-back": re.compile(r"(?<!fn )\bsubpage_header\s*\("),
    "own-search": re.compile(
        r"\bSearchField::(?:new|filter)\s*\(|\bFiltered::new\s*\(|\bTextInputKind::Search\b"
        r"|\.set_query_placeholder\s*\(|(?<!fn )\bsearch_button\s*\(|\bFindBar::new\s*\("
        r"|\bFindScope::new\s*\(|\bCommandPalette::commands\s*\("
    ),
}
PLACEHOLDER = re.compile(r"\.(?:with|set)_placeholder\s*\(")
ASKS_TO_SEARCH = re.compile(r"(?i)(?:\b|_)(?:search|filter|find)")
FLUENT_KEY = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)+")
FLUENT_MESSAGE = re.compile(r"(?m)^([a-z0-9][a-z0-9-]*)\s*=\s*(.*)$")
ROW_ADVICE = ("put the app's controls in the window bar's slots (ui::window_toolbar::declare, "
              "or NavShell::with_window_toolbar) instead of drawing a toolbar row")
ADVICE = {
    "own-search": "the window bar's search is the only search entry: offer the app's resources "
                  "to it (a query view), make the list Filterable, or make the document Findable",
    "own-back": "Back is the window bar's navigation slot (a NavShell in Push mode hands it over)",
    "nav-header-leading": "give the bar what the leading widget shows (its location, views or "
                          "actions), so the shell hands its header to the bar",
}
NAV_HEADER = re.compile(r"\.set_(?:content|overview|sidebar)_header\s*\(")
EMPTY_LIST = re.compile(r",\s*(?:Vec::new\(\)|vec!\[\s*\])\s*,?\s*$")


def sources(root: Path) -> list[Path]:
    found = []
    for scope in SCOPES:
        base = root / scope
        if not base.is_dir():
            continue
        for path in base.rglob("*.rs"):
            relative = path.relative_to(root)
            if ("target" in relative.parts or "src" not in relative.parts
                    or any(part.startswith(".") for part in relative.parts)
                    or rust_source.is_test_file(relative)
                    or "examples" in relative.parts):
                continue
            found.append(path)
    return sorted(found)


def messages(root: Path) -> dict[str, str]:
    """The English Fluent messages of every app, by key."""
    found: dict[str, str] = {}
    for catalog in sorted(root.glob("apps/*/locales/en-US.ftl")):
        for key, value in FLUENT_MESSAGE.findall(catalog.read_text(errors="replace")):
            found.setdefault(key, value)
    return found


def exemptions(text: str) -> tuple[set[int], list[tuple[int, str]]]:
    """The lines a `// window-bar: <reason>` comment exempts, and the
    comments naming a reason that is not one of `EXEMPT`, by line. A comment
    after code exempts its line; a comment on a line of its own exempts the
    statement after it, through the line that ends it (`;`, `,`, `{`, `}`)."""
    exempt: set[int] = set()
    unknown: list[tuple[int, str]] = []
    lines = text.splitlines()
    for index, line in enumerate(lines):
        found = EXEMPTION.search(line)
        if not found:
            continue
        if found.group(1) not in EXEMPT:
            unknown.append((index + 1, found.group(1)))
            continue
        exempt.add(index + 1)
        if line[:found.start()].strip():
            continue
        for after in range(index + 1, len(lines)):
            exempt.add(after + 1)
            code = lines[after].split("//", 1)[0].rstrip()
            if code.endswith((";", ",", "{", "}")):
                break
    return exempt, unknown


def asks_to_search(code: str, literals: list, start: int, close: int,
                   catalog: dict[str, str]) -> bool:
    """Whether the call whose arguments span `start..close` is given a
    placeholder that asks to search: a literal saying so, or a Fluent key
    named so or whose English message says so."""
    for literal in literals:
        if literal.start < start or literal.end > close:
            continue
        said = [literal.value]
        if FLUENT_KEY.fullmatch(literal.value):
            said.append(catalog.get(literal.value, ""))
        if any(ASKS_TO_SEARCH.search(value) for value in said):
            return True
    return False


def count_file(text: str, catalog: dict[str, str] | None = None) -> Counter:
    counts: Counter = Counter()
    lexed, literals = rust_source.lex(text)
    code = rust_source.blank_test_items(lexed)
    exempt, _ = exemptions(text)

    def counted(offset: int) -> bool:
        return rust_source.line_number(text, offset) not in exempt

    searches: set[int] = set()
    for rule, pattern in PATTERNS.items():
        for match in pattern.finditer(code):
            if not counted(match.start()):
                continue
            if rule == "own-search":
                # A field and its placeholder are one search entry.
                searches.add(rust_source.line_number(text, match.start()))
            else:
                counts[rule] += 1
    for match in NAV_HEADER.finditer(code):
        close = rust_source.matching(code, match.end() - 1)
        arguments = code[match.end():close - 1] if close > match.end() else ""
        if not EMPTY_LIST.search(arguments) and counted(match.start()):
            counts["nav-header-actions"] += 1
    for match in PLACEHOLDER.finditer(code):
        close = rust_source.matching(code, match.end() - 1)
        # A search field's own placeholder is that field, counted above.
        begins = max(code.rfind(edge, 0, match.start()) for edge in ";{},") + 1
        if PATTERNS["own-search"].search(code, begins, match.start()):
            continue
        if counted(match.start()) and asks_to_search(code, literals, match.end(), close,
                                                     catalog or {}):
            searches.add(rust_source.line_number(text, match.start()))
    counts["own-search"] = len(searches)
    return counts


def scan(root: Path) -> dict[tuple[str, str], int]:
    found: dict[tuple[str, str], int] = {}
    catalog = messages(root)
    for path in sources(root):
        counts = count_file(path.read_text(errors="replace"), catalog)
        relative = path.relative_to(root).as_posix()
        for rule, count in counts.items():
            if count:
                found[(relative, rule)] = count
    return found


def load_baseline(path: Path) -> dict[tuple[str, str], int]:
    allowed: dict[tuple[str, str], int] = {}
    if not path.exists():
        return allowed
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        rule, count, file = line.split(maxsplit=2)
        if rule not in RULES:
            raise ValueError(f"{path.name}: unknown rule {rule!r}")
        allowed[(file, rule)] = int(count)
    return allowed


def unknown_exemptions(root: Path) -> list[str]:
    errors = []
    for path in sources(root):
        _, unknown = exemptions(path.read_text(errors="replace"))
        relative = path.relative_to(root).as_posix()
        errors.extend(
            f"{relative}:{line}: window-bar exemption {reason!r} is not one of "
            f"{', '.join(sorted(EXEMPT))} (docs/policies.md, \"Exceptions\")"
            for line, reason in unknown
        )
    return errors


def check(root: Path, baseline_path: Path) -> list[str]:
    found = scan(root)
    allowed = load_baseline(baseline_path)
    errors = unknown_exemptions(root)
    # The baseline's findings name it from the top of the tree.
    listed = (baseline_path.relative_to(root).as_posix()
              if baseline_path.is_relative_to(root) else baseline_path.name)
    for key in sorted(set(found) | set(allowed)):
        file, rule = key
        have, may = found.get(key, 0), allowed.get(key, 0)
        if have > may:
            errors.append(f"{file}: {have} {rule} (allowed {may}); {ADVICE.get(rule, ROW_ADVICE)}")
        elif have < may:
            errors.append(f"{listed}: {rule} {may} {file} is now {have}; "
                          "lower the count (or delete the line at 0)")
    return errors


def print_baseline(root: Path) -> None:
    print("# Apps that still draw a toolbar row of their own under the window bar")
    print("# (tools/policy/check_app_toolbars.py). Counts only shrink.")
    for (file, rule), count in sorted(scan(root).items(), key=lambda kv: (kv[0][1], kv[0][0])):
        print(f"{rule} {count} {file}")


def main(argv: list[str]) -> int:
    if argv[1:] == ["--print-baseline"]:
        print_baseline(ROOT)
        return 0
    errors = check(ROOT, BASELINE)
    if errors:
        print("\n".join(errors))
        return 1
    print("app toolbars OK (an app's controls are its window bar's slots)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))

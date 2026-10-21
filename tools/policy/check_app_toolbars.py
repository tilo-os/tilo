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
- nav-header-actions: a `NavShell` header row given actions
  (`set_content_header`, `set_overview_header`, `set_sidebar_header` with
  anything but an empty list).

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

RULES = ("window-header", "history-in-row", "header-row", "toolbar-row", "nav-header-actions")

PATTERNS = {
    "window-header": re.compile(r"\bimpl\b[^{;]*\bWindowHeader\s+for\b"),
    "history-in-row": re.compile(r"\bHistoryOpener\b|\.header_with_history\s*\(|\.with_history_if\s*\("),
    "header-row": re.compile(r"\bHeaderRowFace::new\s*\("),
    "toolbar-row": re.compile(
        r"\bToolbar::new\s*\(\s*(?:[A-Za-z_][A-Za-z0-9_]*::)*Orientation::Horizontal\s*\)"
    ),
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


def count_file(text: str) -> Counter:
    counts: Counter = Counter()
    code = rust_source.blank_test_items(rust_source.code_only(text))
    for rule, pattern in PATTERNS.items():
        counts[rule] += len(pattern.findall(code))
    for match in NAV_HEADER.finditer(code):
        close = rust_source.matching(code, match.end() - 1)
        arguments = code[match.end():close - 1] if close > match.end() else ""
        if not EMPTY_LIST.search(arguments):
            counts["nav-header-actions"] += 1
    return counts


def scan(root: Path) -> dict[tuple[str, str], int]:
    found: dict[tuple[str, str], int] = {}
    for path in sources(root):
        counts = count_file(path.read_text(errors="replace"))
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


def check(root: Path, baseline_path: Path) -> list[str]:
    found = scan(root)
    allowed = load_baseline(baseline_path)
    errors = []
    for key in sorted(set(found) | set(allowed)):
        file, rule = key
        have, may = found.get(key, 0), allowed.get(key, 0)
        if have > may:
            errors.append(f"{file}: {have} {rule} (allowed {may}); put the app's controls in "
                          "the window bar's slots (ui::window_toolbar::declare, or "
                          "NavShell::with_window_toolbar) instead of drawing a toolbar row")
        elif have < may:
            errors.append(f"{baseline_path.name}: {rule} {may} {file} is now {have}; "
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
    print("app toolbars OK (an app's controls are its window bar's slots)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))

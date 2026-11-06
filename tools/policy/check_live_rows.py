#!/usr/bin/env python3
"""A live row is an item resource of a declared type, never an untyped row
with an app effect a click runs (docs/designs/app-language/one-app-model.md
§1, "Types are resources").

A row a surface publishes beside its resource — a process, a container, a
fetched catalogue's channel or video — used to be `LiveRow::new(id, kind,
label)` with a tag and app-internal effects. Search could not tell a channel
from a video, and an agent had no type's actions to take on it. Now the row
is `LiveRow::typed::<T>`: an item resource of a declared `[#resource]` type,
carrying that type's words (its noun, its app's name) so the one search names
it exactly as it names a tilolang item, and what a press does is one of the
type's actions or `open`.

This refuses `LiveRow::new` in production Rust under `apps/`. The apps still
on it are listed in `live_rows_baseline.txt`, one line per file with how many
untyped rows it still makes. A count only shrinks: more than listed fails,
fewer fails until the line is lowered, so a migrated app cannot fall back and
the list reaches empty as each app moves to typed rows.
"""

from __future__ import annotations

import re
import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).resolve().parent / "live_rows_baseline.txt"

SCOPES = ("apps",)
RULE = "untyped-live-row"
PATTERN = re.compile(r"\bLiveRow::new\s*\(")
ADVICE = ("publish the row as an item of a declared #[resource] type "
          "(LiveRow::typed::<Channel>), so search names it by what it is and a "
          "press runs the type's action or open (one-app-model.md §1)")


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


def count_file(text: str) -> int:
    lexed, _ = rust_source.lex(text)
    code = rust_source.blank_test_items(lexed)
    return len(PATTERN.findall(code))


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
    print("# Apps that still publish untyped live rows (LiveRow::new)")
    print("# (tools/policy/check_live_rows.py). Counts only shrink.")
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
    print("live rows OK (a row is an item of a declared type)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

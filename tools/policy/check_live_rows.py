#!/usr/bin/env python3
"""A row is an item of a declared type, and its actions and effects are
declared, never hand-built (docs/designs/app-language/one-app-model.md §1,
"Types are resources").

What an app publishes beneath its resource — a live row (a process, a
container, a fetched catalogue's channel), or the resource's own tree — used
to be built by hand: `LiveRow::new(id, kind, label)` with a tag and effects a
click runs, `ResourceSpec::new(...)` with actions pushed onto it, and
`ActionSpec::typed(...)` naming a bound action nobody declared. Search could
not tell a channel from a video, and an agent had no type's actions to take
on an entity.

Now a row is `LiveRow::typed::<T>`: an item resource of a declared
`#[resource]` or `#[derive(Row)]` type, carrying that type's words (its noun,
its app's name) and the type's declared effects (`#[effect(item = …)]`,
`#[effect(on = …)]`), each bound to the entity. The tree comes from the
declarations (`ResourceSpec::new` in the framework, `ActionSpec::typed` in
`tilo-actions`), not from an app.

This refuses, in production Rust under `apps/` and `crates/session/`:

* `untyped-live-row`: `LiveRow::new`, a row that names no declared type;
* `row-operation`: `.operation(...)` or `.effect(...)` chained onto a
  `LiveRow` construction (including `LiveRow::typed::<…>`) or added to a row
  a variable holds, whether the variable is the construction itself or what a
  helper that returns a row built (`item.kind.row(…)`, a `…_row` function) —
  an effect offered on a row the app shaped rather than one its type
  declares;
* `hand-built-tree`: `ResourceSpec::new`, a resource tree an app built;
* `hand-bound-action`: `ActionSpec::typed`, an action an app bound itself.

The files still on them are listed in `live_rows_baseline.txt`, one line per
rule and file with how many it still has. A count only shrinks: more than
listed fails, fewer fails until the line is lowered, so a migrated app cannot
fall back and the list reaches empty as each app moves to declared types.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).resolve().parent / "live_rows_baseline.txt"

SCOPES = ("apps", "crates/session")

RULES = {
    "untyped-live-row": re.compile(r"\bLiveRow::new\s*\("),
    "hand-built-tree": re.compile(r"\bResourceSpec::new\s*\("),
    "hand-bound-action": re.compile(r"\bActionSpec::typed"),
}

# Every way a row is built: untyped, of a declared type (with the turbofish
# that spells it: `LiveRow::typed::<Widget>(…)`), with the state its type's
# effects bind, or from an entity's own resource.
ROW = re.compile(
    r"\bLiveRow\s*::\s*(?:new|typed|typed_with|from_resource)"
    r"\s*(?:::?<[^()]*>)?\s*\("
)
ROW_METHODS = {"operation", "effect"}
# One `let name = <initializer>;` binding, initializer and all. Read as a
# row's variable when the initializer builds one: `LiveRow::…` itself, or a
# helper that returns a row (a function or method named `row` or `…_row`,
# `item.kind.row(…)`), which the chain above cannot follow to its definition.
LET_BINDING = re.compile(r"\blet\s+(?:mut\s+)?(\w+)\s*(?::[^=;]+)?=([^;]*?);")
ROW_BUILDER = re.compile(
    r"\bLiveRow\s*::|(?:\b|\.)(?:\w*_row|row)\s*\("
)

ADVICE = {
    "untyped-live-row": (
        "publish the row as an item of a declared type "
        "(LiveRow::typed::<Widget>, #[derive(Row)]), so search names it by "
        "what it is and a press runs the type's effect (one-app-model.md §1)"
    ),
    "row-operation": (
        "declare the effect on the type (#[effect(on = Row)]) and let "
        "LiveRow::typed publish it bound, instead of chaining it onto the row"
    ),
    "hand-built-tree": (
        "publish the tree from the type's declaration (Resource::children, "
        "#[derive(Row)]), not by building a ResourceSpec"
    ),
    "hand-bound-action": (
        "declare the action or effect and bind it (Effect::bound, "
        "#[action(item = field)]), not by building an ActionSpec"
    ),
}


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


def row_operations(code: str) -> int:
    """`.operation(...)` and `.effect(...)` chained onto a row construction,
    which declares nothing: the row's type should offer them itself."""
    count = 0
    for match in ROW.finditer(code):
        index = rust_source.matching(code, match.end() - 1)
        while True:
            while index < len(code) and code[index] in " \t\r\n":
                index += 1
            if index >= len(code) or code[index] != ".":
                break
            index += 1
            while index < len(code) and code[index] in " \t\r\n":
                index += 1
            start = index
            while index < len(code) and (code[index].isalnum() or code[index] == "_"):
                index += 1
            name = code[start:index]
            while index < len(code) and code[index] in " \t\r\n":
                index += 1
            if name in ROW_METHODS:
                count += 1
            if index < len(code) and code[index] == "(":
                index = rust_source.matching(code, index)
    rows = {
        match.group(1)
        for match in LET_BINDING.finditer(code)
        if ROW_BUILDER.search(match.group(2))
    }
    for ident in rows:
        count += len(re.findall(
            rf"\b{re.escape(ident)}\s*\.\s*(?:{'|'.join(sorted(ROW_METHODS))})\s*\(",
            code,
        ))
    return count


def count_file(text: str) -> dict[str, int]:
    lexed, _ = rust_source.lex(text)
    code = rust_source.blank_test_items(lexed)
    found = {rule: len(pattern.findall(code)) for rule, pattern in RULES.items()}
    found["row-operation"] = row_operations(code)
    return {rule: count for rule, count in found.items() if count}


def scan(root: Path) -> dict[tuple[str, str], int]:
    found: dict[tuple[str, str], int] = {}
    for path in sources(root):
        for rule, count in count_file(path.read_text(errors="replace")).items():
            found[(rule, path.relative_to(root).as_posix())] = count
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
        if rule not in RULES and rule != "row-operation":
            raise ValueError(f"{path.name}: unknown rule {rule!r}")
        allowed[(rule, file)] = int(count)
    return allowed


def check(root: Path, baseline_path: Path) -> list[str]:
    found = scan(root)
    allowed = load_baseline(baseline_path)
    errors = []
    for rule, file in sorted(set(found) | set(allowed)):
        have, may = found.get((rule, file), 0), allowed.get((rule, file), 0)
        if have > may:
            errors.append(f"{file}: {have} {rule} (allowed {may}); {ADVICE[rule]}")
        elif have < may:
            errors.append(f"{baseline_path.relative_to(root).as_posix()}: "
                          f"{rule} {may} {file} is now {have}; "
                          "lower the count (or delete the line at 0)")
    return errors


def print_baseline(root: Path) -> None:
    print("# Rows and trees apps still build by hand")
    print("# (tools/policy/check_live_rows.py). Counts only shrink.")
    for (rule, file), count in sorted(scan(root).items()):
        print(f"{rule} {count} {file}")


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

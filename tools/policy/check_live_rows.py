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

This refuses, in production Rust of every crate but the framework's own
(`FRAMEWORK` below), every occurrence:

* `untyped-live-row`: `LiveRow::new`, a row that names no declared type;
* `row-operation`: `.operation(...)` or `.effect(...)` chained onto a
  `LiveRow` construction (including `LiveRow::typed::<…>`), added to a row
  a variable holds, whether the variable is the construction itself or what a
  helper that returns a row built (`item.kind.row(…)`, a `…_row` function),
  or added to a row the file was handed: a parameter, closure parameter or
  spelled-out binding typed `LiveRow` — an effect offered on a row the app
  shaped rather than one its type declares;
* `hand-built-tree`: `ResourceSpec::new`, a resource tree an app built;
* `hand-bound-action`: `ActionSpec::typed`, an action an app bound itself.

There is no baseline: the migration is done, so the old builders are
framework-internal (`#[doc(hidden)]`) and an app or an SDK crate that reaches
for one fails here rather than silently publishing an untyped row. Test code
is not the app's publication and is not counted.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]

# The framework's own crates build the specs and trees a declaration
# publishes: the declaration machinery and the toolkit's generated controls,
# the session and window runtimes, the publisher, the agent and browser
# services that place what an app declared, and the device daemon's control
# provider, which publishes the steps its step program declares as data.
# Every other crate — an app, and the SDK crates it builds its surfaces with —
# declares.
FRAMEWORK = (
    "crates/agent/agent-session",
    "crates/browser/tilo-browser-runtime",
    "crates/collab/actions",
    "crates/collab/automation",
    "crates/collab/collaboration",
    "crates/lang/tilolang",
    "crates/platform/tilo-app-window",
    "crates/platform/tilolang-tilo",
    "crates/runtime/tilo-runtime/src/device/control",
    "crates/sdk/tilo-app",
    "crates/sdk/tilo-resource",
    "crates/sdk/ui-core",
)
SCOPES = ("apps", "crates", "tools")

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
# A call that answers a row, chained with an operation before anything holds
# it: `provider_row(profile).operation(spec)`, `item.kind.row(id).effect(…)`.
ROW_CALL = re.compile(r"(?:\b|\.)(?:\w*_row|row)\s*\(")
# A name the file types as a row it was handed rather than built: a
# parameter (`fn patch(row: LiveRow)`), a closure's parameter (`|row:
# LiveRow|`), or a binding whose type is spelled (`let row: LiveRow = …`).
# Only the type says it is a row, so no builder in the file leads to it.
LIVE_ROW_TYPE = re.compile(
    r"\b(\w+)\s*:\s*&?\s*(?:'\w+\s*)?(?:\w+\s*::\s*)*LiveRow\b"
)
FN_SIGNATURE = re.compile(r"\bfn\s+[A-Za-z_]\w*\s*(?:<[^(){};]*>)?\s*\(")
CLOSURE_PARAMS = re.compile(r"\|([^|\n]*)\|")
TYPED_BINDING = re.compile(
    r"\blet\s+(?:mut\s+)?(\w+)\s*:\s*&?\s*(?:'\w+\s*)?(?:\w+\s*::\s*)*LiveRow\b"
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
                    or "examples" in relative.parts
                    or any(relative.as_posix().startswith(f"{crate}/")
                           for crate in FRAMEWORK)):
                continue
            found.append(path)
    return sorted(found)


def chained_operations(code: str, open_paren: int) -> int:
    """The `.operation(…)` and `.effect(…)` of the dot chain that follows the
    call whose argument list opens at `open_paren`."""
    count = 0
    index = rust_source.matching(code, open_paren)
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
    return count


def row_names(code: str) -> set[str]:
    """The names the file itself types as a row it was handed: a function's
    parameter, a closure's parameter, or a binding whose type is spelled."""
    names = {match.group(1) for match in TYPED_BINDING.finditer(code)}
    for signature in FN_SIGNATURE.finditer(code):
        close = rust_source.matching(code, signature.end() - 1)
        names.update(
            match.group(1)
            for match in LIVE_ROW_TYPE.finditer(code[signature.end() : close])
        )
    for closure in CLOSURE_PARAMS.finditer(code):
        names.update(
            match.group(1) for match in LIVE_ROW_TYPE.finditer(closure.group(1))
        )
    return names


def row_operations(code: str) -> int:
    """`.operation(...)` and `.effect(...)` chained onto a row construction,
    which declares nothing: the row's type should offer them itself. A row
    the file was handed (a parameter or a spelled-out binding) counts too,
    as does a helper call chained before anything holds it."""
    count = sum(
        chained_operations(code, match.end() - 1) for match in ROW.finditer(code)
    )
    count += sum(
        chained_operations(code, match.end() - 1) for match in ROW_CALL.finditer(code)
    )
    rows = {
        match.group(1)
        for match in LET_BINDING.finditer(code)
        if ROW_BUILDER.search(match.group(2))
    } | row_names(code)
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


def check(root: Path) -> list[str]:
    found = scan(root)
    return [
        f"{file}: {count} {rule}; {ADVICE[rule]}"
        for (rule, file), count in sorted(found.items())
    ]


def main() -> int:
    errors = check(ROOT)
    if errors:
        print("\n".join(errors))
        return 1
    print("live rows OK (a row is an item of a declared type)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

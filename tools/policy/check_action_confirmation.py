#!/usr/bin/env python3
"""Typed confirmation is for what Undo cannot take back, and names it.

Undo and History are system services: an action Undo takes back runs at once
and offers Undo, with no confirmation at all. A typed confirmation
(`ConfirmationPolicy::Phrase`) is reserved for irreversible, high-risk actions
(erasing a drive, deleting an app's data), and the phrase names what is lost,
so typing it means the person read it. This check refuses, in production Rust:

- a typed phrase on an action declared undoable: in the same builder chain or
  contract literal as `.reversible()`, `reversible: true`, a compensated undo
  or `ActionKind::Undoable`;
- a typed phrase that is a generic word ("CONFIRM", "YES", "DELETE", ...),
  which says nothing about what is lost;
- a `"CONFIRM"` literal anywhere: no person is ever asked to type it;
- a destructive action that runs at once with Undo (`#[action(destructive)]`
  without `irreversible`) and is not listed, with why Undo takes it back in
  full, in tools/policy/undoable-deletions.toml. One that reaches outside
  the record (a server, a device, an inbox, what others saw) is marked
  `irreversible` and asked first instead.

`ActionContract::validate_declaration` refuses the same at runtime; this check
catches it before anything runs.
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import is_test_file, line_number, production_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
TREES = ("apps", "crates", "examples")
LEDGER = ROOT / "tools/policy/undoable-deletions.toml"

ATTRIBUTE = re.compile(r"#\[\s*(action|effect)\s*\(([^\]]*)\)\s*\]")
WORD = re.compile(r"[A-Za-z_]+")
ITEM = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)|^\s*([A-Z][A-Za-z0-9_]*)\b", re.M)

# Kept in step with `GENERIC_CONFIRMATION_WORDS` in
# crates/collab/actions/src/contract.rs.
GENERIC_WORDS = {
    "confirm", "confirmed", "yes", "ok", "okay", "sure", "agree", "i agree", "continue",
    "proceed", "accept", "delete", "remove", "erase", "wipe", "reset", "destroy",
}

PHRASE = re.compile(r"ConfirmationPolicy\s*::\s*Phrase\s*\{")
PHRASE_LITERAL = re.compile(r'phrase\s*:\s*"([^"]*)"')
CONFIRM_LITERAL = re.compile(r'"CONFIRM"')
ASSIGNED = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\.\s*contract\s*\.\s*confirmation\s*=")
UNDOABLE = re.compile(
    r"\.\s*reversible\s*\(\s*\)"
    r"|\breversible\s*:\s*true\b"
    r"|\bCOMPENSATED_UNDO_TAG\b"
    r"|\bActionKind\s*::\s*Undoable\b"
)


def statement_bounds(code: str, at: int) -> tuple[int, int]:
    """The statement around offset `at`: back to the previous `;`, `{` or `}`
    at the same depth, forward to the `;` (or close) that ends it."""
    depth = 0
    start = at
    while start > 0:
        char = code[start - 1]
        if char in ")]}":
            depth += 1
        elif char in "([{":
            if depth == 0:
                break
            depth -= 1
        elif char == ";" and depth == 0:
            break
        start -= 1
    depth = 0
    end = at
    while end < len(code):
        char = code[end]
        if char in "([{":
            depth += 1
        elif char in ")]}":
            if depth == 0:
                break
            depth -= 1
        elif char == ";" and depth == 0:
            break
        end += 1
    return start, end


def check_source(relative: str, text: str) -> list[str]:
    code = production_source(text)
    errors: list[str] = []
    for found in CONFIRM_LITERAL.finditer(code):
        errors.append(
            f'{relative}:{line_number(code, found.start())}: "CONFIRM" is never a word a '
            "person types: an undoable action asks nothing, an irreversible one names what it "
            "destroys"
        )
    for found in PHRASE.finditer(code):
        line = line_number(code, found.start())
        start, end = statement_bounds(code, found.start())
        statement = code[start:end]
        literal = PHRASE_LITERAL.search(code, found.end(), min(end, found.end() + 400))
        if literal and literal.group(1).strip().lower() in GENERIC_WORDS:
            errors.append(
                f'{relative}:{line}: typed confirmation "{literal.group(1)}" says nothing '
                "about what is lost; make the phrase the name of what it destroys"
            )
        # A phrase set on a spec built earlier: the statement that built it.
        assigned = ASSIGNED.search(statement)
        if assigned:
            name = re.escape(assigned.group(1))
            # The nearest binding before it is the one it changes.
            built = None
            for built in re.finditer(rf"\blet\s+(?:mut\s+)?{name}\b", code[:start]):
                pass
            if built is not None:
                built_start, built_end = statement_bounds(code, built.end())
                statement += code[built_start:built_end]
        if UNDOABLE.search(statement):
            errors.append(
                f"{relative}:{line}: a typed confirmation on an action Undo takes back; it "
                "runs at once and offers Undo instead"
            )
    return errors


def undoable_deletions(relative: str, text: str) -> list[tuple[str, int]]:
    """Each destructive action in `text` that Undo is taken to reverse, as
    `<file>::<fn>` with its line: `#[action(destructive)]` without
    `irreversible`, and an `#[effect]` that is both destructive and undoable."""
    code = production_source(text)
    found: list[tuple[str, int]] = []
    for attribute in ATTRIBUTE.finditer(code):
        kind, arguments = attribute.groups()
        words = set(WORD.findall(arguments))
        if "destructive" not in words or "irreversible" in words:
            continue
        if kind == "effect" and not re.search(r'kind\s*=\s*"undoable"', arguments):
            continue
        item = ITEM.search(code, attribute.end())
        if item is None:
            continue
        name = item.group(1) or item.group(2)
        found.append((f"{relative}::{name}", line_number(code, attribute.start())))
    return found


def check_ledger(found: dict[str, int], ledger: dict[str, str]) -> list[str]:
    errors = []
    for key, line in sorted(found.items()):
        if not ledger.get(key, "").strip():
            path, name = key.split("::")
            errors.append(
                f"{path}:{line}: `{name}` is destructive and runs at once with Undo: if "
                "everything it does is in the record Undo restores, list it with why in "
                "tools/policy/undoable-deletions.toml; if it reaches outside (a server, a "
                "device, an inbox, what others saw), mark it `irreversible` so it is asked first"
            )
    for key in sorted(set(ledger) - set(found)):
        errors.append(
            f"tools/policy/undoable-deletions.toml: {key} is no destructive action Undo "
            "takes back any more; take it off the list"
        )
    return errors


def main() -> int:
    errors: list[str] = []
    found: dict[str, int] = {}
    for tree in TREES:
        for path in sorted((ROOT / tree).rglob("*.rs")):
            relative = path.relative_to(ROOT)
            if "target" in relative.parts or is_test_file(relative):
                continue
            text = path.read_text(encoding="utf-8", errors="replace")
            if "destructive" in text:
                found.update(undoable_deletions(relative.as_posix(), text))
            if "Phrase" not in text and "CONFIRM" not in text:
                continue
            errors.extend(check_source(relative.as_posix(), text))
    ledger = tomllib.loads(LEDGER.read_text(encoding="utf-8")).get("undoable", {})
    errors.extend(check_ledger(found, ledger))
    for error in errors:
        print(error, file=sys.stderr)
    if errors:
        print(
            f"{len(errors)} typed confirmation(s) break the rule: undoable actions run at once "
            "with Undo; only irreversible ones ask for a typed name",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

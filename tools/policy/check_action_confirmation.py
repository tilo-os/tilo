#!/usr/bin/env python3
"""Typed confirmation is for what Undo cannot take back, and names it.

Undo and History are system services: an action Undo takes back runs at once
and offers Undo, with no confirmation at all. A typed confirmation
(`ConfirmationPolicy::Phrase`) is reserved for irreversible, high-risk actions
(erasing a drive, deleting an app's data), and the phrase names what is lost,
so typing it means the person read it. This check refuses, in production Rust:

- a typed phrase on an action declared undoable: in the same builder chain or
  contract literal as a compensated undo or `ActionKind::Undoable`;
- a typed phrase that is a generic word ("CONFIRM", "YES", "DELETE", ...),
  which says nothing about what is lost;
- a `"CONFIRM"` literal anywhere: no person is ever asked to type it.

`ActionContract::validate_declaration` refuses the same at runtime; this check
catches it before anything runs.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import is_test_file, line_number, production_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
TREES = ("apps", "crates", "examples")

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
    r"\bkind\s*:\s*ActionKind\s*::\s*Undoable\b"
    r"|\bof_kind\s*\(\s*ActionKind\s*::\s*Undoable\s*\)"
    r"|\bCOMPENSATED_UNDO_TAG\b"
)


NAME_ASSIGNED = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\.\s*contract\s*\.")
KIND_ASSIGNED = re.compile(
    r"\b([A-Za-z_][A-Za-z0-9_]*)(?:\s*\.\s*contract)?\s*\.\s*kind\s*=\s*"
    r"ActionKind\s*::\s*(Undoable|Outside|Read)\b"
)


def names_undoable(statement: str, code: str, start: int) -> bool:
    """Whether `statement` names an action Undo takes back: its own contract
    says so, or it is a binding of one that does, reached through the builder
    chain (`spec.kind = ActionKind::Undoable`) or a contract field
    (`spec.contract.kind = ActionKind::Undoable`). A binding that goes on to
    name another kind says what it is as well: one that reaches outside is
    not undoable, whatever it started as."""
    # The kind the chain names last is what it is: one a builder set later
    # (the arm of a conditional) overrides one set before it.
    kinds = list(KIND_ASSIGNED.finditer(statement))
    if kinds:
        return kinds[-1].group(2) == "Undoable"
    named = NAME_ASSIGNED.search(statement)
    if named is None:
        # A contract literal, wherever it sits.
        return bool(UNDOABLE.search(statement))
    # The nearest binding before the statement that changes it, and what
    # that binding is: never what this binding's own chain says.
    name = re.escape(named.group(1))
    binding = None
    for binding in re.finditer(
        rf"\b(?:let|const|static)\s+(?:mut\s+)?{name}\b", code[:start]
    ):
        pass
    built_start = statement_bounds(code, binding.end())[0] if binding else start
    return names_undoable(code[built_start:start], code, built_start)


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
        # A phrase set on a spec built earlier is read with what that
        # binding already says it is: `names_undoable` reaches the binding
        # from the kind the statement sets, never from its own text.
        if names_undoable(statement, code, start):
            errors.append(
                f"{relative}:{line}: a typed confirmation on an action Undo takes back; it "
                "runs at once and offers Undo instead"
            )
    return errors


def main() -> int:
    errors: list[str] = []
    for tree in TREES:
        for path in sorted((ROOT / tree).rglob("*.rs")):
            relative = path.relative_to(ROOT)
            if "target" in relative.parts or is_test_file(relative):
                continue
            text = path.read_text(encoding="utf-8", errors="replace")
            if "Phrase" not in text and "CONFIRM" not in text:
                continue
            errors.extend(check_source(relative.as_posix(), text))
    for error in errors:
        print(error)
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

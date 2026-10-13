#!/usr/bin/env python3
"""Every Fluent key a crate's source resolves exists in each catalogue it ships.

A key with no message renders as the key itself, so a missing entry ships an
untranslatable identifier to the screen. Keys passed straight to a resolver
(`t`, `t_args`, `tr`, `tr_args`) are checked in every crate with a
`locales/en-US.ftl`; Settings also routes keys through conditionals and
tables, so its key-shaped literals in a known namespace are checked too.
Lookups fall back to the toolkit's catalogue, so its messages count for all.
"""

from __future__ import annotations

import re
from pathlib import Path

import check_app_portability as rust


ROOT = Path(__file__).resolve().parents[2]
MESSAGE = re.compile(r"^([A-Za-z][A-Za-z0-9_-]*)\s*=", re.MULTILINE)
IDENTIFIER = re.compile(r"[A-Za-z][A-Za-z0-9_-]*\Z")
KEY = re.compile(r"[A-Za-z][A-Za-z0-9_]*(?:-[A-Za-z0-9_]+)+\Z")
RESOLVER = re.compile(r"\b(?:t|t_args|tr|tr_args)\s*\(")
# These argument positions are stable machine identifiers, never Fluent IDs.
MACHINE_CALL = re.compile(
    r"(?:\bButton::new|\.(?:with_id|thread_name|name))\s*\("
)


def first_argument(code: str, start: int, limit: int | None = None) -> tuple[int, int]:
    """The first argument of a lexed call, preserving source offsets."""
    end = rust.matching(code, start - 1) - 1 if limit is None else limit
    position = start
    while position < end:
        if code[position] in "([{":
            position = rust.matching(code, position)
        elif code[position] == ",":
            return start, position
        else:
            position += 1
    return start, end


def contains(ranges: list[tuple[int, int]], offset: int) -> bool:
    return any(start <= offset < end for start, end in ranges)


def used_keys(text: str, namespaces: set[str]) -> list[tuple[str, int]]:
    """Production copy references, without comments/test or control IDs."""
    code, literals = rust.lex(text)
    pruned = rust.strip_tests(text, code)
    resolved = [first_argument(pruned.code, call.end())
                for call in RESOLVER.finditer(pruned.code)]
    machine = [first_argument(pruned.code, call.end())
               for call in MACHINE_CALL.finditer(pruned.code)]
    # Navigation slugs and control-ID pairs are passed separately from copy.
    for function in re.finditer(r"\bfn\s+slug\s*\([^{}]*\)[^{]*\{", pruned.code):
        machine.append((function.end(), rust.matching(pruned.code, function.end() - 1)))
    for call in re.finditer(r"\.push_confirm\s*\(", pruned.code):
        limit = rust.matching(pruned.code, call.end() - 1) - 1
        start, end = first_argument(pruned.code, call.end(), limit)
        start, end = first_argument(pruned.code, end + 1, limit)
        start, end = first_argument(pruned.code, end + 1, limit)
        machine.append((start, end))
    found = []
    for literal in literals:
        if (literal.byte or not pruned.keeps(literal.start)
                or re.match(r'c(?:r#*)?"', text[literal.start:literal.end])):
            continue
        direct = contains(resolved, literal.start)
        key = literal.value
        if not (IDENTIFIER if direct else KEY).fullmatch(key):
            continue
        # Match-arm selectors describe stored values, not displayed copy.
        if re.match(r"\s*(?:=>|\|)", code[literal.end:]):
            continue
        if not direct and key.partition("-")[0] not in namespaces:
            continue
        if not direct and (contains(machine, literal.start)
                           or re.search(r"\bicon\s*:\s*$", code[:literal.start])):
            continue
        found.append((key, rust.line_number(text, literal.start)))
    return found


TOOLKIT = "crates/sdk/ui-core"
# Crates whose copy keys also travel through conditionals and tables.
INDIRECT = {"apps/settings"}


def catalogues(crate: Path) -> dict[str, set[str]]:
    return {path.stem: set(MESSAGE.findall(path.read_text()))
            for path in sorted((crate / "locales").glob("*.ftl"))}


def copy_crates(root: Path) -> list[Path]:
    return sorted(ftl.parents[1] for pattern in ("apps/*", "crates/*/*")
                  for ftl in root.glob(f"{pattern}/locales/en-US.ftl"))


def missing_copy(root: Path) -> list[str]:
    toolkit = catalogues(root / TOOLKIT)
    errors = []
    for crate in copy_crates(root):
        own = catalogues(crate)
        name = crate.relative_to(root).as_posix()
        indirect = name in INDIRECT
        namespaces = {key.partition("-")[0] for key in own["en-US"]} if indirect else set()
        for path in sorted((crate / "src").rglob("*.rs")):
            # Settings' scenarios name capture-only states, not live panes.
            if indirect and path.name == "scenarios.rs":
                continue
            for key, line in used_keys(path.read_text(), namespaces):
                for locale, keys in own.items():
                    if key not in keys and key not in toolkit.get(locale, toolkit["en-US"]):
                        errors.append(f"{path.relative_to(root)}:{line}: {locale} lacks {key}")
    return errors


def main() -> int:
    errors = missing_copy(ROOT)
    if errors:
        print("\n".join(errors))
        return 1
    print("app copy OK (every resolved key is in each shipped catalogue)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Every Fluent key a crate's source resolves exists in each catalogue it ships.

A key with no message renders as the key itself, so a missing entry ships an
untranslatable identifier to the screen. Keys passed straight to a resolver
(`t`, `t_args`, `tr`, `tr_args`) are checked in every crate with a
`locales/en-US.ftl`; Settings also routes keys through conditionals and
tables, so its key-shaped literals in a known namespace are checked too.
Lookups fall back to the toolkit's catalogue, so its messages count for all.
A key passed to another crate's resolver (`tilo_desktop::i18n::t_args`) is
checked in that crate's catalogue, the one it is looked up in.
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
QUALIFIED = re.compile(r"\b([A-Za-z_]\w*)::i18n::(?:t|t_args|tr|tr_args)\s*\(")
# These argument positions are stable machine identifiers, never Fluent IDs.
MACHINE_CALL = re.compile(
    r"(?:\bButton::new|\bdevice_read::thread|\.(?:with_id|thread_name|name))\s*\("
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


def foreign_calls(code: str, modules: dict[str, Path]) -> list[tuple[re.Match[str], Path]]:
    """Resolver calls qualified with another crate's `i18n` module."""
    return [(call, modules[call.group(1)]) for call in QUALIFIED.finditer(code)
            if call.group(1) in modules]


def used_keys(text: str, namespaces: set[str],
              foreign: dict[str, Path] | None = None) -> list[tuple[str, int]]:
    """Production copy references, without comments/test or control IDs.

    Keys passed to another crate's resolver (`foreign`) are that crate's, so
    they are left to `foreign_keys`.
    """
    code, literals = rust.lex(text)
    pruned = rust.strip_tests(text, code)
    elsewhere = [first_argument(pruned.code, call.end())
                 for call, _ in foreign_calls(pruned.code, foreign or {})]
    resolved = [first_argument(pruned.code, call.end())
                for call in RESOLVER.finditer(pruned.code)]
    resolved = [span for span in resolved if span not in elsewhere]
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
        # A compared value inside a resolver's argument (`t(if id == "icloud"
        # { … } else { … })`) is a state, not the key being resolved.
        if (re.search(r"[=!]=\s*$", code[:literal.start])
                or re.match(r"\s*[=!]=", code[literal.end:])):
            continue
        if contains(elsewhere, literal.start):
            continue
        if not direct and key.partition("-")[0] not in namespaces:
            continue
        if not direct and (contains(machine, literal.start)
                           or re.search(r"\bicon\s*:\s*$", code[:literal.start])):
            continue
        found.append((key, rust.line_number(text, literal.start)))
    return found


def foreign_keys(text: str, modules: dict[str, Path]) -> list[tuple[Path, str, int]]:
    """Keys passed straight to another crate's resolver, with that crate."""
    code, literals = rust.lex(text)
    pruned = rust.strip_tests(text, code)
    found = []
    for call, crate in foreign_calls(pruned.code, modules):
        start, end = first_argument(pruned.code, call.end())
        for literal in literals:
            if (start <= literal.start < end and not literal.byte
                    and pruned.keeps(literal.start) and IDENTIFIER.fullmatch(literal.value)):
                found.append((crate, literal.value, rust.line_number(text, literal.start)))
    return found


def i18n_modules(root: Path, crates: list[Path]) -> dict[str, Path]:
    """Each crate whose `i18n` module resolves a catalogue, by library name.

    A crate that re-exports another's `i18n` (`pub use other::{i18n, ..}`)
    resolves through that crate's catalogue.
    """
    def library(crate: Path) -> str | None:
        manifest = crate / "Cargo.toml"
        if not manifest.is_file():
            return None
        found = re.search(r'^name\s*=\s*"([^"]+)"', manifest.read_text(), re.MULTILINE)
        return found.group(1).replace("-", "_") if found else None

    modules = {name: crate for crate in crates if (name := library(crate))}
    for lib in sorted(root.glob("crates/*/*/src/lib.rs")) + sorted(root.glob("apps/*/src/lib.rs")):
        name = library(lib.parents[1])
        if name is None or name in modules:
            continue
        for use in re.finditer(r"\bpub use (\w+)::\{?([^;]*);", lib.read_text()):
            if use.group(1) in modules and re.search(r"\bi18n\b", use.group(2)):
                modules[name] = modules[use.group(1)]
    return modules


TOOLKIT = "crates/sdk/ui-core"
# Crates whose copy keys also travel through conditionals and tables.
INDIRECT = {"apps/settings-core"}
# Catalogues a crate's resolver falls back to after its own: Settings' `t`
# resolves the shared provider administration copy it embeds
# (apps/settings-core/src/i18n.rs).
FALLBACK = {"apps/settings-core": ["crates/sdk/provider-settings-ui"]}
# Crates whose copy is another crate's catalogue: Settings' panes, in the
# app and in settings-rare, resolve settings-core's.
SOURCES = {"apps/settings-core": ["apps/settings", "apps/settings-rare"]}


def catalogues(crate: Path) -> dict[str, set[str]]:
    return {path.stem: set(MESSAGE.findall(path.read_text()))
            for path in sorted((crate / "locales").glob("*.ftl"))}


def copy_crates(root: Path) -> list[Path]:
    return sorted(ftl.parents[1] for pattern in ("apps/*", "crates/*/*")
                  for ftl in root.glob(f"{pattern}/locales/en-US.ftl"))


def missing_copy(root: Path) -> list[str]:
    toolkit = catalogues(root / TOOLKIT)
    errors = []
    crates = copy_crates(root)
    modules = i18n_modules(root, crates)
    for crate in crates:
        own = catalogues(crate)
        name = crate.relative_to(root).as_posix()
        shared = [catalogues(root / fallback) for fallback in FALLBACK.get(name, [])]
        indirect = name in INDIRECT
        namespaces = {key.partition("-")[0] for key in own["en-US"]} if indirect else set()
        sources = [crate] + [root / other for other in SOURCES.get(name, [])]
        for path in sorted(p for source in sources for p in (source / "src").rglob("*.rs")):
            # Settings' scenarios name capture-only states, not live panes.
            if indirect and path.name == "scenarios.rs":
                continue
            text = path.read_text()
            foreign = {name: other for name, other in modules.items() if other != crate}
            for key, line in used_keys(text, namespaces, foreign):
                for locale, keys in own.items():
                    if key in keys or key in toolkit.get(locale, toolkit["en-US"]):
                        continue
                    if not any(key in fallback.get(locale, ()) for fallback in shared):
                        errors.append(f"{path.relative_to(root)}:{line}: {locale} lacks {key}")
            for other, key, line in foreign_keys(text, foreign):
                for locale, keys in catalogues(other).items():
                    if key not in keys and key not in toolkit.get(locale, toolkit["en-US"]):
                        errors.append(f"{path.relative_to(root)}:{line}: {locale} lacks {key} "
                                      f"({other.relative_to(root).as_posix()})")
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

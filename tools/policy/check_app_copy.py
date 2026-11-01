#!/usr/bin/env python3
"""Every Fluent key a crate's source resolves exists in each catalogue it ships.

A key with no message renders as the key itself, so a missing entry ships an
untranslatable identifier to the screen. Keys passed straight to a resolver
(`t`, `t_args`, `tr`, `tr_args`) are checked in every crate with a
`locales/en-US.ftl`, and in a crate that re-exports another's `i18n` without
shipping a catalogue of its own. Settings, and the desktop surface with the
desktop that draws it, also route keys through conditionals and tables, so
their key-shaped literals in a known namespace are checked too.
Lookups fall back to the toolkit's catalogue, so its messages count for all.
A key passed to another crate's resolver (`tilo_desktop::i18n::t_args`, or
`tilo_onboarding::t` from a crate whose root declares
`ui::embedded_catalog!(pub fn, ..)`) is checked in that crate's catalogue, the
one it is looked up in.
"""

from __future__ import annotations

import bisect
import functools
import re
import sys
from pathlib import Path

import rust_source as rust


ROOT = Path(__file__).resolve().parents[2]
MESSAGE = re.compile(r"^([A-Za-z][A-Za-z0-9_-]*)\s*=", re.MULTILINE)
IDENTIFIER = re.compile(r"[A-Za-z][A-Za-z0-9_-]*\Z")
KEY = re.compile(r"[A-Za-z][A-Za-z0-9_]*(?:-[A-Za-z0-9_]+)+\Z")
# The patterns below start with a literal, so a search jumps from one occurrence of it to the
# next; one that starts with `\b` or an alternation is tried at every offset of some 100 MB of
# source. A leading `\b` is checked by `starts_word` instead.
RESOLVER = re.compile(r"t(?:_args|r|r_args)?\s*\(")
# `<path>::t(`: found by its literal part, then the path (`tilo_desktop::i18n`, `tilo_onboarding`)
# read backwards, which a search from every position took seconds per tree for.
QUALIFIED = re.compile(r"::(?:t|t_args|tr|tr_args)\s*\(")
# A crate root that declares its own lookups (`ui::embedded_catalog!(pub fn, ..)`) resolves
# `<crate>::t(` as well as `<crate>::i18n::t(`.
ROOT_CATALOGUE = re.compile(r"\bembedded_catalog!\s*\(\s*pub\s+fn\b")
# These argument positions are stable machine identifiers, never Fluent IDs.
MACHINE_CALLS = (re.compile(r"Button::new\s*\("), re.compile(r"device_read::thread\s*\("))
MACHINE_METHOD = re.compile(r"\.(?:with_id|thread_name|name|start_job)\s*\(")
SLUG = re.compile(r"fn\s+slug\s*\([^{}]*\)[^{]*\{")
ARM_AFTER = re.compile(r"\s*(?:=>|\|)")
COMPARED_AFTER = re.compile(r"\s*[=!]=")
WORD = re.compile(r"\w")
IDENT = re.compile(r"[A-Za-z_]\w*")
C_STRING = re.compile(r'c(?:r#*)?"')


def starts_word(code: str, offset: int) -> bool:
    """A `\\b` before a word character at `offset`."""
    return offset == 0 or not WORD.match(code[offset - 1])


def resolver_calls(code: str) -> list[re.Match[str]]:
    """`t(`, `t_args(`, `tr(` and `tr_args(` (`\\b(?:t|t_args|tr|tr_args)\\s*\\(`)."""
    return [call for call in RESOLVER.finditer(code) if starts_word(code, call.start())]


def machine_calls(code: str) -> list[re.Match[str]]:
    """Calls whose first argument is a machine identifier, in source order."""
    calls = [call for pattern in MACHINE_CALLS for call in pattern.finditer(code)
             if starts_word(code, call.start())]
    return sorted(calls + list(MACHINE_METHOD.finditer(code)), key=lambda call: call.start())


def lexed(text: str):
    """A source's lexed code, literals and test-pruned code, worked out once for both readers of
    a file (`used_keys`, `foreign_keys`)."""
    return _lexed(text)


@functools.lru_cache(maxsize=2)
def _lexed(text: str):
    code, literals = rust.lex(text)
    return code, literals, rust.strip_tests(text, code)


def before_whitespace(code: str, offset: int) -> int:
    """Where the whitespace that ends at `offset` starts."""
    while offset > 0 and code[offset - 1].isspace():
        offset -= 1
    return offset


def compared_before(code: str, offset: int) -> bool:
    """`code[:offset]` ends in `==` or `!=` and whitespace (`[=!]=\\s*$`), without copying it."""
    end = before_whitespace(code, offset)
    return end >= 2 and code[end - 1] == "=" and code[end - 2] in "=!"


def icon_field_before(code: str, offset: int) -> bool:
    """`code[:offset]` ends in `icon:` and whitespace (`\\bicon\\s*:\\s*$`), without copying it."""
    end = before_whitespace(code, offset)
    if end < 1 or code[end - 1] != ":":
        return False
    end = before_whitespace(code, end - 1)
    return code[end - 4:end] == "icon" and (end == 4 or not WORD.match(code[end - 5]))


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


class Spans:
    """Ranges of a source, asked whether they hold an offset in logarithmic time: a file has
    hundreds of resolver calls and thousands of literals."""

    def __init__(self, ranges: list[tuple[int, int]]):
        ordered = sorted(ranges)
        self.ranges = ranges
        self.starts = [start for start, _ in ordered]
        self.reach = []
        furthest = -1
        for _, end in ordered:
            furthest = max(furthest, end)
            self.reach.append(furthest)

    def __contains__(self, offset: int) -> bool:
        index = bisect.bisect_right(self.starts, offset) - 1
        return index >= 0 and self.reach[index] > offset


def contains(ranges: list[tuple[int, int]] | Spans, offset: int) -> bool:
    if isinstance(ranges, Spans):
        return offset in ranges
    return any(start <= offset < end for start, end in ranges)


class Qualified:
    """A resolver call qualified with a path: the path (`group(1)`, `tilo_desktop::i18n`) and
    where the call's arguments start (`end()`)."""

    def __init__(self, crate: str, end: int):
        self.crate, self._end = crate, end

    def group(self, index: int) -> str:
        assert index == 1
        return self.crate

    def end(self) -> int:
        return self._end


def qualified_calls(code: str) -> list[Qualified]:
    calls = []
    for call in QUALIFIED.finditer(code):
        start = call.start()
        while True:
            segment = start
            while segment > 0 and (code[segment - 1].isalnum() or code[segment - 1] == "_"):
                segment -= 1
            if not IDENT.fullmatch(code[segment:start]):
                break
            start = segment
            if not code.endswith("::", 0, start):
                break
            start -= 2
        if start < call.start() and IDENT.match(code, start):
            calls.append(Qualified(code[start:call.start()], call.end()))
    return calls


def foreign_calls(code: str, modules: dict[str, Path]) -> list[tuple[Qualified, Path]]:
    """Resolver calls qualified with another crate's `i18n` module or catalogue root."""
    return [(call, modules[call.group(1)]) for call in qualified_calls(code)
            if call.group(1) in modules]


def used_keys(text: str, namespaces: set[str],
              foreign: dict[str, Path] | None = None) -> list[tuple[str, int]]:
    """Production copy references, without comments/test or control IDs.

    Keys passed to another crate's resolver (`foreign`) are that crate's, so
    they are left to `foreign_keys`.
    """
    code, literals, pruned = lexed(text)
    elsewhere = [first_argument(pruned.code, call.end())
                 for call, _ in foreign_calls(pruned.code, foreign or {})]
    resolved = [first_argument(pruned.code, call.end())
                for call in resolver_calls(pruned.code)]
    excluded = set(elsewhere)
    resolved = [span for span in resolved if span not in excluded]
    resolved_spans = Spans(resolved)
    machine = [first_argument(pruned.code, call.end())
               for call in machine_calls(pruned.code)]
    # Navigation slugs are passed separately from copy.
    for function in (f for f in SLUG.finditer(pruned.code) if starts_word(pruned.code, f.start())):
        machine.append((function.end(), rust.matching(pruned.code, function.end() - 1)))
    found = []
    elsewhere_spans, machine_spans = Spans(elsewhere), Spans(machine)
    for literal in literals:
        # Only identifier-shaped literals can be keys (KEY is a narrower IDENTIFIER): the cheap
        # test first, as most literals of a file are not.
        if literal.byte or not IDENTIFIER.fullmatch(literal.value):
            continue
        if not pruned.keeps(literal.start) or C_STRING.match(text, literal.start):
            continue
        direct = literal.start in resolved_spans
        key = literal.value
        if not (IDENTIFIER if direct else KEY).fullmatch(key):
            continue
        # Match-arm selectors describe stored values, not displayed copy.
        if ARM_AFTER.match(code, literal.end):
            continue
        # A compared value inside a resolver's argument (`t(if id == "icloud"
        # { … } else { … })`) is a state, not the key being resolved.
        if compared_before(code, literal.start) or COMPARED_AFTER.match(code, literal.end):
            continue
        if literal.start in elsewhere_spans:
            continue
        if not direct and key.partition("-")[0] not in namespaces:
            continue
        if not direct and (literal.start in machine_spans or icon_field_before(code, literal.start)):
            continue
        found.append((key, rust.line_number(text, literal.start)))
    return found


def foreign_keys(text: str, modules: dict[str, Path]) -> list[tuple[Path, str, int]]:
    """Keys passed straight to another crate's resolver, with that crate."""
    code, literals, pruned = lexed(text)
    found = []
    for call, crate in foreign_calls(pruned.code, modules):
        start, end = first_argument(pruned.code, call.end())
        for literal in literals:
            if (start <= literal.start < end and not literal.byte
                    and pruned.keeps(literal.start) and IDENTIFIER.fullmatch(literal.value)):
                found.append((crate, literal.value, rust.line_number(text, literal.start)))
    return found


def library(crate: Path) -> str | None:
    """The library name a crate's manifest gives, as Rust spells it."""
    manifest = crate / "Cargo.toml"
    if not manifest.is_file():
        return None
    found = re.search(r'^name\s*=\s*"([^"]+)"', manifest.read_text(), re.MULTILINE)
    return found.group(1).replace("-", "_") if found else None


def i18n_modules(root: Path, crates: list[Path]) -> dict[str, Path]:
    """Each path whose resolvers look up a crate's catalogue: `<lib>::i18n`, and `<lib>`
    when the crate's root declares its lookups (`embedded_catalog!(pub fn, ..)`).

    A crate that re-exports another's `i18n` (`pub use other::{i18n, ..}`)
    resolves through that crate's catalogue.
    """
    owners = {name: crate for crate in crates if (name := library(crate))}
    modules: dict[str, Path] = {}
    for name, crate in owners.items():
        modules[f"{name}::i18n"] = crate
        lib = crate / "src" / "lib.rs"
        if lib.is_file() and ROOT_CATALOGUE.search(lib.read_text()):
            modules[name] = crate
    for lib in sorted(root.glob("crates/*/*/src/lib.rs")) + sorted(root.glob("apps/*/src/lib.rs")):
        name = library(lib.parents[1])
        if name is None or name in owners:
            continue
        for use in re.finditer(r"\bpub use (\w+)::\{?([^;]*);", lib.read_text()):
            if use.group(1) in owners and re.search(r"\bi18n\b", use.group(2)):
                modules[f"{name}::i18n"] = owners[use.group(1)]
    return modules


def copy_readers(root: Path, modules: dict[str, Path], crates: list[Path]) -> list[tuple[Path, Path]]:
    """Each crate that resolves copy, with the crate whose catalogue it resolves in.

    That is the crates shipping a catalogue, plus a crate that re-exports
    another's `i18n` and ships none: `crates/session/desktop` reads the
    desktop surface's.
    """
    directories = {}
    for pattern in ("apps/*", "crates/*/*"):
        for manifest in root.glob(f"{pattern}/Cargo.toml"):
            name = library(manifest.parent)
            if name is not None:
                directories.setdefault(name, manifest.parent)
    readers = [(crate, crate) for crate in crates]
    readers.extend(
        (directories[name], crate) for name, crate in sorted(modules.items())
        if name in directories and directories[name] not in crates
    )
    return readers


TOOLKIT = "crates/sdk/ui-core"
# Crates whose copy keys also travel through conditionals and tables, by the
# namespaces their code picks keys in (`None`: every namespace of their own
# catalogue). Settings picks across its whole catalogue; the desktop surface
# picks `activity-` keys in a `match` and in its word builders.
INDIRECT = {
    "apps/settings": None,
    "crates/session/desktop": {"activity"},
    "crates/session/desktop-surface": {"activity"},
}
# Catalogues a crate's resolver falls back to after its own: Settings' `t`
# resolves the shared provider administration copy it embeds
# (apps/settings/src/i18n.rs).
FALLBACK = {"apps/settings": ["crates/sdk/provider-settings-ui"]}


def catalogues(crate: Path) -> dict[str, set[str]]:
    return {path.stem: set(MESSAGE.findall(path.read_text()))
            for path in sorted((crate / "locales").glob("*.ftl"))}


def copy_crates(root: Path) -> list[Path]:
    return sorted(ftl.parents[1] for pattern in ("apps/*", "crates/*/*")
                  for ftl in root.glob(f"{pattern}/locales/en-US.ftl"))


def indirect_namespaces(name: str, own: dict[str, set[str]]) -> set[str]:
    """The namespaces whose key-shaped literals in `name` are keys even where no resolver
    call takes them directly."""
    if name not in INDIRECT:
        return set()
    prefixes = INDIRECT[name]
    return {key.partition("-")[0] for key in own["en-US"]} if prefixes is None else set(prefixes)


def missing_copy(root: Path) -> list[str]:
    toolkit = catalogues(root / TOOLKIT)
    errors = []
    crates = copy_crates(root)
    modules = i18n_modules(root, crates)
    for source, crate in copy_readers(root, modules, crates):
        own = catalogues(crate)
        name = source.relative_to(root).as_posix()
        shared = [catalogues(root / fallback) for fallback in FALLBACK.get(name, [])]
        namespaces = indirect_namespaces(name, own)
        for path in sorted((source / "src").rglob("*.rs")):
            # Settings' scenarios name capture-only states, not live panes.
            if name == "apps/settings" and path.name == "scenarios.rs":
                continue
            text = path.read_text()
            foreign = {module: other for module, other in modules.items() if other != crate}
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
    print("app copy OK (every resolved key is in each shipped catalogue)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

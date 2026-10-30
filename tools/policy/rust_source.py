"""Shared reading of Rust sources for the repository policy checks."""

import bisect
import fnmatch
import re
import tomllib
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path

# An attribute that compiles what follows only for tests; group 1 is `!`
# for the inner form that gates its whole file.
TEST_ATTRIBUTE = re.compile(r"#(!?)\[(?:cfg\((?:test|all\(test\b)|test\])")
LINE_COMMENT = re.compile(r"(?m)^[ \t]*//.*$")
CLOSING = {")": "(", "]": "[", "}": "{"}
STRING_START = re.compile(r'b?r#*"|b"')


def line_number(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def is_test_file(relative: Path) -> bool:
    """Whether `relative` is test or benchmark code rather than production code."""
    name = relative.name
    return (
        "tests" in relative.parts
        or "benches" in relative.parts
        or name == "tests.rs"
        or name.endswith("_tests.rs")
    )


def production_source(text: str) -> str:
    """The source without its test-only items or full-line comments, blanked
    so diagnostics keep their line numbers."""
    return blank_test_items(LINE_COMMENT.sub("", text))


def blank_test_items(code: str) -> str:
    """`code` with each `#[cfg(test)]`, `#[cfg(all(test, …))]` or `#[test]`
    item blanked on its own (a module, function, `use`, field or statement),
    keeping newlines; `#![cfg(test)]` blanks it all. A test-only import near
    the top does not hide the rest of the file. Braces inside comments and
    literals are counted, so callers that can should pass lexed code."""
    out = list(code)
    at = 0
    while found := TEST_ATTRIBUTE.search(code, at):
        if found.group(1):
            return re.sub(r"[^\n]", " ", code)
        end = test_item_end(code, found.start())
        for i in range(found.start(), end):
            if out[i] != "\n":
                out[i] = " "
        at = end
    return "".join(out)


def test_item_end(code: str, start: int) -> int:
    """End of the item gated by the attribute at `start`: its attributes, then
    up to a `;` or `,`, the close of its first top-level block, or the close
    of the enclosing one."""
    depth: list[str] = []
    in_attribute = False
    i = start
    while i < len(code):
        char = code[i]
        if char == '"' or (char in "br" and STRING_START.match(code, i) and not is_ident(code, i - 1)):
            i = skip_string(code, i)
            continue
        if char in "([{":
            if not depth and char == "[" and code[i - 1 : i] in ("#", "!"):
                in_attribute = True
            depth.append(char)
        elif char in CLOSING:
            if not depth:
                return i
            depth.pop()
            if not depth:
                if in_attribute:
                    in_attribute = False
                elif char == "}":
                    return i + 1
        elif char in ";," and not depth:
            return i + 1
        i += 1
    return len(code)


def is_ident(text: str, index: int) -> bool:
    return index >= 0 and (text[index].isalnum() or text[index] == "_")


def skip_string(text: str, start: int) -> int:
    """Index just past the string literal opening at `start`."""
    raw = re.match(r'b?r(#*)"', text[start:])
    if raw:
        closing = '"' + raw.group(1)
        end = text.find(closing, start + raw.end())
        return len(text) if end < 0 else end + len(closing)
    index = text.index('"', start) + 1
    while index < len(text):
        if text[index] == "\\":
            index += 2
            continue
        if text[index] == '"':
            return index + 1
        index += 1
    return len(text)



@dataclass(frozen=True)
class Literal:
    start: int
    end: int
    value: str
    byte: bool


CHAR_LITERAL = re.compile(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]{1,6}\}|.)|[^'\\\n])'")
LEXEME = re.compile(r"""//|/\*|"|'""")
STRING_BODY = re.compile(r'(?:[^"\\]|\\.)*"', re.DOTALL)
BLOCK_COMMENT_EDGE = re.compile(r"/\*|\*/")
NOT_NEWLINE = re.compile(r"[^\n]")


def _is_ident(char: str) -> bool:
    return char.isalnum() or char == "_"


def _prefix_start(text: str, quote: int, floor: int) -> tuple[int, int | None, bool]:
    """Where the literal whose quote is at ``quote`` starts.

    Returns (start, raw hash count or None, byte literal).
    """
    i = quote
    while i > floor and text[i - 1] == "#":
        i -= 1
    hashes = quote - i
    if i > floor and text[i - 1] == "r":
        start = i - 1
        byte = start > floor and text[start - 1] in "bc"
        start -= 1 if byte else 0
        if start == 0 or not _is_ident(text[start - 1]):
            return start, hashes, byte and text[start] == "b"
    if hashes == 0 and quote > floor and text[quote - 1] in "bc":
        start = quote - 1
        if start == 0 or not _is_ident(text[start - 1]):
            return start, None, text[start] == "b"
    return quote, None, False


def lex(text: str) -> tuple[str, list[Literal]]:
    """Blank comments and literals, keeping offsets and newlines.

    Returns the blanked code and the string literals it contained.
    """
    pieces: list[str] = []
    literals: list[Literal] = []
    position = 0
    length = len(text)
    while match := LEXEME.search(text, position):
        start = match.start()
        lexeme = match.group()
        if lexeme == "//":
            end = text.find("\n", start)
            end = length if end < 0 else end
        elif lexeme == "/*":
            depth, end = 1, start + 2
            while depth and (edge := BLOCK_COMMENT_EDGE.search(text, end)):
                depth += 1 if edge.group() == "/*" else -1
                end = edge.end()
            end = end if not depth else length
        elif lexeme == '"':
            quote = start
            start, hashes, byte = _prefix_start(text, quote, position)
            if hashes is not None:
                terminator = '"' + "#" * hashes
                close = text.find(terminator, quote + 1)
                end = length if close < 0 else close + len(terminator)
                value = text[quote + 1 : max(close, quote + 1)]
            else:
                body = STRING_BODY.match(text, quote + 1)
                end = body.end() if body else length
                value = text[quote + 1 : end - 1]
            literals.append(Literal(start, end, value, byte))
        else:
            char = CHAR_LITERAL.match(text, start)
            if not char or (start > position and _is_ident(text[start - 1])
                            and text[start - 1] != "b"):
                pieces.append(text[position : start + 1])
                position = start + 1
                continue
            start -= 1 if start > position and text[start - 1] == "b" else 0
            end = char.end()
        pieces.append(text[position:start])
        pieces.append(NOT_NEWLINE.sub(" ", text[start:end]))
        position = end
    pieces.append(text[position:])
    return "".join(pieces), literals


BRACKET = re.compile(r"[()\[\]{}]")
CLOSERS = {"(": ")", "[": "]", "{": "}"}


def matching(code: str, open_index: int) -> int:
    """Index just past the bracket that closes ``code[open_index]``."""
    stack = [CLOSERS[code[open_index]]]
    for bracket in BRACKET.finditer(code, open_index + 1):
        char = bracket.group()
        if char in CLOSERS:
            stack.append(CLOSERS[char])
        elif char == stack[-1]:
            stack.pop()
            if not stack:
                return bracket.end()
    return len(code)


# Checks read the same file text repeatedly; key by content so an edit at the
# same path is always relexed.
@lru_cache(maxsize=128)
def code_only(text: str) -> str:
    """`text` with comments and literals blanked, keeping offsets and newlines."""
    return lex(text)[0]


# ---------------------------------------------------------------------------
# Test pruning with offsets: which items of a file exist in a production
# build, and the literal positions that survive them.
# ---------------------------------------------------------------------------


def split_top_level(text: str) -> list[str]:
    parts, depth, current = [], 0, []
    for char in text:
        if char in "([{":
            depth += 1
        elif char in ")]}":
            depth -= 1
        if char == "," and depth == 0:
            parts.append("".join(current))
            current = []
        else:
            current.append(char)
    parts.append("".join(current))
    return [part.strip() for part in parts if part.strip()]


def requires_test(predicate: str) -> bool:
    """Whether a ``cfg`` predicate can only hold in a test build."""
    predicate = re.sub(r"\s+", "", predicate)
    if predicate == "test":
        return True
    match = re.fullmatch(r"(all|any)\((.*)\)", predicate)
    if not match:
        return False
    arguments = split_top_level(match.group(2))
    if match.group(1) == "all":
        return any(requires_test(argument) for argument in arguments)
    return bool(arguments) and all(requires_test(argument) for argument in arguments)


OUTER_ATTRIBUTE = re.compile(r"#\s*\[")
INNER_TEST_CFG = re.compile(r"#\s*!\s*\[\s*cfg\s*\(")
TEST_ITEM_ATTRIBUTE = re.compile(r"(?:[A-Za-z_][\w]*\s*::\s*)*test")
MOD_DECLARATION = re.compile(r"(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_]\w*)\s*;")
PATH_ATTRIBUTE = re.compile(r'#\s*\[\s*path\s*=\s*"([^"]*)"\s*\]')


def attribute_is_test(body: str) -> bool:
    body = body.strip()
    if TEST_ITEM_ATTRIBUTE.fullmatch(body):
        return True
    match = re.fullmatch(r"cfg\s*\((.*)\)", body, re.DOTALL)
    return bool(match) and requires_test(match.group(1))


ITEM_KEYWORD = re.compile(
    r"(?:pub(?:\s*\([^)]*\))?\s+)?(?:(?:unsafe|async|default)\s+)*"
    r"(?:fn|mod|impl|struct|enum|union|trait|const|static|use|type|macro_rules|let|extern)\b"
)


def item_extent(code: str, start: int) -> tuple[int, int]:
    """Start and end of what follows the attributes at ``start``.

    An item or statement ends at its `;` or closing brace. A field, variant,
    match arm or argument ends at the `,` or bracket that closes it.
    """
    i = start
    while True:
        while i < len(code) and code[i].isspace():
            i += 1
        attribute = OUTER_ATTRIBUTE.match(code, i)
        if not attribute:
            break
        i = matching(code, attribute.end() - 1)
    item_start = i
    item = ITEM_KEYWORD.match(code, i) is not None
    depth = 0
    while i < len(code):
        char = code[i]
        if char in "([" or (char == "{" and depth > 0):
            depth += 1
        elif char in ")]}":
            if depth == 0:
                return item_start, i
            depth -= 1
        elif depth == 0 and char == ";":
            return item_start, i + 1
        elif depth == 0 and char == "{":
            return item_start, matching(code, i)
        elif depth == 0 and char == "," and not item:
            return item_start, i + 1
        i += 1
    return item_start, len(code)


@dataclass
class Pruned:
    code: str
    removed: list[tuple[int, int]]
    # (module, `#[path]` value) of each out-of-line test module.
    test_modules: list[tuple[str, str | None]]
    test_only: bool

    def keeps(self, offset: int) -> bool:
        index = bisect.bisect_right(self.removed, (offset, len(self.code) + 1)) - 1
        return index < 0 or not self.removed[index][0] <= offset < self.removed[index][1]


def strip_tests(text: str, code: str) -> Pruned:
    """Blank ``#[test]`` and ``#[cfg(test)]`` items in lexed ``code``."""
    inner = INNER_TEST_CFG.search(code)
    if inner:
        close = matching(code, inner.end() - 1)
        if requires_test(code[inner.end() : close - 1]):
            return Pruned("", [(0, len(code))], [], True)
    out = list(code)
    removed: list[tuple[int, int]] = []
    modules: list[tuple[str, str | None]] = []
    position = 0
    while attribute := OUTER_ATTRIBUTE.search(code, position):
        # The run of attributes on one item: any test attribute gates it all.
        first_close = matching(code, attribute.end() - 1)
        run_end, is_test = attribute.start(), False
        while run := OUTER_ATTRIBUTE.match(code, run_end):
            close = matching(code, run.end() - 1)
            is_test = is_test or attribute_is_test(code[run.end() : close - 1])
            run_end = close
            while run_end < len(code) and code[run_end].isspace():
                run_end += 1
        if not is_test:
            position = first_close
            continue
        item_start, end = item_extent(code, attribute.start())
        declaration = MOD_DECLARATION.match(code, item_start, end)
        if declaration and declaration.end() == end:
            path = PATH_ATTRIBUTE.search(text, attribute.start(), item_start)
            modules.append((declaration.group(1), path.group(1) if path else None))
        for index in range(attribute.start(), end):
            if out[index] != "\n":
                out[index] = " "
        removed.append((attribute.start(), end))
        position = end
    return Pruned("".join(out), removed, modules, False)


def path_regex(path: str, suffix: str = "") -> re.Pattern[str]:
    """``a::b`` with any whitespace; callers check what precedes a match."""
    segments = [re.escape(segment) for segment in path.split("::")]
    return re.compile(r"\s*::\s*".join(segments) + r"\b" + suffix)


def starts(code: str, pattern: re.Pattern[str], forbidden: str = "."):
    """Matches of ``pattern`` that begin an identifier and do not follow
    ``forbidden`` (a leading lookbehind would disable the regex prefix scan)."""
    for match in pattern.finditer(code):
        before = code[match.start() - 1] if match.start() else " "
        if not (before.isalnum() or before == "_" or before in forbidden):
            yield match


@dataclass(frozen=True)
class Source:
    path: Path
    # Production code: comments, literals and test items blanked.
    code: str
    # String literals outside test items.
    literals: list[Literal]


def parse_source(path: Path, text: str) -> tuple[Source, list[tuple[str, str | None]], bool]:
    code, literals = lex(text)
    pruned = strip_tests(text, code)
    live = [literal for literal in literals if pruned.keeps(literal.start)]
    return Source(path, pruned.code, live), pruned.test_modules, pruned.test_only


def module_children(file: Path) -> Path:
    """The directory a file's out-of-line modules live in."""
    if file.name in ("lib.rs", "main.rs", "mod.rs") or file.parent.name == "bin":
        return file.parent
    return file.parent / file.stem


def production_sources(
    directory: Path, exempt_dirs: list[str], root: Path
) -> list[Source]:
    """The package's shipped sources: the files a production build compiles,
    without the items only tests compile."""
    nested = {
        path.parent for path in directory.rglob("Cargo.toml") if path.parent != directory
    }
    candidates = []
    for path in sorted(directory.rglob("*.rs")):
        relative = path.relative_to(directory)
        if relative.parts[0] in (*exempt_dirs, "target") or relative == Path("build.rs"):
            continue
        if any(parent in nested for parent in path.parents):
            continue
        if is_test_file(relative):
            continue
        candidates.append(path)
    parsed = []
    excluded: set[Path] = set()
    for path in candidates:
        source, test_modules, test_only = parse_source(
            path, path.read_text(encoding="utf-8", errors="replace")
        )
        if not test_only:
            parsed.append(source)
        for module, path_attribute in test_modules:
            if path_attribute:
                excluded.add((path.parent / path_attribute).resolve())
                continue
            children = module_children(path)
            excluded.add((children / f"{module}.rs").resolve())
            excluded.add((children / module).resolve())
    return [
        source
        for source in parsed
        if not any(
            item == source.path.resolve() or item in source.path.parents
            for item in excluded
        )
    ]


def load_manifest(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def dependency_tables(manifest: dict) -> list[tuple[str, dict]]:
    """Dependency tables that reach shipped code (not dev-dependencies)."""
    tables = []
    for key in ("dependencies", "build-dependencies"):
        if isinstance(manifest.get(key), dict):
            tables.append((key, manifest[key]))
    targets = manifest.get("target", {})
    if isinstance(targets, dict):
        for target, value in targets.items():
            if not isinstance(value, dict):
                continue
            for key in ("dependencies", "build-dependencies"):
                if isinstance(value.get(key), dict):
                    tables.append((f"target.{target}.{key}", value[key]))
    return tables


def key_line(text: str, key: str) -> int:
    """The line of `key` in a Cargo manifest, or the first line naming it."""
    for index, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        if re.match(rf"^{re.escape(key)}\s*[=.]", stripped) or re.match(
            rf'^"{re.escape(key)}"\s*[=.]', stripped
        ):
            return index
    for index, line in enumerate(text.splitlines(), start=1):
        if key in line:
            return index
    return 1


def matches_any(value: str, patterns: list[str]) -> bool:
    return any(fnmatch.fnmatchcase(value, pattern) for pattern in patterns)

"""Shared reading of Rust sources for the repository policy checks."""

import re
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


# --- Lexing ------------------------------------------------------------------


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

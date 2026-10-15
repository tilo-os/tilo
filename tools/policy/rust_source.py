"""Shared reading of Rust sources for the repository policy checks."""

import re
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

#!/usr/bin/env python3
"""The Markdown shape every reference page has.

A reference page is one `# Title`, a lead paragraph, then `##` sections of
`###` items and `####` of a class's members. Signatures and command lines are
fenced blocks; their info string names the language and carries `fragment`,
which tells the docs-examples check they are not a whole program to run.

Every generator writes its page through here, so the pages cannot drift apart
in shape. Each generator is a command of its own:

    <generator> --root <repository> --out <page.md>
"""

from __future__ import annotations

import argparse
import inspect
import re
import sys
from pathlib import Path
from typing import Callable


# The languages a reference page fences: the SDKs' own, commands, and data.
LANGUAGES = frozenset({"python", "typescript", "sh", "json"})
# How a source's own name for a language is written on a page.
ALIASES = {"ts": "typescript", "js": "typescript", "bash": "sh", "shell": "sh", "py": "python"}
FENCE = re.compile(r"^ {0,3}(`{3,})(?P<info>.*)$")
HEADING = re.compile(r"^ {0,3}(#{1,6})(?:[ \t]+(.*))?$")


class ReferenceError(Exception):
    """What a generator refuses to write a page for."""


def heading(level: int, text: str) -> str:
    """A heading whose text is plain: anchors are slugs of exactly this text."""
    if not 1 <= level <= 6:
        raise ReferenceError(f"there is no heading level {level}")
    if not text or "\n" in text:
        raise ReferenceError(f"a heading needs one line of text, not {text!r}")
    if set(text) & set("`[*"):
        raise ReferenceError(f"heading {text!r}: keep it plain words, no markup")
    return f"{'#' * level} {text}"


def prose(text: str) -> str:
    """A docstring as Markdown prose: dedented, its example blocks fragments.

    Docstrings are wrapped for the source's width; Markdown joins the lines of
    a paragraph, so they are kept as written. A fenced block inside one is an
    illustration, never a program the docs check would run: it is marked
    `fragment`.
    """
    if not text or not text.strip():
        raise ReferenceError("a documented item has no text")
    marked: list[str] = []
    fence: str | None = None
    for line in inspect.cleandoc(text).split("\n"):
        opening = FENCE.match(line)
        if fence is None and opening:
            fence = opening.group(1)
            info = opening.group("info").strip()
            language, _, rest = info.partition(" ")
            language = ALIASES.get(language, language)
            if language and language not in LANGUAGES:
                raise ReferenceError(f"a docstring fences {language!r}, which no page fences")
            words = [word for word in rest.split() if word != "fragment"]
            if language:
                words.append("fragment")
            line = " ".join([fence + language, *words])
        elif fence is not None and opening:
            closing = not opening.group("info").strip()
            if closing and opening.group(1)[0] == fence[0]:
                fence = None
        marked.append(line)
    if fence is not None:
        raise ReferenceError("a docstring opens a code fence and never closes it")
    return "\n".join(marked).strip()


def code(language: str, body: str) -> str:
    """A fenced block that is read, never run: a signature or a command."""
    language = ALIASES.get(language, language)
    if language not in LANGUAGES:
        raise ReferenceError(f"{language!r} is not a language a reference page fences")
    if not body.strip():
        raise ReferenceError("a fenced block needs something in it")
    if "```" in body:
        raise ReferenceError("a fenced block cannot contain a fence of its own")
    return f"```{language} fragment\n{body.strip(chr(10))}\n```"


def table(headers: tuple[str, ...], rows: list[tuple[str, ...]]) -> str:
    """A table; a cell's newlines become spaces and its pipes are escaped."""
    if not headers or any(not header for header in headers):
        raise ReferenceError("a table needs a name for every column")
    for row in rows:
        if len(row) != len(headers):
            raise ReferenceError(f"table row {row!r} does not match {headers!r}")

    def cell(value: str) -> str:
        return " ".join(value.split()).replace("|", "\\|")

    lines = ["| " + " | ".join(headers) + " |", "|" + "|".join(" --- " for _ in headers) + "|"]
    lines.extend("| " + " | ".join(cell(value) for value in row) + " |" for row in rows)
    return "\n".join(lines)


def page(title: str, lead: str, sections: list[str]) -> str:
    """The whole page, checked: one H1, every block a fragment with a language."""
    text = "\n\n".join([heading(1, title), prose(lead), *sections]) + "\n"
    check(text)
    return text


def check(text: str) -> None:
    """Fail on a page with a second H1 or a block the docs check would run."""
    fence: str | None = None
    headings = 0
    for line in text.split("\n"):
        opening = FENCE.match(line)
        if fence is None:
            level = heading_level(line)
            headings += level == 1
            if opening:
                fence = opening.group(1)
                info = opening.group("info").strip().split()
                if not info or info[0] not in LANGUAGES:
                    raise ReferenceError(f"a fenced block names no language: {line!r}")
                if "fragment" not in info[1:]:
                    raise ReferenceError(f"a fenced block is not marked fragment: {line!r}")
        elif opening:
            if not opening.group("info").strip() and opening.group(1)[0] == fence[0]:
                fence = None
    if fence is not None:
        raise ReferenceError("the page opens a code fence and never closes it")
    if headings != 1:
        raise ReferenceError(f"a page has exactly one H1, not {headings}")


def heading_level(line: str) -> int:
    match = HEADING.match(line)
    return len(match.group(1)) if match else 0


def arguments(description: str, extra: tuple[tuple[str, str], ...] = ()) -> argparse.Namespace:
    """`--root` and `--out`, which every generator takes, and any `extra` path."""
    parser = argparse.ArgumentParser(description=description, allow_abbrev=False)
    parser.add_argument("--root", required=True, type=Path, help="the repository to read")
    parser.add_argument("--out", required=True, type=Path, help="the page to write")
    for flag, help_text in extra:
        parser.add_argument(flag, type=Path, help=help_text)
    return parser.parse_args()


def main(
    generate: Callable[[argparse.Namespace], str],
    description: str,
    extra: tuple[tuple[str, str], ...] = (),
) -> int:
    """A generator's entry point: its page, or one line saying why there is none."""
    parsed = arguments(description, extra)
    try:
        text = generate(parsed)
        parsed.out.parent.mkdir(parents=True, exist_ok=True)
        parsed.out.write_text(text, encoding="utf-8")
        return 0
    except ReferenceError as error:
        print(f"{Path(sys.argv[0]).name}: {error}", file=sys.stderr)
        return 1

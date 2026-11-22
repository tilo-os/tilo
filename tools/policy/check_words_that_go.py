#!/usr/bin/env python3
"""The words docs/architecture/core.md says go get no new names.

Each row of that page's "Words that go" table names a concept the design does
not have. `words_that_go.toml` lists, for each row, the identifiers and text
that spell it, and `words_that_go_baseline.json` the names that hold them today.
This check reads the code again (Rust, tilolang, TypeScript, JavaScript and
Python; never vendored code or docs, never comments, and inside a string only
what is shaped like an identifier, `held_by` but not the prose word "held") and
fails when a row's specific words appear in a name the baseline does not list:
new code named a concept that goes. More calls to a name that already exists,
or code moved from one file to another, are printed but never fail. A row that
counts the lines of some files instead of names fails when those files grow.

A row's generic words (`record`, `kind`, `document`, `held`...) are too common
in ordinary code to judge by name, so they are counted and printed but never
fail. Nothing that shrinks fails: `--update` drops the names that are gone and
lowers a line count, and never adds a name or raises a count. The check also
fails when the table and `words_that_go.toml` no longer name the same rows.

A name goes when the code that holds the concept is deleted. A rename alone
takes the word out of the code without removing the concept, so it is not a way
to lower a row.

    python3 tools/policy/check_words_that_go.py            # check
    python3 tools/policy/check_words_that_go.py --print    # every row's names and uses
    python3 tools/policy/check_words_that_go.py --update   # drop the names that are gone
"""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tomllib
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORDS = ROOT / "tools/policy/words_that_go.toml"
BASELINE = ROOT / "tools/policy/words_that_go_baseline.json"
SPEC = ROOT / "docs/architecture/core.md"
SELF = "tools/policy/check_words_that_go.py"
SKIPPED_TOPS = {"vendor", "docs"}
SKIPPED_PARTS = {"target", "node_modules", "vendor", "__pycache__", ".git"}

C_LIKE_STRINGS = r'''(?<![A-Za-z0-9_])r(#*)"[\s\S]*?"\1|"(?:\\.|[^"\\])*"'''
LEXERS = {
    # Rust and tilolang: a quote around one character is a character, not a string.
    "rust": re.compile(
        r"//[^\n]*|/\*[\s\S]*?\*/|" + C_LIKE_STRINGS + r"|'(?:\\.|[^'\\\n])'"
    ),
    "js": re.compile(
        r"//[^\n]*|/\*[\s\S]*?\*/|"
        r'"(?:\\.|[^"\\\n])*"|' + r"'(?:\\.|[^'\\\n])*'|`(?:\\.|[^`\\])*`"
    ),
    "python": re.compile(
        r'#[^\n]*|"""[\s\S]*?"""|' + r"'''[\s\S]*?'''|"
        r'"(?:\\.|[^"\\\n])*"|' + r"'(?:\\.|[^'\\\n])*'"
    ),
}
COMMENT_STARTS = ("//", "/*", "#")
LANGUAGES = {
    ".rs": "rust",
    ".tl": "rust",
    ".ts": "js",
    ".tsx": "js",
    ".js": "js",
    ".mjs": "js",
    ".cjs": "js",
    ".py": "python",
}
IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
WORD = re.compile(r"[A-Z]?[a-z]+[0-9]*|[A-Z]+[0-9]*(?![a-z])|[0-9]+")


def sources(root: Path) -> list[Path]:
    """The code files of the tree: the index's when there is one, else the disk's."""
    try:
        listed = subprocess.run(
            ["git", "ls-files", "-z"],
            cwd=root,
            check=True,
            capture_output=True,
        ).stdout.decode()
        paths = [Path(path) for path in listed.split("\0") if path]
    except (OSError, subprocess.CalledProcessError):
        paths = []
        for directory, names, files in os.walk(root):
            names[:] = [name for name in names if name not in SKIPPED_PARTS]
            base = Path(directory).relative_to(root)
            paths.extend(base / name for name in files)
    return [
        path
        for path in paths
        if path.suffix in LANGUAGES
        and path.parts[0] not in SKIPPED_TOPS
        and not SKIPPED_PARTS.intersection(path.parts)
        and path.as_posix() != SELF
        and (root / path).is_file()
    ]


def code(text: str, language: str) -> tuple[str, list[str]]:
    """`text` without its comments, and the identifiers its strings name.

    The text keeps its strings, for the exact text a row counts. A string's
    identifiers are those with more than one word (`held_by`, `HeldWorkspace`):
    a plain word in a string is prose, a message or a test's title.
    """
    named: list[str] = []

    def keep(match: re.Match[str]) -> str:
        found = match.group(0)
        if found.startswith(COMMENT_STARTS):
            return " "
        named.extend(
            identifier
            for identifier in IDENT.findall(found)
            if len(WORD.findall(identifier)) > 1
        )
        return found

    body = LEXERS[language].sub(keep, text)
    outside = LEXERS[language].sub(" ", text)
    return body, IDENT.findall(outside) + named


def words(identifier: str) -> tuple[str, ...]:
    return tuple(word.lower() for word in WORD.findall(identifier))


def holds(identifier: tuple[str, ...], term: tuple[str, ...]) -> bool:
    """Whether `term` is a run of `identifier`'s words, its last word maybe plural."""
    *head, last = term
    width = len(term)
    for start in range(len(identifier) - width + 1):
        window = identifier[start : start + width]
        if list(window[:-1]) == head and window[-1] in (last, last + "s", last + "es"):
            return True
    return False


def load_rows() -> list[dict]:
    return tomllib.loads(WORDS.read_text(encoding="utf-8"))["word"]


def spec_rows() -> list[str]:
    """The first cells of the "Words that go" table, as plain text."""
    text = SPEC.read_text(encoding="utf-8")
    section = text.split("## Words that go", 1)[1].split("\n## ", 1)[0]
    rows = []
    for line in section.splitlines():
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if not line.startswith("|") or len(cells) < 3 or set(cells[0]) <= {"-"}:
            continue
        if cells[0] == "Word in the code":
            continue
        rows.append(cells[0].replace("`", ""))
    return rows


@dataclass
class Row:
    """What the code holds of one row."""

    names: set[str]
    uses: int
    lines: int
    generic: int


def count(root: Path, rows: list[dict]) -> dict[str, Row]:
    identifiers: Counter[str] = Counter()
    texts: Counter[str] = Counter()
    literals = sorted({text for row in rows for text in row.get("text", [])})
    prefixes = sorted({prefix for row in rows for prefix in row.get("paths", [])})
    lines: Counter[str] = Counter()
    for path in sources(root):
        raw = (root / path).read_text(encoding="utf-8", errors="replace")
        body, named = code(raw, LANGUAGES[path.suffix])
        identifiers.update(named)
        for literal in literals:
            texts[literal] += body.count(literal)
        for prefix in prefixes:
            if path.as_posix().startswith(prefix):
                lines[prefix] += sum(1 for line in body.splitlines() if line.strip())
    split = {identifier: words(identifier) for identifier in identifiers}

    def holding(terms: list[str]) -> set[str]:
        runs = [tuple(term.split()) for term in terms if not term.startswith("=")]
        whole = {tuple(term[1:].split()) for term in terms if term.startswith("=")}
        return {
            identifier
            for identifier in identifiers
            if split[identifier] in whole or any(holds(split[identifier], run) for run in runs)
        }

    counts = {}
    for row in rows:
        names = holding(row.get("idents", []))
        names |= {literal for literal in row.get("text", []) if texts[literal]}
        uses = sum(identifiers[name] for name in names if name in identifiers)
        uses += sum(texts[literal] for literal in row.get("text", []))
        counts[row["row"]] = Row(
            names=names,
            uses=uses,
            lines=sum(lines[prefix] for prefix in row.get("paths", [])),
            generic=sum(identifiers[name] for name in holding(row.get("generic", []))),
        )
    return counts


def main(argv: list[str]) -> int:
    rows = load_rows()
    named = [row["row"] for row in rows]
    spec = spec_rows()
    problems = [
        f"{SPEC.relative_to(ROOT)}: row '{row}' is not in {WORDS.relative_to(ROOT)}"
        for row in spec
        if row not in named
    ] + [
        f"{WORDS.relative_to(ROOT)}: row '{row}' is not in {SPEC.relative_to(ROOT)}'s table"
        for row in named
        if row not in spec
    ]
    counts = count(ROOT, rows)
    baseline = json.loads(BASELINE.read_text(encoding="utf-8")) if BASELINE.exists() else {}
    if "--print" in argv:
        print(f"{'names':>6} {'uses':>7} {'lines':>6} {'generic':>8}  row")
        for row in named:
            now = counts[row]
            print(f"{len(now.names):>6} {now.uses:>7} {now.lines:>6} {now.generic:>8}  {row}")
        return 0
    if "--update" in argv:
        written = {}
        for row in named:
            now, was = counts[row], baseline.get(row)
            names = now.names if was is None else now.names & set(was["names"])
            lines = now.lines if was is None else min(now.lines, was["lines"])
            written[row] = {"names": sorted(names), "lines": lines}
        BASELINE.write_text(json.dumps(written, indent=1) + "\n", encoding="utf-8")
        print(f"wrote {BASELINE.relative_to(ROOT)}", file=sys.stderr)
        return 0
    for row in named:
        now, was = counts[row], baseline.get(row)
        if was is None:
            problems.append(f"{BASELINE.relative_to(ROOT)}: no entry for '{row}'")
            continue
        new = sorted(now.names - set(was["names"]))
        if new:
            problems.append(f"'{row}': new names {', '.join(new)}")
        if now.lines > was["lines"]:
            problems.append(
                f"'{row}': {now.lines} lines, {now.lines - was['lines']} more than the"
                f" baseline's {was['lines']}"
            )
    for problem in problems:
        print(problem)
    if problems:
        print(
            "words that go: new code named a concept core.md's table says goes, or the rows"
            " differ. Name it with the word the table has instead. A name that is gone is"
            " dropped from the baseline with `--update`.",
            file=sys.stderr,
        )
        return 1
    print(f"words that go: {len(named)} rows hold no new names", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

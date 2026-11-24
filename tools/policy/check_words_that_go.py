#!/usr/bin/env python3
"""The words docs/architecture/core.md says go get no new names.

Each row of that page's "Words that go" table names a concept the design does
not have. `words_that_go.toml` lists, for each row, the identifiers and text
that spell it, and `words_that_go_baseline.json` the names that hold them today.
This check reads the code again (Rust, tilolang, TypeScript, JavaScript and
Python; never vendored code or docs, never comments, and inside a string only
what is shaped like an identifier, `held_by` but not the prose word "held"; a
row's exact text counts only as a whole name, so `tilo.member` is not found
inside `tilo.membership`) and
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
    python3 tools/policy/check_words_that_go.py --staged   # only the names this commit adds
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

sys.path.insert(0, str(Path(__file__).resolve().parent))
from judged import STAGED_FLAG, git, judged  # noqa: E402

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
NAME_CHAR = r"[A-Za-z0-9_]"


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


def name_uses(text: str, literal: str) -> int:
    """How many whole names `literal` is in `text`.

    A name is a run of identifier characters; a separator (`.`, `-`, `/`, a
    quote) ends one. `tilo.member` inside `tilo.membership` begins `tilo`'s
    name and then continues another segment, so it is that other name, not
    this one. A boundary is required only on a side where `literal` itself
    ends in an identifier character, so a literal ending in a separator
    (`tilo-personal-store/`) still finds the path it begins.
    """
    left = rf"(?<!{NAME_CHAR})" if re.match(NAME_CHAR, literal) else ""
    right = rf"(?!{NAME_CHAR})" if re.search(rf"{NAME_CHAR}$", literal) else ""
    return len(re.findall(left + re.escape(literal) + right, text))


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
            texts[literal] += name_uses(body, literal)
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


def judged_sources(root: Path, staged) -> list[tuple[Path, str]]:
    """The files a commit stages that this check judges, in path order."""
    return [
        (relative, text)
        for relative, text in staged.sources(suffixes=tuple(LANGUAGES))
        if relative.parts
        and relative.parts[0] not in SKIPPED_TOPS
        and not SKIPPED_PARTS.intersection(relative.parts)
        and relative.as_posix() != SELF
    ]


def held_by_the_tree(baseline: dict) -> set[str]:
    """The names a whole-tree pass last accepted, as its baseline records them.

    The staged run answers "did the tree hold this name before this commit" from
    the baseline rather than by searching HEAD: the whole-tree run passes only
    while every name of the tree's rows is listed there, and `--update` records
    exactly those names, the row's text literals among them. A name the tree
    gained since the last `--update` is one the whole-tree run is already red on,
    and a commit that uses it is refused until that run records it, which is what
    the red asks for. No commit costs a walk over HEAD: the tree around it was
    judged by the run that wrote the baseline.
    """
    held: set[str] = set()
    for row in baseline.values():
        if isinstance(row, dict):
            held.update(name for name in row.get("names", ()) if isinstance(name, str))
    return held


def head_texts(
    root: Path, relatives: list[Path], renamed_from=lambda relative: relative
) -> dict[Path, str]:
    """The text each of `relatives` held in the tree before this commit: a moved file's is
    the text HEAD holds under its old name (`renamed_from`), so a move adds no names."""
    wanted = [relative for relative in relatives if "\n" not in relative.as_posix()]
    if not wanted:
        return {}
    # One `git cat-file` for the lot: a commit of many files costs one process.
    output = git(
        root,
        "cat-file",
        "--batch",
        input=b"".join(
            b"HEAD:" + os.fsencode(renamed_from(relative).as_posix()) + b"\n"
            for relative in wanted
        ),
        empty=(128,),
    )
    texts = {}
    offset = 0
    for relative in wanted:
        end = output.index(b"\n", offset)
        header = output[offset:end].split()
        offset = end + 1
        if len(header) != 3:
            continue
        _oid, kind, size = header
        if kind != b"blob":
            continue
        length = int(size)
        texts[relative] = output[offset : offset + length].decode("utf-8", errors="replace")
        offset += length + 1
    return texts


def staged_problems(root: Path, rows: list[dict], held: set[str]) -> list[str]:
    """The row names a commit adds: ones neither its own files nor the tree held.

    `held` is what the tree already held ([`held_by_the_tree`]), so a commit is
    charged only with what it introduces. Nothing here walks HEAD or the tree:
    the commit's own texts come from the index, their earlier ones from HEAD in
    one `git cat-file`, and the rest from the baseline.
    """
    staged = judged(root, staged=True)
    literals = sorted({text for row in rows for text in row.get("text", [])})
    added: Counter[str] = Counter()
    added_texts: Counter[str] = Counter()
    sources = judged_sources(root, staged)
    before_texts = head_texts(
        root, [relative for relative, _ in sources], staged.renamed_from
    )
    for relative, text in sources:
        language = LANGUAGES[relative.suffix]
        body, named = code(text, language)
        before = before_texts.get(relative, "")
        if before:
            before_body, before_named = code(before, language)
        else:
            before_body, before_named = "", []
        added.update(Counter(named) - Counter(before_named))
        for literal in literals:
            newer = name_uses(body, literal) - name_uses(before_body, literal)
            if newer > 0:
                added_texts[literal] += newer
    split = {identifier: words(identifier) for identifier in added}

    def holding(terms: list[str]) -> set[str]:
        runs = [tuple(term.split()) for term in terms if not term.startswith("=")]
        whole = {tuple(term[1:].split()) for term in terms if term.startswith("=")}
        return {
            identifier
            for identifier in added
            if split[identifier] in whole or any(holds(split[identifier], run) for run in runs)
        }

    # Only a name that holds a row's words can be reported, and one the tree already held is
    # not new: both answers come from the baseline, never from a walk over HEAD, so a commit
    # costs the same whether the tree around it is large or small.
    rejected = {row["row"]: holding(row.get("idents", [])) for row in rows}
    problems = []
    for row in rows:
        names = rejected[row["row"]] - held
        names |= {text for text in row.get("text", []) if added_texts[text] and text not in held}
        if names:
            problems.append(f"'{row['row']}': new names {', '.join(sorted(names))}")
    return problems


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
    baseline = json.loads(BASELINE.read_text(encoding="utf-8")) if BASELINE.exists() else {}
    if STAGED_FLAG in argv:
        problems += staged_problems(ROOT, rows, held_by_the_tree(baseline))
        for problem in problems:
            print(problem)
        if problems:
            print(
                "words that go: this commit names a concept core.md's table says goes. Name it"
                " with the word the table has instead.",
                file=sys.stderr,
            )
            return 1
        print("words that go: this commit adds no name the table rejects", file=sys.stderr)
        return 0
    counts = count(ROOT, rows)
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

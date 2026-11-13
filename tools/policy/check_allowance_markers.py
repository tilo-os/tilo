#!/usr/bin/env python3
"""A reviewed allowance marker goes only with the code it covers.

Three checks let a count fall but never grow, and read a comment as the review
of a hit they would otherwise count:

* `// ratchet-allow(<pattern>[, <pattern>...]): <why>` on the line above the
  hit, naming the patterns it allows: the design-token, spacing, typography and
  copy ratchets (`crates/sdk/ui-core/tests/*_audit.rs`, engine in
  `tests/support/ratchet.rs`);
* `allow-breakpoint: <why>` and `allow-socket-path: <why>`, trailing the line
  whose hit they excuse (`check_breakpoints.py`, `check_socket_paths.py`).

A marker is therefore data the checks read, not prose. Trim the comment away
and the hit it allowed becomes a count the baseline does not hold, so the
policy suite or `cargo test -p tilo-ui` goes red in a run far from the commit
that lost the marker.

So a marker may go only together with the code it covers:

* the commit deletes or changes that line (the hit is gone, or now reads a
  token or the shared breakpoint), or
* for a ratchet marker, the pattern it names is no longer declared by an audit
  (the marker was dead).

Keeping the code line and dropping the marker is what this refuses.

The marker spellings are the checks' own: the inline patterns are imported
from `check_breakpoints.py` and `check_socket_paths.py`, and the ratchet's
grammar (`support::ratchet::allow_marker`) is mirrored here, with a test of the
suite failing as soon as a marker of the tree stops parsing.

Three things run it, which is what keeps a trimming pass from landing:

``staged``, in the pre-commit hook: the tree being committed against HEAD's.
``check_commit_policy.py``, in the lint job, on every new commit of a change
against its parents' exact blobs: the check a ``--no-verify`` commit still
meets.
``against BASE`` by hand, when a marker went some commits back and the commit
that did it is not in the range ``before``..``head`` names.
"""

from __future__ import annotations

import argparse
from collections import Counter, defaultdict
from dataclasses import dataclass
from pathlib import Path
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_breakpoints import ALLOW as BREAKPOINT_ALLOW  # noqa: E402
from check_socket_paths import ALLOW as SOCKET_PATH_ALLOW  # noqa: E402
from judged import git  # noqa: E402
from rust_source import blank_test_items, is_test_file, lex  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
# The marker on the line above the hit, and the patterns it names.
RATCHET = "ratchet-allow"
MARKER = "// " + RATCHET + "("
# The shortest reason the ratchet counts as one: a marker with less allows
# nothing, so removing it takes no allowance away.
MIN_REASON = 12
# Where the audits declare the patterns a marker can name.
AUDITS = Path("crates/sdk/ui-core/tests")
DECLARED = re.compile(r'Pattern::new\(\s*"([A-Za-z0-9_]+)"')
# The markers trailing the line they excuse, by the check that reads them.
INLINE = {
    "allow-breakpoint": BREAKPOINT_ALLOW,
    "allow-socket-path": SOCKET_PATH_ALLOW,
}
SCOPES = ("crates/", "apps/")
BLANKS = re.compile(r"\s+")


@dataclass(frozen=True)
class Allowance:
    """One hit a marker lets through: a marker, and the code it covers."""

    family: str  # which kind of marker
    marker: int  # 0-based line of the marker
    index: int  # 0-based line of the code it covers
    key: str  # the pattern a ratchet marker names, empty for an inline one
    code: str  # that code line, without indentation and comments


def marker_keys(line: str) -> tuple[str, ...] | None:
    """The patterns `line` allows on the next line, or None if it is no marker."""
    rest = line.lstrip()
    if not rest.startswith(MARKER):
        return None
    rest = rest[len(MARKER) :]
    keys, separator, reason = rest.partition("):")
    if not separator:
        return None
    names = tuple(key.strip() for key in keys.split(",") if key.strip())
    return names if names and len(reason.strip()) >= MIN_REASON else None


def in_scope(path: str) -> bool:
    """Whether `path` is a file the checks read (production Rust sources)."""
    return (
        path.endswith(".rs")
        and path.startswith(SCOPES)
        and not path.startswith("vendor/")
        and not is_test_file(Path(path))
    )


def _code(text: str) -> str:
    """`text` as code: its comments gone, its literals as written, spaces evened."""
    blanked, literals = lex(text)
    pieces = list(blanked)
    for literal in literals:
        pieces[literal.start : literal.end] = text[literal.start : literal.end]
    return BLANKS.sub(" ", "".join(pieces)).strip()


def _test_items(lines: list[str]) -> set[int]:
    """The 0-based lines inside a `#[cfg(test)]` item: no check counts a hit there."""
    blanked = blank_test_items("\n".join(lines)).splitlines()
    return {
        index
        for index, (line, blank) in enumerate(zip(lines, blanked))
        if line.strip() and not blank.strip()
    }


def allowances(lines: list[str]) -> list[Allowance]:
    """Every marker of `lines`, with the code it covers.

    A ratchet marker is read as the ratchet reads it (`Ratchet::count`): it
    allows the next line that is neither a marker nor a comment, and a second
    marker above a line replaces the first, so only the lowest of stacked
    markers allows anything. An inline marker allows the code of its own line.
    A marker over a `#[cfg(test)]` item allows nothing: no check counts there.
    """
    found: list[Allowance] = []
    test_items = _test_items(lines)
    keys: tuple[str, ...] = ()
    marker = 0
    for index, line in enumerate(lines):
        names = marker_keys(line)
        if names is not None:
            keys, marker = names, index
            continue
        for family, pattern in INLINE.items():
            inline = pattern.search(line)
            if inline is None:
                continue
            code = _code(line[: inline.start()])
            # A marker in prose allows nothing, and a comment changes freely.
            if code and not code.startswith("//") and index not in test_items:
                found.append(Allowance(family, index, index, "", code))
        if line.lstrip().startswith("//"):
            continue
        if keys and index not in test_items:
            found.extend(
                Allowance(RATCHET, marker, index, key, line.strip()) for key in keys
            )
        keys = ()
    return found


def _group(allowance: Allowance) -> tuple[str, str, str]:
    return allowance.family, allowance.key, allowance.code


def _problem(path: str, line: int, allowance: Allowance) -> str:
    if allowance.family == RATCHET:
        return (
            f"{path}:{line}: this line's `{allowance.key}` allowance is gone -- its "
            f"`// {RATCHET}({allowance.key})` marker was removed while the line stayed. "
            "The ratchets read the marker as the review that this hit is content, so the "
            "count grows and `cargo test -p tilo-ui` fails on it in a later run. Keep the "
            "marker, or delete or change the line it covers in this commit."
        )
    return (
        f"{path}:{line}: this line's `{allowance.family}: <why>` marker is gone while the "
        "line stayed. Its check counts what the marker excuses, so the count grows and the "
        "policy suite fails on it in a later run. Keep the marker, or delete or change the "
        "line in this commit."
    )


def diff_problems(
    path: str, before: str, after: str, declared: set[str]
) -> list[str]:
    """What `before` allowed on a line that `after` still has, and no longer allows."""
    after_lines = after.splitlines()
    after_tests = _test_items(after_lines)
    allowed_at: dict[int, set[str]] = defaultdict(set)
    remaining: Counter[tuple[str, str, str]] = Counter()
    for allowance in allowances(after_lines):
        if allowance.family == RATCHET:
            allowed_at[allowance.index].add(allowance.key)
        remaining[_group(allowance)] += 1
    problems = []
    for allowance in allowances(before.splitlines()):
        group = _group(allowance)
        if remaining[group]:
            remaining[group] -= 1
            continue
        if allowance.family == RATCHET:
            if allowance.key not in declared or not allowance.code:
                continue
            survivors = [
                index
                for index, line in enumerate(after_lines)
                if line.strip() == allowance.code
                and index not in after_tests
                and allowance.key not in allowed_at[index]
            ]
        else:
            pattern = INLINE[allowance.family]
            survivors = [
                index
                for index, line in enumerate(after_lines)
                if index not in after_tests
                and _code(line).startswith(allowance.code)
                and not pattern.search(line)
            ]
        if not survivors:
            continue
        nearest = min(survivors, key=lambda index: abs(index - allowance.marker))
        problems.append(_problem(path, nearest + 1, allowance))
    return problems


def declared_keys(root: Path | None = None) -> set[str]:
    """The pattern keys the audit sources declare, in `root`'s tree.

    A tree with no ratchet at all (a fixture repository) declares none, and its
    markers allow nothing. Sources that are there and declare none have stopped
    being read, which would silently call every marker dead, so that fails.
    """
    root = root or ROOT
    if not (root / AUDITS).is_dir():
        return set()
    keys: set[str] = set()
    for source in sorted((root / AUDITS).rglob("*.rs")):
        keys |= set(
            DECLARED.findall(source.read_text(encoding="utf-8", errors="replace"))
        )
    if not keys:
        raise ValueError(
            f"{AUDITS} declares no `Pattern::new(\"key\")`; the check cannot see "
            "which allowances are dead, so it would judge nothing"
        )
    return keys


def _changed(root: Path, span: list[str]) -> tuple[list[str], dict[str, str]]:
    """The in-scope paths the diff `span` changes, and its renames (before: after)."""
    changed = [
        path
        for path in git(root, "diff", "--name-only", "-M", *span, "--")
        .decode(errors="replace")
        .splitlines()
        if in_scope(path)
    ]
    renamed: dict[str, str] = {}
    for line in (
        git(root, "diff", "--name-status", "-M", *span, "--")
        .decode(errors="replace")
        .splitlines()
    ):
        parts = line.split("\t")
        if parts[0].startswith("R") and len(parts) == 3 and in_scope(parts[1]):
            renamed[parts[1]] = parts[2]
            changed.append(parts[1])
    return sorted(set(changed)), renamed


def _text(root: Path, spec: str) -> str | None:
    """The text `spec` (`<revision>:<path>`, or `:<path>` for the index) holds."""
    try:
        return git(root, "show", spec).decode(errors="replace")
    except subprocess.CalledProcessError:
        return None


def staged(root: Path | None = None) -> list[str]:
    """What the markers of `root`'s index lose of HEAD's tree."""
    root = root or ROOT
    changed, renamed = _changed(root, ["--cached"])
    declared = declared_keys(root)
    problems = []
    for path in changed:
        before = _text(root, f"HEAD:{path}")
        if before is None:
            # A file HEAD does not hold lost no allowance it had.
            continue
        target = renamed.get(path, path)
        after = _text(root, f":{target}") or ""
        problems.extend(diff_problems(target, before, after, declared))
    return problems


def against(base: str, root: Path | None = None, head: str | None = None) -> list[str]:
    """What the markers of `head`'s tree (or this working tree) lose of `base`, from
    where the two meet: a marker `base` gained after `head` branched was never
    `head`'s to keep."""
    root = root or ROOT
    base = git(root, "merge-base", base, head or "HEAD").decode().strip()
    span = [base, head] if head else [base]
    changed, renamed = _changed(root, span)
    declared = declared_keys(root)
    problems = []
    for path in changed:
        before = _text(root, f"{base}:{path}")
        if before is None:
            continue
        target = renamed.get(path, path)
        if head:
            after = _text(root, f"{head}:{target}")
        else:
            current = root / target
            after = (
                current.read_text(encoding="utf-8", errors="replace")
                if current.is_file()
                else None
            )
        problems.extend(diff_problems(target, before, after or "", declared))
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = parser.add_subparsers(dest="command", required=True)
    base = sub.add_parser("against", help="a revision, against its tree or HEAD's")
    base.add_argument("base")
    sub.add_parser("staged", help="this index, against HEAD")
    args = parser.parse_args(argv)
    try:
        problems = against(args.base) if args.command == "against" else staged()
    except (ValueError, subprocess.CalledProcessError) as error:
        print(f"::error::allowance marker check failed: {error}", file=sys.stderr)
        return 1
    for problem in problems:
        print(f"::error::{problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())

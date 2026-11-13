#!/usr/bin/env python3
"""A reviewed allowance marker goes only with the code it covers.

Seven checks let something through, or keep something out of the way, and read a
comment as the review of what they would otherwise flag:

* `// ratchet-allow(<pattern>[, <pattern>...]): <why>` on the line above the
  hit, naming the patterns it allows: the design-token, spacing, typography and
  copy ratchets (`crates/sdk/ui-core/tests/*_audit.rs`, engine in
  `tests/support/ratchet.rs`);
* `allow-breakpoint: <why>` and `allow-socket-path: <why>`, trailing the line
  whose hit they excuse (`check_breakpoints.py`, `check_socket_paths.py`);
* `/// Quiet: <why>`, the doc comment above a headless harness entry point that
  cannot leave a frame stale (`crates/sdk/ui-core/src/headless/quiet_frame_tests.rs`);
* `@generated` in a source's first lines, which keeps a generated file out of
  the size ceiling and out of the formatting check (`check_source_file_size.py`,
  `tools/ci/check-changed-rustfmt.sh`);
* `// tilo-app:graph-props=<contract>` at the head of a generated component,
  which says how its props are read back (`crates/dev/tilo-app-cli`);
* `// tilo-app:locale=<locale>` in a generated registry, which registers the
  catalogue beside it (`check_catalogue_coverage.py`).

A marker is therefore data the checks read, not prose. Trim the comment away
and what it allowed or described is judged as if no one had reviewed it: the
policy suite or `cargo test -p tilo-ui` goes red in a run far from the commit
that lost the marker.

So a marker may go only together with the code it covers:

* the commit deletes or changes that line (the hit is gone, or now reads a
  token, the shared breakpoint, the shared socket helper, an invalidating
  entry point, or the code the file marker described);
* for a ratchet marker, the pattern it names is no longer declared by an audit
  (the marker was dead);
* for any marker, the scan that reads it is gone from the tree, or no longer
  names the marker (nothing reads it any more).

Keeping the code and dropping the marker is what this refuses.

The marker spellings are the checks' own: the inline patterns are imported
from `check_breakpoints.py` and `check_socket_paths.py`, the registration from
`check_catalogue_coverage.py`, and the ratchet's grammar
(`support::ratchet::allow_marker`) is mirrored here, with a test of the suite
failing as soon as a marker of the tree stops parsing.

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
from check_catalogue_coverage import REGISTRATION as LOCALE_REGISTRATION  # noqa: E402
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
# The doc line above the headless entry point whose stillness it explains, read
# by the quiet-frame scan of that crate's own sources.
QUIET = "quiet"
QUIET_MARKER = "Quiet: "
QUIET_SCAN = Path("crates/sdk/ui-core/src/headless/quiet_frame_tests.rs")
QUIET_DECLARATION = 'const QUIET: &str = "' + QUIET_MARKER + '";'
# The marker that keeps a generated source as the generator wrote it, read in
# the first lines of the file by the size and formatting checks.
GENERATED = "generated"
GENERATED_MARKER = "@generated"
GENERATED_LINES = 20
# The markers `tilo-app` writes at the head of the code it generated: the props
# contract a component fulfils, and the locales a registry loads.
GRAPH_PROPS = "tilo-app:graph-props"
GRAPH_PROPS_MARKER = "// " + GRAPH_PROPS + "="
GRAPH_PROPS_DECLARATION = 'const GRAPH_PROPS_MARKER: &str = "// ' + GRAPH_PROPS + '=";'
# The contract `tilo-app` reads a component without a marker as: such a marker
# says nothing a missing one does not, so it covers nothing.
GRAPH_PROPS_DEFAULTS = frozenset({"display-v1"})
LOCALE = "tilo-app:locale"
LOCALE_DECLARATION = LOCALE + "="
# The line of the code a marker heading a whole file covers.
WHOLE_FILE = -1
WHOLE = frozenset({GENERATED, GRAPH_PROPS})

# The trees each marker's readers scan, keyed by family.
SCOPE = {
    RATCHET: ("crates/", "apps/"),
    "allow-breakpoint": ("apps/", "crates/session/"),
    "allow-socket-path": ("apps/", "crates/"),
    QUIET: ("crates/sdk/ui-core/src/",),
    GENERATED: ("crates/", "apps/", "examples/", "tools/"),
    GRAPH_PROPS: ("apps/", "examples/"),
    LOCALE: ("crates/", "apps/", "examples/", "tools/"),
}
FAMILIES = frozenset(SCOPE)
# The scans that read a marker, and the text each holds while it reads one. A
# family none of whose scans names it any more allows nothing, so removing its
# markers takes no allowance away.
READERS = {
    QUIET: ((QUIET_SCAN, QUIET_DECLARATION),),
    GENERATED: (
        (Path("tools/policy/check_source_file_size.py"), GENERATED_MARKER),
        (Path("tools/ci/check-changed-rustfmt.sh"), GENERATED_MARKER),
    ),
    GRAPH_PROPS: ((Path("crates/dev/tilo-app-cli/src/lib.rs"), GRAPH_PROPS_DECLARATION),),
    LOCALE: ((Path("tools/policy/check_catalogue_coverage.py"), LOCALE_DECLARATION),),
}
# What each marker is called in a refusal, and what reads it.
KIND = {
    RATCHET: (
        "`// {RATCHET}({key})`",
        "The ratchets read the marker as the review that this hit is content, so the "
        "count grows and `cargo test -p tilo-ui` fails on it in a later run.",
    ),
    **{
        family: (
            f"`{family}: <why>`",
            "Its check counts what the marker excuses, so the count grows and the "
            "policy suite fails on it in a later run.",
        )
        for family in INLINE
    },
    QUIET: (
        "`/// Quiet: <why>`",
        "The quiet-frame scan reads it as the reason this entry point leaves no frame "
        "stale, so `cargo test -p tilo-ui` fails on the entry point in a later run.",
    ),
    GENERATED: (
        "`@generated`",
        "The size and formatting checks keep a generated source as its generator wrote "
        "it because of the marker, so the file fails one of them in a later run.",
    ),
    GRAPH_PROPS: (
        "`// " + GRAPH_PROPS + "=<contract>`",
        "`tilo-app doctor` reads the marker as the props contract the component "
        "fulfils, so the generated component fails doctor in a later run.",
    ),
    LOCALE: (
        "`// " + LOCALE + "=<locale>`",
        "The catalogue check reads the registrations as the locales the registry "
        "loads, so a catalogue beside it goes unread and the policy suite fails on it "
        "in a later run.",
    ),
}
BLANKS = re.compile(r"\s+")


@dataclass(frozen=True)
class Allowance:
    """One hit a marker lets through: a marker, and the code it covers."""

    family: str  # which kind of marker
    marker: int  # 0-based line of the marker
    index: int  # 0-based line of the code it covers, or WHOLE_FILE
    key: str  # the pattern a ratchet marker names, or the key a file marker names
    # The code line without indentation and comments; for a marker heading the
    # file, the file's body with its marker lines left out.
    code: str


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


def in_scope(path: str, families: frozenset[str] = FAMILIES) -> bool:
    """Whether `path` is a source that one of `families` reads."""
    return (
        path.endswith(".rs")
        and not path.startswith("vendor/")
        and not is_test_file(Path(path))
        and any(path.startswith(SCOPE[family]) for family in families)
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


def _body(lines: list[str], marker: str) -> str:
    """`lines` as code, the marker lines left out: what a file marker covers."""
    return "\n".join(_code(line) for line in lines if marker not in line)


def _item_after(lines: list[str], marker: int) -> int | None:
    """The line of the item a doc comment above `marker` documents: the first
    line below it that is neither blank, a comment, nor an attribute."""
    for index in range(marker + 1, len(lines)):
        rest = lines[index].strip()
        if rest and not rest.startswith(("//", "#[")):
            return index
    return None


def _still_needs_a_reason(lines: list[str], item: int) -> bool:
    """Whether the entry point starting at `item` still needs its `Quiet:` reason:
    the quiet-frame scan reads one only while its body does not open by
    invalidating local state itself."""
    end = item
    while end < len(lines) - 1 and not lines[end].rstrip().endswith(("{", ";")):
        end += 1
    if not lines[end].rstrip().endswith("{"):
        return True
    first = next(
        (
            line.strip()
            for line in lines[end + 1 :]
            if line.strip() and not line.strip().startswith("//")
        ),
        None,
    )
    return first != "self.invalidate_local();"


def _ratchet_allowances(lines: list[str], test_items: set[int]) -> list[Allowance]:
    """The `// ratchet-allow(...)` markers, each over the line it allows.

    A ratchet marker is read as the ratchet reads it (`Ratchet::count`): it
    allows the next line that is neither a marker nor a comment, and a second
    marker above a line replaces the first, so only the lowest of stacked
    markers allows anything.
    """
    found: list[Allowance] = []
    keys: tuple[str, ...] = ()
    marker = 0
    for index, line in enumerate(lines):
        names = marker_keys(line)
        if names is not None:
            keys, marker = names, index
            continue
        if line.lstrip().startswith("//"):
            continue
        if keys and index not in test_items:
            found.extend(
                Allowance(RATCHET, marker, index, key, line.strip()) for key in keys
            )
        keys = ()
    return found


def _inline_allowances(
    lines: list[str], family: str, pattern: re.Pattern[str], test_items: set[int]
) -> list[Allowance]:
    """The markers trailing the line whose hit they excuse."""
    found = []
    for index, line in enumerate(lines):
        inline = pattern.search(line)
        if inline is None:
            continue
        code = _code(line[: inline.start()])
        # A marker in prose allows nothing, and a comment changes freely.
        if code and not code.startswith("//") and index not in test_items:
            found.append(Allowance(family, index, index, "", code))
    return found


def _quiet_allowances(lines: list[str], test_items: set[int]) -> list[Allowance]:
    """The `/// Quiet: <why>` doc lines, each over the item it documents."""
    found = []
    for index, line in enumerate(lines):
        rest = line.lstrip()
        if not rest.startswith("///") or not rest[3:].strip().startswith(QUIET_MARKER):
            continue
        item = _item_after(lines, index)
        if item is None or item in test_items:
            continue
        found.append(Allowance(QUIET, index, item, "", _code(lines[item])))
    return found


def _generated_allowances(lines: list[str]) -> list[Allowance]:
    """The `@generated` marker a generated source carries in its first lines."""
    for index, line in enumerate(lines[:GENERATED_LINES]):
        if GENERATED_MARKER in line:
            return [
                Allowance(GENERATED, index, WHOLE_FILE, "", _body(lines, GENERATED_MARKER))
            ]
    return []


def _graph_props_allowances(lines: list[str]) -> list[Allowance]:
    """The props contract a generated component heads its source with."""
    return [
        Allowance(
            GRAPH_PROPS,
            index,
            WHOLE_FILE,
            line[len(GRAPH_PROPS_MARKER) :].strip(),
            _body(lines, GRAPH_PROPS_MARKER),
        )
        for index, line in enumerate(lines)
        if line.startswith(GRAPH_PROPS_MARKER)
    ]


def _locale_allowances(lines: list[str]) -> list[Allowance]:
    """The locales a generated registry registers with its comment markers."""
    found = []
    for index, line in enumerate(lines):
        match = LOCALE_REGISTRATION.match(line)
        if match is not None:
            found.append(Allowance(LOCALE, index, WHOLE_FILE, match.group(1), ""))
    return found


def allowances(
    lines: list[str], families: frozenset[str] = FAMILIES
) -> list[Allowance]:
    """Every marker of `lines` in `families`, with the code it covers.

    A marker over or inside a `#[cfg(test)]` item allows nothing: no check
    counts a hit there.
    """
    found: list[Allowance] = []
    test_items = _test_items(lines)
    if RATCHET in families:
        found.extend(_ratchet_allowances(lines, test_items))
    for family, pattern in INLINE.items():
        if family in families:
            found.extend(_inline_allowances(lines, family, pattern, test_items))
    if QUIET in families:
        found.extend(_quiet_allowances(lines, test_items))
    if GENERATED in families:
        found.extend(_generated_allowances(lines))
    if GRAPH_PROPS in families:
        found.extend(_graph_props_allowances(lines))
    if LOCALE in families:
        found.extend(_locale_allowances(lines))
    return found


def _group(allowance: Allowance) -> tuple[str, str, str]:
    return allowance.family, allowance.key, allowance.code


def _problem(path: str, line: int, allowance: Allowance) -> str:
    marker, breaks = KIND[allowance.family]
    if allowance.family == RATCHET:
        marker = marker.format(RATCHET=RATCHET, key=allowance.key)
    named = f"`{allowance.key}` " if allowance.key and allowance.family == RATCHET else ""
    return (
        f"{path}:{line}: this line's {named}allowance is gone -- its {marker} marker "
        f"was removed while the line stayed. {breaks} Keep the marker, or delete or "
        "change the line it covers in this commit."
    )


def _survivors(
    allowance: Allowance, after_lines: list[str], after_tests: set[int]
) -> list[int]:
    """The lines of `after` that still hold the code `allowance` covered, without
    a marker of its own."""
    if allowance.family in WHOLE:
        marker = GENERATED_MARKER if allowance.family == GENERATED else GRAPH_PROPS_MARKER
        return [] if allowance.code not in _body(after_lines, marker) else [allowance.marker]
    if allowance.family == LOCALE:
        # The registry loads the locale the marker declares: that name is its code.
        return (
            [allowance.marker]
            if f'"{allowance.key}"' in _code("\n".join(after_lines))
            else []
        )
    pattern = INLINE.get(allowance.family)
    survivors = []
    for index, line in enumerate(after_lines):
        if index in after_tests:
            continue
        code = _code(line)
        if not code or not code.startswith(allowance.code):
            continue
        if allowance.family == QUIET and not _still_needs_a_reason(after_lines, index):
            continue
        if pattern is not None and pattern.search(line):
            continue
        survivors.append(index)
    return survivors


def diff_problems(
    path: str,
    before: str,
    after: str,
    declared: set[str],
    families: frozenset[str] = FAMILIES,
) -> list[str]:
    """What `before` allowed on a line that `after` still has, and no longer allows."""
    families = frozenset(
        family for family in families if path.startswith(SCOPE[family])
    )
    after_lines = after.splitlines()
    after_tests = _test_items(after_lines)
    allowed_at: dict[int, set[str]] = defaultdict(set)
    remaining: Counter[tuple[str, str, str]] = Counter()
    for allowance in allowances(after_lines, families):
        if allowance.family == RATCHET:
            allowed_at[allowance.index].add(allowance.key)
        remaining[_group(allowance)] += 1
    problems = []
    for allowance in allowances(before.splitlines(), families):
        group = _group(allowance)
        if remaining[group]:
            remaining[group] -= 1
            continue
        if allowance.family == RATCHET and (
            allowance.key not in declared or not allowance.code
        ):
            # A pattern no audit declares any more is a dead marker.
            continue
        if allowance.family in WHOLE and not allowance.code:
            continue
        if allowance.family == GRAPH_PROPS and allowance.key in GRAPH_PROPS_DEFAULTS:
            # A component the reader already reads the marker's way covers nothing.
            continue
        if allowance.family == RATCHET:
            survivors = [
                index
                for index, line in enumerate(after_lines)
                if line.strip() == allowance.code
                and index not in after_tests
                and allowance.key not in allowed_at[index]
            ]
        else:
            survivors = _survivors(allowance, after_lines, after_tests)
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


def read_families(root: Path | None = None) -> frozenset[str]:
    """The marker families a scan of `root`'s tree reads.

    A family whose scans are all gone from the tree allows nothing there, so its
    markers may go with nothing to keep. The ratchet and the two inline markers
    belong to this tool's own tree, and their dead markers are the pattern keys
    `declared_keys` compares.
    """
    root = root or ROOT
    read = {RATCHET, *INLINE}
    for family, scans in READERS.items():
        for path, declaration in scans:
            source = root / path
            if source.is_file() and declaration in source.read_text(
                encoding="utf-8", errors="replace"
            ):
                read.add(family)
                break
    return frozenset(read)


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
    families = read_families(root)
    problems = []
    for path in changed:
        before = _text(root, f"HEAD:{path}")
        if before is None:
            # A file HEAD does not hold lost no allowance it had.
            continue
        target = renamed.get(path, path)
        after = _text(root, f":{target}") or ""
        problems.extend(diff_problems(target, before, after, declared, families))
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
    families = read_families(root)
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
        problems.extend(diff_problems(target, before, after or "", declared, families))
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

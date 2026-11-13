#!/usr/bin/env python3
"""An enum that serde encodes keeps every variant it had, in its place.

Compact encodings (postcard, which workspaces, relays and the device's records
are written in) name a variant by its position: removing a variant, or
moving one, makes every later variant read as another, and bytes already
held anywhere change meaning. Name-based encodings (JSON) cannot read a
removed variant either. So for each enum that derives `Serialize` or
`Deserialize`:

It fails when a variant an enum had is gone, or, for an enum encoded by
position (externally tagged, the default), when the variants it had no
longer lead it in the same order. New variants go at the end.

Three things run it, which is what keeps a removal from landing:

``staged``, in the pre-commit hook: the tree being committed against HEAD's.
``check_commit_policy.py``, in the lint job, on every new commit of a change,
each one against its parents' exact blobs: the check a ``--no-verify`` commit
still meets.
``against BASE`` by hand, when a variant was removed some commits back and
the commit that did it is not in the range ``before``..``head`` names.

A variant retired in place (renamed ``Retired...``, the convention here)
keeps its position and is no removal. An enum whose encoding goes through
another type both ways
(``#[serde(from = ..., into = ...)]``, or ``try_from``) is judged by that
type, which keeps a retired variant in its place and reads it as what it
means now. Reading through another type alone does not change what the
enum writes.

A change that may break an encoding on purpose (a type no byte of which is
ever kept, or a format whose version is bumped and read by migration) is
named, with the reason, in ``tools/policy/wire_enum_breaks.toml``, in the
commit that makes it.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import code_only, is_test_file, matching  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
BREAKS = Path("tools/policy/wire_enum_breaks.toml")
SCOPES = ("crates/", "apps/")

DERIVE = re.compile(r"#\[derive\(([^\]]*)\)\]")
ENUM = re.compile(r"\benum\s+([A-Za-z_][A-Za-z0-9_]*)\b[^{;]*\{")
ATTRIBUTE = re.compile(r"#\s*\[")
VARIANT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
INTO = re.compile(r"\binto\s*=")
FROM = re.compile(r"\b(?:from|try_from)\s*=")
BY_NAME = re.compile(r"\b(?:untagged|tag\s*=|content\s*=)")


@dataclass(frozen=True)
class WireEnum:
    name: str
    variants: tuple[str, ...]
    by_position: bool


def _attributes_before(code: str, start: int) -> str:
    """The attributes (and visibility) written right before offset `start`."""
    at = start
    while True:
        head = code[:at].rstrip()
        if head.endswith("]"):
            depth, i = 0, len(head) - 1
            while i >= 0:
                if head[i] == "]":
                    depth += 1
                elif head[i] == "[":
                    depth -= 1
                    if depth == 0:
                        break
                i -= 1
            if i > 0 and head[i - 1] == "#":
                at = i - 1
                continue
            return code[at:start]
        visibility = re.search(r"\bpub(?:\s*\([^)]*\))?\s*$", head)
        if visibility:
            at = visibility.start()
            continue
        return code[at:start]


def _variants(body: str) -> tuple[str, ...]:
    """The variant names of an enum body (between its braces)."""
    names = []
    at = 0
    expecting = True
    while at < len(body):
        char = body[at]
        if char.isspace():
            at += 1
            continue
        if ATTRIBUTE.match(body, at):
            at = matching(body, body.index("[", at))
            continue
        if char in "({[":
            at = matching(body, at)
            continue
        if char == ",":
            expecting = True
            at += 1
            continue
        found = VARIANT.match(body, at)
        if found and expecting:
            names.append(found.group())
            expecting = False
            at = found.end()
            continue
        at += 1
    return tuple(names)


def wire_enums(text: str) -> dict[str, WireEnum]:
    """Enums deriving Serialize or Deserialize in `text`, by name; the ones
    encoded through another type are left out."""
    code = code_only(text)
    found = {}
    for match in ENUM.finditer(code):
        attributes = _attributes_before(code, match.start())
        derives = " ".join(DERIVE.findall(attributes))
        writes = re.search(r"\bSerialize\b", derives) is not None
        reads = re.search(r"\bDeserialize\b", derives) is not None
        if not writes and not reads:
            continue
        serde = " ".join(re.findall(r"#\[serde\(([^\]]*)\)\]", attributes))
        # Judged by the other type only when both directions go through it.
        if (not writes or INTO.search(serde)) and (not reads or FROM.search(serde)):
            continue
        open_brace = match.end() - 1
        body = code[open_brace + 1 : matching(code, open_brace) - 1]
        found[match.group(1)] = WireEnum(
            match.group(1), _variants(body), by_position=not BY_NAME.search(serde)
        )
    return found


def in_scope(path: str) -> bool:
    """Whether `path` is a file whose enums this check reads."""
    return (
        path.endswith(".rs")
        and path.startswith(SCOPES)
        and not is_test_file(Path(path))
    )


def _git(root: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=root, capture_output=True, text=True, check=True
    ).stdout


def _revision_text(root: Path, spec: str) -> str | None:
    """The text `spec` (`<revision>:<path>`, or `:<path>` for the index) holds."""
    shown = subprocess.run(
        ["git", "show", spec], cwd=root, capture_output=True, text=True
    )
    return shown.stdout if shown.returncode == 0 else None


def _diff_changes(root: Path, span: list[str]) -> tuple[list[str], dict[str, str]]:
    """The in-scope paths the diff `span` touches, and its renames (old: new)."""
    changed = [
        path
        for path in _git(root, "diff", "--name-only", "-M", *span, "--").splitlines()
        if in_scope(path)
    ]
    renamed = {}
    for line in _git(root, "diff", "--name-status", "-M", *span, "--").splitlines():
        parts = line.split("\t")
        if parts[0].startswith("R") and len(parts) == 3:
            renamed[parts[1]] = parts[2]
            changed.append(parts[1])
    return sorted(set(changed)), renamed


def _crate(path: str) -> str:
    """The crate directory `path` belongs to: everything before its `src/`."""
    head, _, _ = path.partition("/src/")
    return head


def _retired_in_place(old: tuple[str, ...], new: tuple[str, ...]) -> set[str]:
    """Variants of `old` whose place in `new` holds a `Retired...` variant."""
    return {
        before
        for before, after in zip(old, new)
        if before != after and after.startswith("Retired")
    }


def _keeps_places(old: tuple[str, ...], new: tuple[str, ...]) -> bool:
    """Every variant of `old` is at its place in `new`, or retired there."""
    return len(new) >= len(old) and all(
        before == after or after.startswith("Retired") for before, after in zip(old, new)
    )


def breaks(root: Path) -> set[tuple[str, str]]:
    """(file, enum) pairs named as broken on purpose, in the working tree."""
    path = root / BREAKS
    return allowed_breaks(path.read_text()) if path.is_file() else set()


def allowed_breaks(text: str) -> set[tuple[str, str]]:
    """(file, enum) pairs `text`, a `wire_enum_breaks.toml`, names."""
    listed = tomllib.loads(text)
    return {(entry["file"], entry["enum"]) for entry in listed.get("break", [])}


def diff_problems(
    before: dict[str, dict[str, WireEnum]],
    now: dict[str, dict[str, WireEnum]],
    allowed: set[tuple[str, str]],
    renamed: dict[str, str] | None = None,
) -> list[str]:
    """What the enums of `now` break of `before`, judged file by file.

    `renamed` maps a path on the `before` side to the one it became.
    """
    renamed = renamed or {}
    problems = []
    for path, enums in sorted(before.items()):
        for name, old in sorted(enums.items()):
            target = renamed.get(path, path)
            new = now.get(target, {}).get(name)
            if new is None:
                # Moved to another file of its crate in the same change.
                moved = [
                    (other, found[name])
                    for other, found in now.items()
                    if name in found and _crate(other) == _crate(path)
                ]
                if len(moved) != 1:
                    continue
                target, new = moved[0]
            if (target, name) in allowed:
                continue
            retired = _retired_in_place(old.variants, new.variants)
            gone = [
                variant
                for variant in old.variants
                if variant not in new.variants and variant not in retired
            ]
            if gone:
                problems.append(
                    f"{target}: enum {name} no longer has {', '.join(gone)}; bytes written "
                    "with it no longer read. Keep the variant (retired, read as what it "
                    "means now), or encode through a type that keeps it "
                    "(#[serde(from, into)])."
                )
            elif old.by_position and not _keeps_places(old.variants, new.variants):
                problems.append(
                    f"{target}: enum {name} moved its variants ({', '.join(old.variants)} "
                    f"became {', '.join(new.variants)}); compact encodings name a variant "
                    "by its place, so stored bytes would read as other variants. New "
                    "variants go at the end."
                )
    return problems


def against(base: str, root: Path = ROOT, head: str | None = None) -> list[str]:
    """What the enums of `head`'s tree (or this working tree) break of `base`, from where
    the two meet: a variant `base` gained after `head` branched is no removal."""
    base = _git(root, "merge-base", base, head or "HEAD").strip()
    span = [base, head] if head else [base]
    changed, renamed = _diff_changes(root, span)
    before, now = {}, {}
    for path in changed:
        if (text := _revision_text(root, f"{base}:{path}")) is not None:
            before[path] = wire_enums(text)
        target = renamed.get(path, path)
        if head:
            text = _revision_text(root, f"{head}:{target}")
        else:
            current = root / target
            text = current.read_text(errors="replace") if current.is_file() else None
        if text is not None:
            now[target] = wire_enums(text)
    return diff_problems(before, now, breaks(root), renamed)


def staged(root: Path = ROOT) -> list[str]:
    """What the enums of `root`'s index break of HEAD's tree."""
    changed, renamed = _diff_changes(root, ["--cached"])
    before, now = {}, {}
    for path in changed:
        if (text := _revision_text(root, f"HEAD:{path}")) is not None:
            before[path] = wire_enums(text)
        target = renamed.get(path, path)
        if (text := _revision_text(root, f":{target}")) is not None:
            now[target] = wire_enums(text)
    return diff_problems(before, now, breaks(root), renamed)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = parser.add_subparsers(dest="command", required=True)
    base = sub.add_parser("against", help="a revision, against its tree or HEAD's")
    base.add_argument("base")
    sub.add_parser("staged", help="this index, against HEAD")
    args = parser.parse_args(argv)
    problems = against(args.base) if args.command == "against" else staged()
    for problem in problems:
        print(f"::error::{problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())

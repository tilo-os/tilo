#!/usr/bin/env python3
"""An enum that serde encodes keeps every variant it had, in its place.

Compact encodings (postcard, which rooms, relays and the device's records
are written in) name a variant by its position: removing a variant, or
moving one, makes every later variant read as another, and bytes already
held anywhere change meaning. Name-based encodings (JSON) cannot read a
removed variant either. So for each enum that derives `Serialize` or
`Deserialize`:

``against BASE`` (t's ``changes`` check, against main) fails when a variant
it had at BASE is gone, or, for an enum encoded by position (externally
tagged, the default), when the variants it had no longer lead it in the
same order. New variants go at the end.

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


def _in_scope(path: str) -> bool:
    return (
        path.endswith(".rs")
        and path.startswith(SCOPES)
        and not is_test_file(Path(path))
    )


def _git(root: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=root, capture_output=True, text=True, check=True
    ).stdout


def _show(root: Path, revision: str, path: str) -> str | None:
    shown = subprocess.run(
        ["git", "show", f"{revision}:{path}"], cwd=root, capture_output=True, text=True
    )
    return shown.stdout if shown.returncode == 0 else None


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
    """(file, enum) pairs named as broken on purpose."""
    path = root / BREAKS
    if not path.is_file():
        return set()
    listed = tomllib.loads(path.read_text())
    return {(entry["file"], entry["enum"]) for entry in listed.get("break", [])}


def against(base: str, root: Path = ROOT, head: str | None = None) -> list[str]:
    """What breaks from `base` to `head` (a revision; the working tree when
    none)."""
    span = [base, head] if head else [base]
    changed = [
        path
        for path in _git(root, "diff", "--name-only", "-M", *span, "--").splitlines()
        if _in_scope(path)
    ]
    renamed = {}
    for line in _git(root, "diff", "--name-status", "-M", *span, "--").splitlines():
        parts = line.split("\t")
        if parts[0].startswith("R") and len(parts) == 3:
            renamed[parts[1]] = parts[2]
            changed.append(parts[1])
    before: dict[str, dict[str, WireEnum]] = {}
    now: dict[str, dict[str, WireEnum]] = {}
    for path in set(changed):
        if (text := _show(root, base, path)) is not None:
            before[path] = wire_enums(text)
        target = renamed.get(path, path)
        if head:
            text = _show(root, head, target)
        else:
            current = root / target
            text = current.read_text(errors="replace") if current.is_file() else None
        if text is not None:
            now[target] = wire_enums(text)
    allowed = breaks(root)
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


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = parser.add_subparsers(dest="command", required=True)
    base = sub.add_parser("against")
    base.add_argument("base")
    args = parser.parse_args(argv)
    problems = against(args.base)
    for problem in problems:
        print(f"::error::{problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())

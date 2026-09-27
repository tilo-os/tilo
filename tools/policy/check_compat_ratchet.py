#!/usr/bin/env python3
"""Ratchet every half-finished API migration down to zero.

A migration that adds a compatibility adapter and stops halfway leaves two
APIs forever. ``compat_ratchet.toml`` lists each deprecated API with a
detector (a regex over Rust source; matches that start in a ``//`` comment
do not count), the number of sites it
matches, an owner, and the API that replaces it. This check:

* fails when a count goes up — new code must use the target API;
* fails when a count goes down until the ledger is lowered in the same change,
  so progress is recorded and cannot silently regress;
* fails at zero until the entry (and its adapter) is deleted;
* fails when an item marked as a compatibility adapter in ``crates/sdk`` (doc
  comment or lint reason: "compatibility adapter", "legacy shim", "temporary
  compatibility", ...) or any ``#[deprecated]`` item is not listed in some
  entry's ``adapters``, and when a listed adapter no longer exists.

Files come from ``git grep`` over the working tree (tracked and untracked, not
ignored), so the check reads only files that contain an entry's ``needles``.
Vendored crates and generated Cap'n Proto bindings are not scanned.
``--list NAME`` prints the sites an entry's detector matches.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
LEDGER = Path(__file__).with_name("compat_ratchet.toml")
SCANNED_ROOTS = ("apps", "crates", "examples", "fuzz", "tools")
SKIPPED_DIRS = {"vendor", "target", "generated"}
MARKER_ROOT = "crates/sdk/"
REQUIRED = ("name", "owner", "target", "reason", "pattern", "needles", "count")

ADAPTER_PHRASE = re.compile(
    r"\b(?:compatibility|compat|legacy|temporary)\s+(?:adapter|shim|wrapper|dispatcher|bridge)s?\b"
    r"|\b(?:temporary|migration)\s+compatibility\b",
    re.IGNORECASE,
)
# Fixed strings every ADAPTER_PHRASE match contains, for the git grep prefilter.
ADAPTER_WORDS = ("compat", "legacy", "temporary", "migration")
DEPRECATED = re.compile(r"^\s*#\[deprecated\b", re.MULTILINE)
ITEM = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:const|async|unsafe|extern\s+\"[^\"]*\")\s+)*"
    r"(?:fn|struct|enum|trait|type|const|static|mod|union)\s+([A-Za-z_]\w*)",
    re.MULTILINE,
)


@dataclass(frozen=True)
class Violation:
    location: str
    reason: str

    def __str__(self) -> str:
        return f"{self.location}: {self.reason}"


@dataclass(frozen=True)
class Marker:
    key: str
    line: int
    kind: str


def line_number(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def scanned(relative: str) -> bool:
    parts = relative.split("/")
    return (
        relative.endswith(".rs")
        and parts[0] in SCANNED_ROOTS
        and not SKIPPED_DIRS.intersection(parts)
        and not relative.endswith("_capnp.rs")
    )


def files_containing(
    root: Path, needles: set[str], tops: tuple[str, ...] = SCANNED_ROOTS, *, ignore_case: bool = False
) -> set[str]:
    """Scanned files under ``tops`` that contain any of the fixed strings."""
    if not needles:
        return set()
    command = ["git", "-C", str(root), "grep", "--untracked", "-I", "-l", "-F"]
    command += ["-i"] if ignore_case else []
    for needle in sorted(needles):
        command += ["-e", needle]
    command += ["--", *(f"{top.rstrip('/')}/*.rs" for top in tops)]
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    if result.returncode not in (0, 1):
        raise SystemExit(f"git grep failed: {result.stderr.strip()}")
    return {relative for relative in result.stdout.splitlines() if scanned(relative)}


def read(root: Path, relative: str, cache: dict[str, str]) -> str:
    if relative not in cache:
        cache[relative] = (root / relative).read_text(encoding="utf-8", errors="replace")
    return cache[relative]


def in_line_comment(text: str, offset: int) -> bool:
    start = text.rfind("\n", 0, offset) + 1
    return text[start:offset].lstrip().startswith("//")


def adapter_markers(root: Path, cache: dict[str, str]) -> list[Marker]:
    """Items announced as compatibility adapters, keyed ``path::item``."""
    deprecated = files_containing(root, {"#[deprecated"})
    phrased = files_containing(root, set(ADAPTER_WORDS), (MARKER_ROOT,), ignore_case=True)
    markers: list[Marker] = []
    for relative in sorted(deprecated | phrased):
        text = read(root, relative, cache)
        hits = [(m.start(), "#[deprecated]") for m in DEPRECATED.finditer(text)]
        if relative in phrased:
            hits += [(m.start(), repr(m.group(0))) for m in ADAPTER_PHRASE.finditer(text)]
        for offset, kind in sorted(hits):
            item = ITEM.search(text, offset)
            name = item.group(1) if item else "<end of file>"
            markers.append(Marker(f"{relative}::{name}", line_number(text, offset), kind))
    return markers


def load_ledger(path: Path) -> tuple[list[dict], list[Violation]]:
    entries = tomllib.loads(path.read_text(encoding="utf-8")).get("api", [])
    problems: list[Violation] = []
    seen: set[str] = set()
    valid = []
    for index, entry in enumerate(entries):
        name = entry.get("name", f"api[{index}]")
        missing = [key for key in REQUIRED if key not in entry]
        if missing:
            problems.append(Violation(name, f"ledger entry is missing {', '.join(missing)}"))
            continue
        if name in seen:
            problems.append(Violation(name, "duplicate ledger entry"))
            continue
        seen.add(name)
        if not isinstance(entry["count"], int) or entry["count"] < 0:
            problems.append(Violation(name, "count must be a non-negative integer"))
            continue
        if not entry["needles"] or not all(isinstance(n, str) and n for n in entry["needles"]):
            problems.append(Violation(name, "needles must be a non-empty list of fixed strings"))
            continue
        try:
            entry["regex"] = re.compile(entry["pattern"])
        except re.error as error:
            problems.append(Violation(name, f"invalid pattern: {error}"))
            continue
        valid.append(entry)
    return valid, problems


def entry_sites(root: Path, entries: list[dict], cache: dict[str, str]) -> dict[str, list[str]]:
    """``path:line`` of every detector match outside line comments, per entry name."""
    candidates = sorted(files_containing(root, {n for entry in entries for n in entry["needles"]}))
    result: dict[str, list[str]] = {entry["name"]: [] for entry in entries}
    for relative in candidates:
        text = read(root, relative, cache)
        for entry in entries:
            prefixes = tuple(entry.get("paths", ()))
            if prefixes and not relative.startswith(prefixes):
                continue
            if not any(needle in text for needle in entry["needles"]):
                continue
            result[entry["name"]] += [
                f"{relative}:{line_number(text, m.start())}"
                for m in entry["regex"].finditer(text)
                if not in_line_comment(text, m.start())
            ]
    return result


def audit(root: Path = ROOT, ledger_path: Path = LEDGER) -> list[Violation]:
    entries, violations = load_ledger(ledger_path)
    cache: dict[str, str] = {}
    counts = {name: len(found) for name, found in entry_sites(root, entries, cache).items()}
    ledger = ledger_path.name

    for entry in entries:
        name, allowed, found = entry["name"], entry["count"], counts[entry["name"]]
        if found > allowed:
            violations.append(Violation(name, (
                f"{found} site(s), ledger allows {allowed}. New code must use "
                f"{entry['target']}; list them with check_compat_ratchet.py --list {name}"
            )))
        elif found == 0:
            violations.append(Violation(name, (
                f"migration complete (0 sites): delete this entry from {ledger} "
                "together with its adapter(s)"
            )))
        elif found < allowed:
            violations.append(Violation(name, (
                f"{found} site(s) remain but {ledger} says {allowed}: "
                f"lower count to {found} in this change"
            )))

    markers = adapter_markers(root, cache)
    claimed = {key: entry["name"] for entry in entries for key in entry.get("adapters", ())}
    for marker in markers:
        if marker.key not in claimed:
            path = marker.key.split("::", 1)[0]
            violations.append(Violation(f"{path}:{marker.line}", (
                f"{marker.key} is marked {marker.kind} but no entry in {ledger} lists it "
                "in `adapters`; finish the migration or add an entry with a detector, "
                "count, owner and target"
            )))
    present = {marker.key for marker in markers}
    for key, name in sorted(claimed.items()):
        if key not in present:
            violations.append(Violation(name, (
                f"adapter {key} is no longer marked or no longer exists; remove it from `adapters`"
            )))
    return violations


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--list", metavar="NAME", help="print the sites an entry matches")
    args = parser.parse_args(argv)

    if args.list:
        entries, _ = load_ledger(LEDGER)
        entries = [entry for entry in entries if entry["name"] == args.list]
        if not entries:
            print(f"no ledger entry named {args.list!r}", file=sys.stderr)
            return 2
        for site in entry_sites(ROOT, entries, {})[args.list]:
            print(site)
        return 0

    violations = audit()
    if violations:
        print("Compatibility-adapter ratchet FAILED", file=sys.stderr)
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        return 1
    print("Compatibility-adapter ratchet OK (every deprecated API is ledgered and none grew)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Symbol bytes by crate in the tapps a symbols build wrote, and what an app gained.

A tapp's bytes are its own code plus what it instantiates of the crates the
runtime provides (a generic written for its own types, a serde visitor for one of
its records). `build-thin-apps.sh` keeps the symbol tables with
TILO_THIN_KEEP_SYMBOLS=1; `nm -S --defined-only -C` on each binary then gives
every defined symbol's size, which `measure` sums by crate: the first segment of
the demangled name, so a generic instantiated for an app's type counts as its
crate's code. `diff` compares two measurements: which crates an app grew by and
how much. The symbol table undercounts inlined code (docs/measurements/
app-package-size.md), and the difference against the shipped tapp's bytes is the
unwound tables, read-only data and the format itself.

    tapp_symbol_sizes.py measure THIN_APP_DIR --plan PLAN [--out FILE] [--extra LIBRARY]
    tapp_symbol_sizes.py diff BASE.json HEAD.json [--app ID] [--crate NAME]
    tapp_symbol_sizes.py biggest BINARY --crate NAME [--limit N]
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import subprocess
import sys


def crate_of(name: str) -> str:
    """The crate a demangled symbol name comes from."""
    cut = name[1:] if name.startswith("<") else name
    if "::" not in cut:
        return "other"
    match = re.match(r"[A-Za-z][A-Za-z0-9_]*", cut)
    return match.group(0) if match else "other"


def parse_symbols(listing: str) -> dict[str, int]:
    """Each defined symbol's size, summed by crate."""
    sizes: dict[str, int] = {}
    for line in listing.splitlines():
        fields = line.split(maxsplit=3)
        if len(fields) < 4 or fields[2] in {"U", "w", "v"}:
            continue
        try:
            size = int(fields[1], 16)
        except ValueError:
            continue
        crate = crate_of(fields[3])
        sizes[crate] = sizes.get(crate, 0) + size
    return sizes


def symbol_bytes(path) -> dict[str, int]:
    """`parse_symbols` over a binary's symbol table."""
    listed = subprocess.run(["nm", "-S", "--defined-only", "-C", str(path)],
                            capture_output=True, text=True, check=True)
    return parse_symbols(listed.stdout)


def measure(thin_app, plan) -> dict[str, dict[str, int]]:
    """Every binary the plan packages, by crate."""
    built = json.loads(plan.read_text(encoding="utf-8"))
    sizes = {}
    for entry in built.get("catalog", []):
        path = thin_app / entry["binary"]
        if not path.is_file():
            raise SystemExit(f"{path} is not built; build with TILO_THIN_KEEP_SYMBOLS=1")
        sizes[entry["id"]] = symbol_bytes(path)
    return sizes


def named(sizes: dict[str, int]) -> int:
    return sum(sizes.values())


def biggest(path, crate: str, limit: int) -> list[str]:
    """The largest symbols of one crate in one binary: the entry points a fix moves out."""
    listed = subprocess.run(["nm", "-S", "--defined-only", "-C", str(path)],
                            capture_output=True, text=True, check=True)
    rows = []
    for line in listed.stdout.splitlines():
        fields = line.split(maxsplit=3)
        if len(fields) < 4 or crate_of(fields[3]) != crate:
            continue
        try:
            rows.append((int(fields[1], 16), fields[3]))
        except ValueError:
            continue
    return [f"{size:8d} {name}" for size, name in sorted(rows, reverse=True)[:limit]]


def diff(base: dict[str, dict[str, int]], head: dict[str, dict[str, int]],
         apps: list[str] | None, crates: list[str] | None) -> list[str]:
    lines = []
    for app in sorted(set(head) & (set(apps) if apps else set(head))):
        before = base.get(app, {})
        after = head[app]
        delta = {crate: after.get(crate, 0) - before.get(crate, 0) for crate in set(before) | set(after)}
        growth = named(after) - named(before)
        if growth <= 0:
            continue
        share = sorted(((value, crate) for crate, value in delta.items() if value >= 0), reverse=True)
        lines.append(f"{app}: {named(before)} -> {named(after)} (+{growth})")
        for value, crate in share[:12]:
            if crates is None or crate in crates:
                if value >= 1024 or crates is not None:
                    lines.append(f"  +{value:8d} {crate}")
        for crate, value in kept_down(before, after):
            lines.append(f"  -{value:8d} {crate}")
    return lines


def kept_down(before: dict[str, int], after: dict[str, int]) -> list[tuple[str, int]]:
    since = {crate: after.get(crate, 0) for crate in before}
    shrank = [(since[crate] - size, crate) for crate, size in before.items() if since[crate] < size]
    return [(crate, -value) for value, crate in sorted(shrank) if -value >= 4096]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    measured = commands.add_parser("measure", help="symbol bytes by crate for each tapp of a symbols build")
    measured.add_argument("thin_app", type=pathlib.Path)
    measured.add_argument("--plan", type=pathlib.Path, required=True)
    measured.add_argument("--out", type=pathlib.Path)
    measured.add_argument("--extra", type=pathlib.Path, action="append",
                          help="a library to measure beside the tapps (the runtime, an engine)")
    compared = commands.add_parser("diff", help="what each app gained, by crate, between two measurements")
    compared.add_argument("base", type=pathlib.Path)
    compared.add_argument("head", type=pathlib.Path)
    compared.add_argument("--app", action="append")
    compared.add_argument("--crate", action="append")
    biggest_parser = commands.add_parser("biggest", help="the largest symbols of one crate in one binary")
    biggest_parser.add_argument("binary", type=pathlib.Path)
    biggest_parser.add_argument("--crate", required=True)
    biggest_parser.add_argument("--limit", type=int, default=20)
    args = parser.parse_args(argv)
    if args.command == "measure":
        sizes = measure(args.thin_app, args.plan)
        for library in args.extra or []:
            sizes[library.name] = symbol_bytes(library)
        text = json.dumps(sizes, indent=1, sort_keys=True)
        if args.out:
            args.out.write_text(text, encoding="utf-8")
        else:
            print(text)
        return 0
    if args.command == "biggest":
        print("\n".join(biggest(args.binary, args.crate, args.limit)))
        return 0
    base = json.loads(args.base.read_text(encoding="utf-8"))
    head = json.loads(args.head.read_text(encoding="utf-8"))
    print("\n".join(diff(base, head, args.app, args.crate)))
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Keep Rust source files small enough to read.

A file past a few thousand lines stops having one subject: it collects every
change that touches its type, conflicts with every other change in flight, and
nobody reads it whole any more. A new or small file may grow to ``LIMIT``
lines. Files already past it are listed in ``source_file_size_baseline.toml``
with a ceiling they may not outgrow; split a file instead of raising its
ceiling. A ceiling more than ``SLACK`` above its file's length is lowered
(``--write-baseline`` does it), so space a split frees is not refilled.

Vendored and ``@generated`` sources are skipped.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).with_name("source_file_size_baseline.toml")
LIMIT = 3000
SLACK = 500


def tracked_sources(root: Path) -> dict[str, int]:
    names = subprocess.run(
        ["git", "ls-files", "-z", "--", "*.rs"],
        cwd=root,
        check=True,
        capture_output=True,
    ).stdout.decode().split("\0")
    sizes: dict[str, int] = {}
    for name in names:
        if not name or name.startswith("vendor/"):
            continue
        path = root / name
        if not path.is_file():
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        if "@generated" in "\n".join(text.splitlines()[:20]):
            continue
        sizes[name] = text.count("\n")
    return sizes


def ceiling_for(lines: int) -> int:
    return -(-lines // SLACK) * SLACK


def load_baseline(path: Path) -> dict[str, int]:
    if not path.exists():
        return {}
    return dict(tomllib.loads(path.read_text(encoding="utf-8")).get("ceilings", {}))


def audit(sizes: dict[str, int], ceilings: dict[str, int]) -> list[str]:
    problems = []
    for name, lines in sorted(sizes.items()):
        ceiling = ceilings.get(name, LIMIT)
        if lines > ceiling:
            problems.append(
                f"{name}: {lines} lines, over its ceiling of {ceiling}; "
                "split it into cohesive modules"
            )
    for name, ceiling in sorted(ceilings.items()):
        lines = sizes.get(name, 0)
        if ceiling > max(ceiling_for(lines), LIMIT) or ceiling <= LIMIT:
            problems.append(
                f"{name}: ceiling {ceiling} but {lines} lines; "
                f"lower it with {Path(__file__).name} --write-baseline"
            )
    return problems


def write_baseline(sizes: dict[str, int], ceilings: dict[str, int], path: Path) -> None:
    lines = [
        "# Rust files longer than check_source_file_size.py's limit, each with the",
        "# ceiling it may not outgrow. Ceilings only fall: split a file rather than",
        "# raise one. Regenerate with check_source_file_size.py --write-baseline.",
        "",
        "[ceilings]",
    ]
    seeding = not ceilings
    for name, size in sorted(sizes.items()):
        if size <= LIMIT or not (seeding or name in ceilings):
            continue
        ceiling = min(ceilings.get(name, ceiling_for(size)), ceiling_for(size))
        lines.append(f'"{name}" = {ceiling}')
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--write-baseline",
        action="store_true",
        help="drop files back under the limit and lower ceilings to fit; never raises or adds one",
    )
    args = parser.parse_args()
    sizes = tracked_sources(ROOT)
    ceilings = load_baseline(BASELINE)
    if args.write_baseline:
        write_baseline(sizes, ceilings, BASELINE)
        ceilings = load_baseline(BASELINE)
    problems = audit(sizes, ceilings)
    for problem in problems:
        print(problem)
    if problems:
        print(
            f"source file size: {len(problems)} problem(s); see tools/policy/check_source_file_size.py",
            file=sys.stderr,
        )
        return 1
    print("source file size OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

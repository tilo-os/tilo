#!/usr/bin/env python3
"""Hold an OS image's /usr to its flavour's budget.

    image_size_budget.py check --flavour production-x86_64 --report image-size.json
    image_size_budget.py check --flavour production-x86_64 --root /path/to/rootfs
    image_size_budget.py list

`--report` takes the JSON tools/perf/image_size.py writes after replaying the
image's layers from the registry or an OCI layout (CI: os-acceptance-{arch});
`--root` walks an unpacked root file system. Either way the answer is the same
three checks, against os/image-size-budget.toml:

* /usr is not larger than the flavour's `usr_bytes` (a size that was not
  observed is a failure, never a pass);
* nothing the Windows compatibility system extension owns is back in the image
  (`moved_to_sysext.paths`; a root walk checks every path, a report checks the
  largest files it lists);
* optionally, no ELF in /usr/bin carries a `.symtab` over the flavour's
  `max_elf_symtab_bytes` (root walk only).

Exit 0 when within budget, 1 when over, 2 for a usage or input error.
"""

from __future__ import annotations

import argparse
import json
import os
import struct
import sys
import tomllib
from pathlib import Path
from typing import Any

BUDGETS = Path(__file__).resolve().parents[1] / "image-size-budget.toml"
ELF_MAGIC = b"\x7fELF"


class BudgetError(Exception):
    """An input that cannot be judged (distinct from being over budget)."""


def load_budgets(path: Path = BUDGETS) -> dict[str, Any]:
    data = tomllib.loads(path.read_text())
    for name, flavour in data.get("flavour", {}).items():
        if not isinstance(flavour.get("usr_bytes"), int) or flavour["usr_bytes"] <= 0:
            raise BudgetError(f"flavour {name} has no positive usr_bytes")
    if not data.get("moved_to_sysext", {}).get("paths"):
        raise BudgetError("the budget lists no paths moved to the sysext")
    return data


def flavour_budget(data: dict[str, Any], name: str) -> dict[str, Any]:
    try:
        return data["flavour"][name]
    except KeyError:
        known = ", ".join(sorted(data.get("flavour", {})))
        raise BudgetError(f"no budget for flavour {name!r} (known: {known})") from None


def measure_usr(root: Path) -> tuple[int, list[tuple[int, str]]]:
    """Regular-file bytes under root/usr, each inode once, and the biggest files."""
    seen: set[tuple[int, int]] = set()
    total = 0
    largest: list[tuple[int, str]] = []
    top = root / "usr"
    if not top.is_dir():
        raise BudgetError(f"{top} is not a directory")
    for directory, _, names in os.walk(top, followlinks=False):
        for name in names:
            path = Path(directory) / name
            try:
                info = path.lstat()
            except OSError as error:
                raise BudgetError(f"cannot stat {path}: {error}") from error
            if not path.is_file() or path.is_symlink():
                continue
            key = (info.st_dev, info.st_ino)
            if key in seen:
                continue
            seen.add(key)
            total += info.st_size
            largest.append((info.st_size, "/" + str(path.relative_to(root))))
    largest.sort(reverse=True)
    return total, largest[:15]


def symtab_bytes(path: Path) -> int:
    """Size of `.symtab` in a little-endian ELF64 file, 0 if there is none."""
    with path.open("rb") as handle:
        header = handle.read(64)
        if header[:4] != ELF_MAGIC or len(header) < 64 or header[4] != 2 or header[5] != 1:
            return 0
        shoff, = struct.unpack_from("<Q", header, 0x28)
        shentsize, shnum, shstrndx = struct.unpack_from("<HHH", header, 0x3A)
        if not shoff or shentsize < 64 or shnum == 0 or shstrndx >= shnum:
            return 0
        handle.seek(shoff)
        table = handle.read(shentsize * shnum)
        if len(table) < shentsize * shnum:
            return 0
        sections = [struct.unpack_from("<IIQQQQIIQQ", table, i * shentsize) for i in range(shnum)]
        _, _, _, _, names_offset, names_size, *_ = sections[shstrndx]
        handle.seek(names_offset)
        names = handle.read(names_size)
        for name_index, _, _, _, _, size, *_ in sections:
            end = names.find(b"\0", name_index)
            if end >= 0 and names[name_index:end] == b".symtab":
                return size
    return 0


def oversized_symbol_tables(root: Path, limit: int) -> list[tuple[int, str]]:
    found = []
    bin_dir = root / "usr/bin"
    if bin_dir.is_dir():
        for path in sorted(bin_dir.iterdir()):
            if path.is_file() and not path.is_symlink():
                size = symtab_bytes(path)
                if size > limit:
                    found.append((size, "/" + str(path.relative_to(root))))
    return sorted(found, reverse=True)


def present_moved_paths(root: Path, paths: list[str]) -> list[str]:
    return ["/" + path for path in paths if (root / path).exists() or (root / path).is_symlink()]


def mb(value: int) -> str:
    return f"{value / 1e6:,.0f} MB"


def check_root(root: Path, budget: dict[str, Any], moved: list[str]) -> tuple[list[str], str]:
    usr, largest = measure_usr(root)
    problems = []
    if usr > budget["usr_bytes"]:
        problems.append(f"/usr is {mb(usr)}, over the {mb(budget['usr_bytes'])} budget; largest files:\n"
                        + "\n".join(f"  {mb(size):>10}  {path}" for size, path in largest[:8]))
    for path in present_moved_paths(root, moved):
        problems.append(f"{path} is in the image; it belongs to the windows-compat system extension")
    limit = budget.get("max_elf_symtab_bytes")
    if limit:
        for size, path in oversized_symbol_tables(root, limit):
            problems.append(f"{path} keeps a {mb(size)} .symtab (limit {mb(limit)}); "
                            "ship it stripped with its symbols split out")
    return problems, f"/usr {mb(usr)} of {mb(budget['usr_bytes'])}"


def check_report(report: dict[str, Any], budget: dict[str, Any], moved: list[str]) -> tuple[list[str], str]:
    usr = report.get("totals", {}).get("usr_content")
    if not isinstance(usr, int) or isinstance(usr, bool):
        raise BudgetError("the report has no totals.usr_content: a size that was not observed is not a pass")
    problems = []
    if usr > budget["usr_bytes"]:
        listed = sorted(report.get("files", []), key=lambda item: -item[1])[:8]
        problems.append(f"/usr is {mb(usr)}, over the {mb(budget['usr_bytes'])} budget; largest files:\n"
                        + "\n".join(f"  {mb(size):>10}  {path}" for path, size in listed))
    forbidden = tuple("/" + path.rstrip("/") for path in moved)
    for path, _size in report.get("files", []):
        if any(path == prefix or path.startswith(prefix + "/") for prefix in forbidden):
            problems.append(f"{path} is in the image; it belongs to the windows-compat system extension")
    return problems, f"/usr {mb(usr)} of {mb(budget['usr_bytes'])}"


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    check = sub.add_parser("check")
    check.add_argument("--flavour", required=True)
    source = check.add_mutually_exclusive_group(required=True)
    source.add_argument("--report", type=Path)
    source.add_argument("--root", type=Path)
    check.add_argument("--budgets", type=Path, default=BUDGETS)
    listing = sub.add_parser("list")
    listing.add_argument("--budgets", type=Path, default=BUDGETS)
    args = parser.parse_args(argv)
    try:
        data = load_budgets(args.budgets)
        if args.command == "list":
            for name, flavour in sorted(data["flavour"].items()):
                print(f"{name}\t{flavour['usr_bytes']}")
            return 0
        budget = flavour_budget(data, args.flavour)
        moved = data["moved_to_sysext"]["paths"]
        if args.root:
            problems, summary = check_root(args.root, budget, moved)
        else:
            problems, summary = check_report(json.loads(args.report.read_text()), budget, moved)
    except (BudgetError, OSError, ValueError, tomllib.TOMLDecodeError) as error:
        print(f"image-size budget: {error}", file=sys.stderr)
        return 2
    if problems:
        print(f"image-size budget FAILED for {args.flavour}: {summary}", file=sys.stderr)
        for problem in problems:
            print(f"- {problem}", file=sys.stderr)
        return 1
    print(f"image-size budget ok for {args.flavour}: {summary}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

#!/usr/bin/env python3
"""Print the nextest filter a run narrowed by `T_TESTS` runs.

`t test <check> --test <name>` hands a check whose t.toml declares `tests = "env"` the names it
selects in `T_TESTS`, one per line: a test's full name, a module path for every test below it, or
a module's own name. This prints the filterset selecting exactly those, which a wrapper
intersects with the filter the run's command names.

A name is matched as a path segment, as t narrows a family's run
(src/drivers/cargo/exec/filters.rs): `parser` names the module `parser` and the test called
`parser`, never `parser_extra`; `parser::` names the module alone.
"""
from __future__ import annotations

import os
import sys


def escape(module: str) -> str:
    """A module path as a regex literal: every character outside Rust's path alphabet escaped."""
    return "".join(c if (c.isascii() and c.isalnum()) or c in "_:" else "\\" + c for c in module)


def name_filter(name: str) -> str:
    """The nextest term selecting `name`: one test, or a module and every test below it."""
    if name.endswith("::"):
        return f"test(/(?:^|::){escape(name[:-2])}::/)"
    return f"test(/(?:^|::){escape(name)}(?:$|::)/)"


def expression(names: list[str]) -> str:
    """The filterset selecting every test any of `names` selects, empty for none."""
    return " | ".join(name_filter(name.strip()) for name in names if name.strip())


def main() -> int:
    print(expression(os.environ.get("T_TESTS", "").splitlines()))
    return 0


if __name__ == "__main__":
    sys.exit(main())

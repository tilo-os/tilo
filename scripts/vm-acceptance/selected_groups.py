#!/usr/bin/env python3
"""The performance KPI groups a run's selection produces, for compare.py --groups.

    scripts/vm-acceptance/selected_groups.py "$only"

`$only` is the `--only` value tools/ci/vm-acceptance.sh ran (scripts/vm-acceptance/
run.py select()): comma-separated check ids or areas, empty for the whole suite. It
prints the KPI groups those checks record, comma-separated: a dispatch that narrowed the
run to one app check is owed that check's measurements, never those of every check in
the lane. A group a selected check produces still has to report every budget the lane
owes in it (tools/perf/compare.py): the selection narrows which groups are owed, not
which measurements a group must hold.

A check names a KPI in its `ctx.record("idle.frames", …)` call, or in a `record_*`
helper it calls. A check whose measurement moves elsewhere moves its group with it, so
this reads the checks' own source. An id no check defines is refused in words, rather
than quietly comparing an empty selection.
"""

from __future__ import annotations

import importlib
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
CHECKS = HERE / "acceptance" / "checks"

# A check's own call; the `def record(` of the context is not one.
RECORD = re.compile(r"\Wctx\.record\(\s*f?[\"']([a-z][a-z0-9_]*)\.")
HELPER_CALL = re.compile(r"\W(record_\w+)\(")
HELPER_DEF = r"^def {name}\(.*?(?=^\S)"
# The checks that judge everything a session did rather than one area (run.py
# SESSION_JUDGES) run in a selection of their own, so theirs is not the selection's.
SESSION_JUDGES = {"P03", "R07", "A16"}


def registered() -> list[tuple[str, str]]:
    """(id, area) of every check, in the order they run."""
    sys.path.insert(0, str(HERE))
    from acceptance import core  # noqa: PLC0415
    for module in sorted(CHECKS.glob("*.py")):
        if module.stem != "__init__":
            importlib.import_module(f"acceptance.checks.{module.stem}")
    return [(c.id, c.area) for c in core.REGISTRY]


def selected(only: str) -> list[str]:
    known = registered()
    ids, areas = {c for c, _ in known}, {a for _, a in known}
    if not only.strip():
        return [c for c, _ in known]
    words = [w.strip() for w in only.split(",")]
    unknown = [w for w in words if w not in ids and w not in areas]
    if unknown:
        raise KeyError(f"no check or area: {', '.join(unknown)}")
    chosen = {c for c in ids if c in words} | {c for c, a in known if a in words}
    return [c for c, _ in known if c in chosen]


def groups(ids: list[str]) -> list[str]:
    """The KPI groups the checks in `ids` record."""
    wanted = set(ids) - SESSION_JUDGES
    produced: set[str] = set()
    for module in sorted(CHECKS.glob("*.py")):
        text = module.read_text(encoding="utf-8")
        # A module registers each check with @check("ID", …), then defines it; its body
        # runs until the next decorator at the start of a line.
        bodies = re.split(r"(?m)^@", text)[1:]
        for body in bodies:
            who = re.match(r'check\("(\w+)"', body)
            if who is None or who.group(1) not in wanted:
                continue
            produced |= set(RECORD.findall(body))
            produced.update(_KPI for name in set(HELPER_CALL.findall(body))
                            for _KPI in RECORD.findall(_helper(text, name)))
    return sorted(produced)


def _helper(text: str, name: str) -> str:
    found = re.search(HELPER_DEF.format(name=name), text, re.S | re.M)
    return found.group(0) if found else ""


def main(argv: list[str]) -> int:
    only = argv[0] if argv else ""
    try:
        produced = groups(selected(only))
    except KeyError as error:
        print(f"selected_groups.py: {error.args[0]} ({only})", file=sys.stderr)
        return 2
    if not produced:
        print(f"selected_groups.py: the selection {only or 'all'} records no KPI", file=sys.stderr)
        return 2
    print(",".join(produced))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

#!/usr/bin/env python3
"""Emit the explicit migration inventory for remote ActionDirectory search.

The normal resource/action contract remains green during this additive rollout.
This report is intentionally machine-readable: a provider listed here is
locally searchable but is omitted from broker-pulled discovery until it proves
both a retained generation and cursor-generation validation.
"""

from __future__ import annotations

import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
IMPL = re.compile(
    r"\bimpl(?:\s*<[^>{}]*>)?\s+SearchProvider\s+for\s+([A-Za-z_][A-Za-z0-9_]*)"
)


def brace_body(source: str, start: int) -> str:
    opening = source.find("{", start)
    if opening < 0:
        return ""
    depth = 0
    for index in range(opening, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[opening + 1 : index]
    return ""


def main() -> int:
    offenders: list[dict[str, str]] = []
    for source in sorted((ROOT / "apps").glob("*/src/**/*.rs")):
        text = source.read_text(encoding="utf-8")
        for match in IMPL.finditer(text):
            body = brace_body(text, match.start())
            missing = []
            if not re.search(r"\bfn\s+action_directory_generation\s*\(", body):
                missing.append("action_directory_generation")
            if not re.search(r"\bfn\s+action_directory_cursor_is_current\s*\(", body):
                missing.append("action_directory_cursor_is_current")
            if missing:
                offenders.append(
                    {
                        "provider": match.group(1),
                        "source": source.relative_to(ROOT).as_posix(),
                        "missing": ",".join(missing),
                    }
                )
    print(json.dumps({"version": 1, "offenders": offenders}, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

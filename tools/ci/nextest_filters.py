#!/usr/bin/env python3
"""Print the shared nextest filtersets as GITHUB_ENV lines.

.config/nextest-filters.toml holds them; each key becomes an upper-case
`<KEY>_FILTER=<filterset>` line with whitespace folded to single spaces.
"""
from __future__ import annotations

from pathlib import Path
import sys
import tomllib

FILTERS = Path(__file__).resolve().parents[2] / ".config" / "nextest-filters.toml"


def filters(path: Path = FILTERS) -> dict[str, str]:
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    return {f"{key.upper()}_FILTER": " ".join(value.split()) for key, value in data.items()}


def main() -> int:
    for name, value in filters().items():
        print(f"{name}={value}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

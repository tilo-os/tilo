#!/usr/bin/env python3
"""Built code reads a surface in its compiled form, never the TOML it was written in.

A surface is written as `surface.toml`, but what is committed, shipped, embedded, hashed and
read is the canonical compiled `surface.json`. Readers refuse the authored TOML. So a crate that
embeds a `*surface*.toml` with `include_str!` or `include_bytes!` hands its reader text the reader
refuses. Its view then fails to start at run time: a newcomer's top bar stayed empty because the
session still embedded `window-menu-surface.toml` beside the compiled file written for it.

Embed the compiled `.json` instead. The one exception is the authoring templates that
`tilo-app-builder` writes into a new app, because those are TOML on purpose. Tests may embed an
authored surface to compile it, so only production source is scanned.
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_source  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
SCANNED = ("apps", "crates", "tools", "examples")
# The authored templates a new app starts from.
TEMPLATES = "crates/lang/tilo-app-builder/templates/"
EMBED = re.compile(
    r"\binclude_(?:str|bytes)!\s*\(\s*(?:concat!\s*\([^)]*?)?\"([^\"]*surface[^\"]*\.toml)\""
)


def embedded(source: Path, text: str) -> list[str]:
    """The authored surfaces `text`, from `source`, embeds outside the templates."""
    found = []
    for path in EMBED.findall(rust_source.production_source(text)):
        target = (source.parent / path).resolve()
        try:
            relative = target.relative_to(ROOT)
        except ValueError:
            relative = Path(path)
        if not str(relative).startswith(TEMPLATES):
            found.append(path)
    return found


def audit(root: Path = ROOT) -> tuple[list[str], int]:
    """Every embed of an authored surface in production source, and the sources scanned."""
    violations: list[str] = []
    scanned = 0
    for tree in SCANNED:
        for source in sorted((root / tree).rglob("*.rs")):
            relative = source.relative_to(root)
            if any(part in {"target", "vendor"} for part in relative.parts):
                continue
            if rust_source.is_test_file(relative):
                continue
            scanned += 1
            for path in embedded(source, source.read_text(encoding="utf-8")):
                violations.append(f"{relative}: embeds the authored surface {path}")
    return violations, scanned


def main() -> int:
    violations, scanned = audit()
    if not scanned:
        print("check_compiled_surfaces: no Rust sources found; the scan is pointed wrong")
        return 1
    for violation in violations:
        print(violation)
    if violations:
        print(
            "Embed the compiled surface (.json) the reader takes; only "
            f"{TEMPLATES} keeps authored TOML."
        )
        return 1
    print(f"check_compiled_surfaces: {scanned} sources embed only compiled surfaces")
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Write the first-party catalogue from the apps' own manifests.

Every `apps/*/tilo-app.toml` is one first-party package. The catalogue is written to
`crates/session/desktop-model/src/first_party_catalogue.rs`, which no one
edits by hand: the desktop model (and through it every host) reads the
first-party packages from there.

  first-party-catalogue.py           write the catalogue
  first-party-catalogue.py --check   fail when the written one differs

`--check` prints its finding as `path: message` on stdout.
"""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "crates/session/desktop-model/src/first_party_catalogue.rs"


def packages() -> list[str]:
    """Each manifest's package id, sorted."""
    ids = [tomllib.loads(manifest.read_text())["package"]["id"]
           for manifest in ROOT.glob("apps/*/tilo-app.toml")]
    duplicates = sorted({package for package in ids if ids.count(package) > 1})
    if duplicates:
        raise SystemExit(f"two manifests declare {duplicates}")
    return sorted(ids)


def rendered() -> str:
    found = packages()

    def listed(name: str, doc: str, ids: list[str]) -> str:
        rows = "".join(f'    "{package}",\n' for package in ids)
        return f"/// {doc}\npub const {name}: &[&str] = &[\n{rows}];\n"

    return (
        "// Written by tools/first-party-catalogue.py from apps/*/tilo-app.toml;\n"
        "// tools/policy checks it matches them. Do not edit by hand.\n"
        "\n"
        + listed("FIRST_PARTY_PACKAGES", "Every first-party package: one per app manifest.", found)
    )


def main() -> int:
    text = rendered()
    if "--check" in sys.argv[1:]:
        current = OUTPUT.read_text() if OUTPUT.exists() else ""
        if current != text:
            print(f"{OUTPUT.relative_to(ROOT)}: differs from the app manifests; "
                  "run tools/first-party-catalogue.py")
            return 1
        print("first-party catalogue OK", file=sys.stderr)
        return 0
    OUTPUT.write_text(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())

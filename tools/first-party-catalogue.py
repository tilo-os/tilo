#!/usr/bin/env python3
"""Write the first-party app ids from the apps' own manifests.

Every `apps/*/tilo-app.toml` is one first-party package, and so is every tilolang package
in `apps/tl` whose `app` line names a `so.tilo.<name>` id: the one that took over a native
app's id when the native app was deleted. The ids are written to
`crates/core/tilo-app-id/src/apps/app_ids.rs`, which no one edits by hand: every
crate that names a first-party app reads its constant from there.

  first-party-catalogue.py           write the ids
  first-party-catalogue.py --check   fail when the written ones differ

`--check` prints its finding as `path: message` on stdout.
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "crates/core/tilo-app-id/src/apps/app_ids.rs"


TILOLANG_APP = re.compile(r"^app (so\.tilo\.[a-z][a-z0-9-]*)\s", re.MULTILINE)


def packages() -> list[str]:
    """Each manifest's package id and each first-party tilolang package's, sorted."""
    ids = [tomllib.loads(manifest.read_text())["package"]["id"]
           for manifest in ROOT.glob("apps/*/tilo-app.toml")]
    for source in ROOT.glob("apps/tl/*/app.tl"):
        named = TILOLANG_APP.search(source.read_text())
        if named:
            ids.append(named.group(1))
    duplicates = sorted({package for package in ids if ids.count(package) > 1})
    if duplicates:
        raise SystemExit(f"two manifests declare {duplicates}")
    return sorted(ids)


def constant(package: str) -> str:
    """`so.tilo.file-explorer` → `FILE_EXPLORER`."""
    return package.removeprefix("so.tilo.").replace("-", "_").upper()


def rendered() -> str:
    found = packages()
    rows = "".join(f'    {constant(package)} => "{package}",\n' for package in found)
    return (
        "// Written by tools/first-party-catalogue.py from apps/*/tilo-app.toml and apps/tl/*/app.tl;\n"
        "// tools/policy checks it matches them. Do not edit by hand.\n"
        "\n"
        "first_party_apps! {\n"
        + rows
        + "}\n"
    )


def main() -> int:
    text = rendered()
    if "--check" in sys.argv[1:]:
        current = OUTPUT.read_text() if OUTPUT.exists() else ""
        if current != text:
            print(f"{OUTPUT.relative_to(ROOT)}: differs from the app manifests; "
                  "run tools/first-party-catalogue.py")
            return 1
        print("first-party app ids OK", file=sys.stderr)
        return 0
    OUTPUT.write_text(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())

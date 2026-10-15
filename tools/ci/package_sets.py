#!/usr/bin/env python3
"""Print the `-p NAME` arguments of a package set check.yml selects from.

  package_sets.py android   apps with an `<app>-android` example
  package_sets.py browser   crates that build for the browser (wasm-bindgen)

The sets are those of tools/ci/affected_packages.py, over every workspace
package, so a whole-workspace run (a t check) lints what an affected-package
run would select.
"""
from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import affected_packages  # noqa: E402

SETS = {"android": affected_packages.android_apps, "browser": affected_packages.browser_crates}


def main(argv: list[str]) -> int:
    if len(argv) != 1 or argv[0] not in SETS:
        print(f"usage: package_sets.py {'|'.join(SETS)}", file=sys.stderr)
        return 2
    metadata = json.loads(subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        check=True, capture_output=True, text=True).stdout)
    packages = {package["name"]: package for package in metadata["packages"]}
    names = sorted(SETS[argv[0]](packages))
    if not names:
        print(f"no {argv[0]} packages in the workspace", file=sys.stderr)
        return 1
    print(" ".join(f"-p {name}" for name in names))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

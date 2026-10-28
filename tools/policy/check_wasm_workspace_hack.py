#!/usr/bin/env python3
"""Keep the workspace-hack out of the browser's wasm32 builds.

``crates/workspace-hack`` unifies the workspace's *native* feature sets
(tokio's ``net``, sockets, D-Bus, ...), and hakari writes most of them into
its unconditional ``[dependencies]``. A crate that also builds for
``wasm32-unknown-unknown`` (the browser runtime's web core, the apps it links
as side modules, the registry verifier) therefore names the hack only under
``[target.'cfg(not(target_arch = "wasm32"))'.dependencies]``; otherwise its
wasm32 build pulls mio and fails. ``cargo hakari manage-deps`` adds the line
unconditionally to a new member, so this check names any member the browser
builds reach that picked it up.

Graphs are resolved as ``cargo tree -p`` does for ``wasm32-unknown-unknown``,
without proc macros, which build for the host. Resolving them does not need
the wasm32 target installed.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WASM = "wasm32-unknown-unknown"
HACK = "workspace-hack"

# What the browser builds for wasm32: the desktop bundle, which links the
# browser runtime and every web app, and the image compute worker, each with
# the manifest a finding about it names.
ROOTS = (
    ("tilo-web-desktop", (), "crates/browser/tilo-web-desktop/Cargo.toml"),
    ("tilo-browser-runtime", (), "crates/browser/tilo-browser-runtime/Cargo.toml"),
    ("tilo-browser-compute", (), "crates/browser/tilo-browser-compute/Cargo.toml"),
)


def tree(package: str, features: tuple[str, ...], invert: str | None = None) -> list[str]:
    command = [
        "cargo", "tree", "--locked", "--package", package, "--target", WASM,
        "--edges", "normal,no-proc-macro", "--prefix", "none", "--format", "{p}",
    ]
    if features:
        command += ["--features", ",".join(features)]
    if invert:
        command += ["--invert", invert, "--depth", "1"]
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"cargo tree -p {package} failed:\n{result.stderr}")
    return [line.split()[0] for line in result.stdout.splitlines() if line.strip()]


def main() -> int:
    failed = False
    for package, features, manifest in ROOTS:
        try:
            reached = tree(package, features)
            if package not in reached:
                print(f"cargo tree did not describe {package} for {WASM}", file=sys.stderr)
                return 1
            if HACK not in reached:
                print(f"{package} ({WASM}): no {HACK}", file=sys.stderr)
                continue
            dependants = sorted(set(tree(package, features, invert=HACK)) - {HACK})
        except RuntimeError as error:
            print(error, file=sys.stderr)
            return 1
        failed = True
        print(
            f"{manifest}: {package} reaches {HACK} for {WASM} through {', '.join(dependants)}; name it "
            "under [target.'cfg(not(target_arch = \"wasm32\"))'.dependencies] in those manifests"
        )
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Print the source directory a build compiled the registry crate CRATE from.

usage: crate_source.py CRATE ARTIFACT_DIR CARGO_LOCK

ARTIFACT_DIR is the build's `target/<triple>/<profile>`. The crate's
dep-info there names the sources rustc read; where that build's paths are
not on this machine (a result restored from a compile cache), the crate is
looked up in Cargo's registry sources at the version the lockfile pins.

Unlike `cargo metadata`, this needs nothing the build did not fetch: the
metadata reads every package the lockfile names, for every platform, so
offline it fails on any package the build never needed.
"""

import os
import pathlib
import sys
import tomllib


def versions(lock: pathlib.Path, crate: str) -> list[str]:
    packages = tomllib.loads(lock.read_text()).get("package", [])
    return [
        p["version"]
        for p in packages
        if p.get("name") == crate and str(p.get("source", "")).startswith("registry+")
    ]


def from_dep_info(artifacts: pathlib.Path, crate: str, wanted: set[str]) -> pathlib.Path | None:
    stem = crate.replace("-", "_")
    infos = sorted(
        (artifacts / "deps").glob(f"{stem}-*.d"),
        key=lambda path: path.stat().st_mtime,
        reverse=True,
    )
    for info in infos:
        for token in info.read_text().replace("\\ ", "\0").split():
            path = pathlib.Path(token.rstrip(":").replace("\0", " "))
            if path.parts[-2:] != ("src", "lib.rs"):
                continue
            root = path.parent.parent
            if root.name in wanted and (root / "Cargo.toml").is_file():
                return root
    return None


def from_registry(wanted: set[str]) -> pathlib.Path | None:
    home = pathlib.Path(os.environ.get("CARGO_HOME", pathlib.Path.home() / ".cargo"))
    for index in sorted((home / "registry" / "src").glob("*")):
        for name in sorted(wanted):
            root = index / name
            if (root / "Cargo.toml").is_file():
                return root
    return None


def main() -> int:
    crate, artifacts, lock = sys.argv[1], pathlib.Path(sys.argv[2]), pathlib.Path(sys.argv[3])
    pinned = versions(lock, crate)
    if not pinned:
        print(f"{lock} pins no registry version of {crate}", file=sys.stderr)
        return 1
    wanted = {f"{crate}-{version}" for version in pinned}
    root = from_dep_info(artifacts, crate, wanted) or from_registry(wanted)
    if root is None:
        print(
            f"no sources of {crate} {', '.join(pinned)}: neither the build in {artifacts} "
            "nor Cargo's registry has them; build the crate first",
            file=sys.stderr,
        )
        return 1
    print(root)
    return 0


if __name__ == "__main__":
    sys.exit(main())

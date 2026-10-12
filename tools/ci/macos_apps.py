#!/usr/bin/env python3
"""Cargo packages of the apps that run on macOS, as `-p NAME` arguments.

An app runs on macOS when its `tilo-app.toml` lists `macos` in
`package.platforms`, or names no platforms at all (the default is Linux, macOS
and the web, as in tools/policy/check_app_portability.py).

    cargo build --locked $(python3 tools/ci/macos_apps.py)
"""

from __future__ import annotations

from pathlib import Path
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_PLATFORMS = ("linux", "macos", "web")


def macos_apps(root: Path = ROOT) -> list[str]:
    names = []
    for manifest in sorted((root / "apps").glob("*/tilo-app.toml")):
        cargo = manifest.parent / "Cargo.toml"
        if not cargo.is_file():
            continue
        package = tomllib.loads(manifest.read_text(encoding="utf-8")).get("package", {})
        if "macos" in package.get("platforms", DEFAULT_PLATFORMS):
            names.append(tomllib.loads(cargo.read_text(encoding="utf-8"))["package"]["name"])
    return names


def main() -> int:
    names = macos_apps()
    if not names:
        print("no app under apps/ runs on macOS", file=sys.stderr)
        return 1
    print(" ".join(f"-p {name}" for name in names))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

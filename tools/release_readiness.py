#!/usr/bin/env python3
"""Decide when immutable component channels are ready for OS composition."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys

import app_release
import ci_select
import desktop_release


ROOT = Path(__file__).resolve().parents[1]

OS_INPUTS = {".forgejo/workflows/build-image.yml"}
OS_PREFIXES = ("os/", "keys/", "docs/recovery.md", "docs/security/")
CHANNEL_SHA = re.compile(r"/0\.0\.0-([0-9a-f]{40})/")


def changed_paths(base: str, head: str = "HEAD", root: Path = ROOT) -> list[str]:
    output = subprocess.check_output(
        [
            "git",
            "diff",
            "--name-only",
            "--diff-filter=ACDMRTUXB",
            f"{base}..{head}",
        ],
        cwd=root,
        text=True,
    )
    return output.splitlines()


def os_changed(paths: list[str]) -> bool:
    return any(path in OS_INPUTS or path.startswith(OS_PREFIXES) for path in paths)


def apps_changed(paths: list[str], root: Path = ROOT) -> bool:
    return bool(
        app_release.apps_from_changes(
            paths,
            app_release.app_catalog(root),
            ci_select.cargo_metadata(root),
            root,
        )
    )


def channel_source(url: str) -> str:
    match = CHANNEL_SHA.search(url)
    if not match:
        raise ValueError("channel URL must contain an immutable 0.0.0-<commit> version")
    return match.group(1)


def readiness(
    app_url: str,
    desktop_url: str,
    head: str = "HEAD",
    root: Path = ROOT,
) -> dict[str, object]:
    app_source = channel_source(app_url)
    desktop_source = channel_source(desktop_url)
    app_paths = changed_paths(app_source, head, root)
    desktop_paths = changed_paths(desktop_source, head, root)
    pending_apps = apps_changed(app_paths, root)
    pending_desktop = desktop_release.desktop_changed(desktop_paths, root=root)
    return {
        "ready": not pending_apps and not pending_desktop,
        "app_source": app_source,
        "desktop_source": desktop_source,
        "pending_apps": pending_apps,
        "pending_desktop": pending_desktop,
    }


def emit_outputs(result: dict[str, object], output: Path) -> None:
    with output.open("a", encoding="utf-8") as stream:
        for name in ("ready", "pending_apps", "pending_desktop"):
            stream.write(f"{name}={str(result[name]).lower()}\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--app-url", required=True)
    parser.add_argument("--desktop-url", required=True)
    parser.add_argument("--github-output", type=Path)
    args = parser.parse_args()
    try:
        result = readiness(args.app_url, args.desktop_url, args.head, args.root)
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f"release readiness: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True))
    if args.github_output:
        emit_outputs(result, args.github_output)
    return 0


if __name__ == "__main__":
    sys.exit(main())

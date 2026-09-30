#!/usr/bin/env python3
"""Declare `platforms` (and `platform-reason`) in an app's tilo-app.toml.

usage: app_platforms.py APP_DIR PLATFORMS REASON
       app_platforms.py --default APP_DIR     # remove a narrowing: all platforms

PLATFORMS is comma-separated, for example `linux,macos` or `tilo-os`.
"""

from __future__ import annotations

import pathlib
import re
import sys


def set_platforms(app: pathlib.Path, platforms: list[str] | None, reason: str | None) -> None:
    path = app / "tilo-app.toml"
    text = path.read_text()
    text = re.sub(r"^platforms\s*=.*\n", "", text, flags=re.M)
    text = re.sub(r"^platform-reason\s*=.*\n", "", text, flags=re.M)
    if platforms is not None:
        package = re.search(r"^\[package\]\n(?:(?!^\[).*\n)*", text, re.M)
        assert package, path
        block = package.group(0).rstrip("\n")
        quoted = ", ".join(f'"{p}"' for p in platforms)
        escaped = reason.replace("\\", "\\\\").replace('"', '\\"')
        block += f'\nplatforms = [{quoted}]\nplatform-reason = "{escaped}"\n'
        rest = text[package.end() :]
        text = text[: package.start()] + block + ("\n" if rest and not rest.startswith("\n") else "") + rest
    path.write_text(text)


def main(argv: list[str]) -> int:
    if argv[:1] == ["--default"]:
        for app in argv[1:]:
            set_platforms(pathlib.Path(app), None, None)
        return 0
    app, platforms, reason = argv
    set_platforms(pathlib.Path(app), platforms.split(","), reason)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

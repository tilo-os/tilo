#!/usr/bin/env python3
"""Linux-only syntax and publication contracts for qOS guest PowerShell.

This deliberately does not execute guest scripts: they access Windows-only
cmdlets and registry providers.  It does parse PowerShell's structural syntax
well enough to catch unbalanced delimiters while correctly skipping comments,
quoted strings, and here-strings.  Keep policy assertions here for the small
publication boundary whose safety must not depend on a Windows VM test run.
"""

from __future__ import annotations

import pathlib
import re
import sys


OPENING = {"(": ")", "[": "]", "{": "}"}
CLOSING = {value: key for key, value in OPENING.items()}


def fail(path: pathlib.Path, line: int, message: str) -> None:
    raise ValueError(f"{path}:{line}: {message}")


def check_structure(path: pathlib.Path) -> None:
    source = path.read_text(encoding="utf-8-sig")
    stack: list[tuple[str, int]] = []
    state = "code"
    here_quote = ""
    line = 1
    column = 0
    line_start = True
    i = 0

    while i < len(source):
        char = source[i]
        next_char = source[i + 1] if i + 1 < len(source) else ""

        if state == "line-comment":
            if char == "\n":
                state = "code"
                line += 1
                column = 0
                line_start = True
            i += 1
            continue
        if state == "block-comment":
            if char == "#" and next_char == ">":
                state = "code"
                i += 2
                column += 2
                continue
            if char == "\n":
                line += 1
                column = 0
                line_start = True
            else:
                column += 1
            i += 1
            continue
        if state == "single":
            if char == "'" and next_char == "'":
                i += 2
                column += 2
                continue
            if char == "'":
                state = "code"
            if char == "\n":
                line += 1
                column = 0
                line_start = True
            else:
                column += 1
            i += 1
            continue
        if state == "double":
            if char == "`" and next_char:
                i += 2
                column += 2
                continue
            if char == '"':
                state = "code"
            if char == "\n":
                line += 1
                column = 0
                line_start = True
            else:
                column += 1
            i += 1
            continue
        if state == "here":
            if line_start and char == here_quote and next_char == "@":
                state = "code"
                i += 2
                column += 2
                continue
            if char == "\n":
                line += 1
                column = 0
                line_start = True
            else:
                column += 1
                line_start = False
            i += 1
            continue

        # Normal PowerShell code.
        if char == "#":
            state = "line-comment"
            i += 1
            column += 1
            continue
        if char == "<" and next_char == "#":
            state = "block-comment"
            i += 2
            column += 2
            continue
        if (
            line_start
            and char == "@"
            and next_char in ("'", '"')
            and i + 2 < len(source)
            and source[i + 2] in ("\r", "\n")
        ):
            state = "here"
            here_quote = next_char
            i += 2
            column += 2
            line_start = False
            continue
        if char == "'":
            state = "single"
        elif char == '"':
            state = "double"
        elif char in OPENING:
            stack.append((char, line))
        elif char in CLOSING:
            if not stack:
                fail(path, line, f"unmatched {char!r}")
            opening, opening_line = stack.pop()
            if CLOSING[char] != opening:
                fail(path, line, f"{char!r} closes {opening!r} opened on line {opening_line}")

        if char == "\n":
            line += 1
            column = 0
            line_start = True
        else:
            column += 1
            if not char.isspace():
                line_start = False
        i += 1

    if state in {"single", "double", "here", "block-comment"}:
        fail(path, line, f"unterminated {state.replace('-', ' ')}")
    if stack:
        opening, opening_line = stack[-1]
        fail(path, opening_line, f"unclosed {opening!r}")


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def check_publication_contract(guest_root: pathlib.Path) -> None:
    bridge = (guest_root / "Start-qOSGuestBridge.ps1").read_text(encoding="utf-8-sig")
    install = (guest_root / "Install-qOSRemoteApp.ps1").read_text(encoding="utf-8-sig")
    start = bridge.index("function Set-RemoteAppRegistry")
    end = bridge.index("function Publish-DiscoveredCandidate", start)
    dynamic_publication = bridge[start:end]

    require("$env:WINDIR\\System32\\WindowsPowerShell\\v1.0\\powershell.exe" in dynamic_publication,
            "dynamic publication must publish powershell.exe, never the discovered executable")
    require("$root\\Start-qOSRemoteApp.ps1" in dynamic_publication,
            "dynamic publication must use Start-qOSRemoteApp.ps1")
    require("-ApplicationId $($app.id)" in dynamic_publication,
            "dynamic publication wrapper must receive only its fixed application id")
    require("-Value ([string]$app.path)" not in dynamic_publication,
            "dynamic publication must not place the discovered executable in the RDS registry")
    require("-PasswordNeverExpires $true" not in install,
            "Set-LocalUser switch must not consume a positional boolean")
    require(re.search(r"Set-LocalUser\s+-Name\s+\$managedUser.*?-PasswordNeverExpires\s+-UserMayChangePassword\s+\$false", install) is not None,
            "managed user update must use the idempotent Set-LocalUser switch syntax")


def main() -> int:
    guest_root = pathlib.Path(sys.argv[1]) if len(sys.argv) == 2 else pathlib.Path(__file__).resolve().parents[1] / "usr/share/qos/windows/guest"
    scripts = sorted(guest_root.glob("*.ps1"))
    require(bool(scripts), f"no guest PowerShell scripts found under {guest_root}")
    for script in scripts:
        check_structure(script)
    check_publication_contract(guest_root)
    print(f"ok    PowerShell structure and fixed-wrapper contracts ({len(scripts)} scripts)")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError) as error:
        print(f"FAIL  {error}", file=sys.stderr)
        raise SystemExit(1)

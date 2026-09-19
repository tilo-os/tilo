#!/usr/bin/env python3
"""Check the installed import executable's startup contract."""

import os
from pathlib import Path
import subprocess
import sys


def close_channel():
    # close_fds also drops arbitrary inherited authority. Explicitly close the
    # receiver's reserved descriptor before exec, even if a caller supplied it.
    try:
        os.close(3)
    except OSError:
        pass


def main():
    if len(sys.argv) != 2 or not Path(sys.argv[1]).is_absolute():
        print("usage: verify-installed-imager.py /absolute installed executable", file=sys.stderr)
        return 2
    for flag, expected, timeout in (
        ("--qos-connected-lease-receiver", 2, 5),
        ("--tilo-package-smoke", 0, 10),
    ):
        try:
            result = subprocess.run(
                [sys.argv[1], flag], close_fds=True, preexec_fn=close_channel,
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                timeout=timeout, cwd="/",
            )
        except subprocess.TimeoutExpired:
            print(f"{flag} timed out after {timeout}s", file=sys.stderr)
            return 1
        except OSError as error:
            print(f"cannot execute {sys.argv[1]}: {error}", file=sys.stderr)
            return 1
        if result.returncode != expected:
            print(
                f"{flag}: expected exit {expected}, got {result.returncode}; "
                f"stdout={result.stdout!r}; stderr={result.stderr!r}", file=sys.stderr,
            )
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

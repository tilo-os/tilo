#!/usr/bin/env python3
"""Write dist/install/tilolang.sh from dist/install/tilo.sh.

Both commands come from the same releases (tools/ci/publish/cli-release.sh packs
`tilo-<os>-<arch>.tar.gz` and `tilolang-<os>-<arch>.tar.gz` under one SHA256SUMS, signed by
the release key), so both are installed by one script: how it fetches, verifies and installs is
tilo.sh's, word for word. tilolang.sh differs in its opening comment and in the values between
tilo.sh's two `# installs:` lines (what it installs and what it says once it has).

    installers.py          write tilolang.sh
    installers.py --check  fail if tilolang.sh is not what this writes
"""

from __future__ import annotations

import argparse
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
SOURCE = HERE / "tilo.sh"
TARGET = HERE / "tilolang.sh"

HEADER = """\
# Install the `tilolang` command, as `curl -fsSL https://tilolang.dev/install | sh` runs it.
#
# This script is dist/install/tilo.sh with this comment and the values between its two
# `# installs:` lines replaced (dist/install/installers.py writes it; edit tilo.sh). Everything
# tilo.sh says about how it fetches, verifies and installs holds here: the command goes to
# ~/.local/bin (or $TILO_INSTALL_DIR) without root; the archive for this machine,
# tilolang-<os>-<arch>.tar.gz, must match its line in the release's SHA256SUMS; where openssl is
# installed, SHA256SUMS must carry a signature by Tilo's release key; anything that fails stops
# the install before a file is written.
#
# `tilolang` is released with `tilo`, from the same releases ($TILO_RELEASES, default
# https://tilo.sh/releases; $TILO_VERSION picks a release other than latest), under the same
# SHA256SUMS and signature. Arguments are a command for the installed `tilolang`, run by its full
# path once it is in place: `curl -fsSL https://tilolang.dev/install | sh -s check .`.
#
# The whole script is one function called on its last line, so a download
# cut short runs nothing.
"""

VALUES = """\
  # installs: what this script installs, and what it says once it has.
  program=tilolang
  title=tilolang
  # A tilolang release packs the command alone.
  beside_programs=
  say_next() { say "Next: $1 check DIR checks the package in DIR; $1 build DIR builds it into an app."; }
  # installs: end
"""


def generated() -> str:
    text = SOURCE.read_text()
    shebang, rest = text.split("\n", 1)
    _, body = rest.split("set -eu\n", 1)
    start = body.index("  # installs: ")
    end = body.index("  # installs: end\n") + len("  # installs: end\n")
    return f"{shebang}\n{HEADER}set -eu\n{body[:start]}{VALUES}{body[end:]}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="fail if tilolang.sh is stale")
    args = parser.parse_args()
    text = generated()
    if args.check:
        if not TARGET.is_file() or TARGET.read_text() != text:
            print(f"{TARGET} is not what {pathlib.Path(__file__).name} writes from {SOURCE.name}: "
                  f"run python3 dist/install/installers.py", file=sys.stderr)
            return 1
        return 0
    TARGET.write_text(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())

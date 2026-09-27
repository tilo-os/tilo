#!/usr/bin/env python3
"""Remove transient state that bootc container lint warns about.

bootc mounts /run and /tmp as tmpfs, applies /var from the image only on
first provisioning, and expects regenerable package state to come from
systemd-tmpfiles on each boot. This script clears build-time scratch and
empties /var so the image does not retain:

* nonempty-run-tmp — NetworkManager, cups, hardware_profile __pycache__, …
* var-log — dnf5.log and other installer logs
* var-tmpfiles — packaged locks, caches, and directories without tmpfiles

Required first-boot behaviour (notably greetd's xdg-desktop-portal user
unit mask) must be declared in tmpfiles.d before this runs; deleting the
baked payload without a matching declaration would change greeter behaviour.
"""

from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path


def _clear_contents(path: Path) -> None:
    if not path.exists():
        return
    if not path.is_dir():
        raise SystemExit(f"{path} is not a directory")
    for child in path.iterdir():
        if child.is_dir() and not child.is_symlink():
            shutil.rmtree(child)
        else:
            child.unlink()


def clean_root(root: Path) -> None:
    root = root.resolve()
    var = root / "var"
    if not var.is_dir():
        raise SystemExit(f"refusing to clean {root}: /var is missing")

    for name in ("run", "tmp"):
        _clear_contents(root / name)

    # /var from the container image is first-boot only. Empty it so package
    # locks, ldconfig/swcatalog caches, and unpackaged runtime dirs are not
    # frozen into the deployment; tmpfiles.d recreates required structure.
    _clear_contents(var)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "root",
        nargs="?",
        default="/",
        type=Path,
        help="rootfs to clean (default: /)",
    )
    args = parser.parse_args(argv)
    clean_root(args.root)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

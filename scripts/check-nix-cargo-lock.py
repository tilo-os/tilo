#!/usr/bin/env python3
"""Reject a Nix vendor hash pinned to a different Cargo.lock."""

import hashlib
import re
from pathlib import Path


root = Path(__file__).resolve().parents[1]
package = (root / "dist/nix/package.nix").read_text(encoding="utf-8")
match = re.search(r'^\s*expectedCargoLockSha256 = "([0-9a-f]{64})";$', package, re.MULTILINE)
if match is None:
    raise SystemExit("dist/nix/package.nix has no Cargo.lock fingerprint")

actual = hashlib.sha256((root / "Cargo.lock").read_bytes()).hexdigest()
if actual != match.group(1):
    raise SystemExit(
        "Cargo.lock changed: refresh cargoHash and expectedCargoLockSha256 "
        "in dist/nix/package.nix"
    )

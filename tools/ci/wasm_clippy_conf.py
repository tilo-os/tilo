#!/usr/bin/env python3
"""Write the Clippy configuration the wasm32 lint pass reads.

std's clocks panic on wasm32-unknown-unknown ("time not implemented on this
platform"), and a browser page that reaches one stops. Native code may read
them freely, so the root ``clippy.toml`` cannot ban them for every target.
The wasm pass (``tools/ci/check-wasm.sh``) points ``CLIPPY_CONF_DIR`` at the
file this writes instead: the root configuration with the std clock reads
added to ``disallowed-methods``. Code that builds for the page reads the clock
through ``tilo_clock`` (or ``ui::time`` in the toolkit), which the page's own
clocks back there.

    tools/ci/wasm_clippy_conf.py OUT_DIR   # writes OUT_DIR/clippy.toml
"""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

REASON = "std has no clock in a browser page; read it through tilo_clock (ui::time in the toolkit)"
CLOCK_READS = (
    "std::time::Instant::now",
    "std::time::Instant::elapsed",
    "std::time::SystemTime::now",
    "std::time::SystemTime::elapsed",
)
OPENING = "disallowed-methods = ["


def wasm_config(root_text: str) -> str:
    """The root configuration with the clock reads added to its banned methods."""
    lines = root_text.splitlines()
    try:
        start = lines.index(OPENING)
        end = lines.index("]", start)
    except ValueError:
        raise SystemExit(f"clippy.toml: no top-level `{OPENING}` ... `]` list to extend") from None
    added = [
        "    # std's clocks panic on wasm32-unknown-unknown; this pass only.",
        *(f'    {{ path = "{path}", reason = "{REASON}" }},' for path in CLOCK_READS),
    ]
    text = "\n".join([*lines[:end], *added, *lines[end:]]) + "\n"
    banned = {entry["path"] for entry in tomllib.loads(text)["disallowed-methods"]}
    missing = set(CLOCK_READS) - banned
    if missing:
        raise SystemExit(f"generated configuration lacks {sorted(missing)}")
    return text


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    path = out / "clippy.toml"
    text = wasm_config((ROOT / "clippy.toml").read_text())
    # Clippy fingerprints the file: rewrite it only when it changes, so an
    # unchanged configuration keeps Cargo's lint cache.
    if not path.is_file() or path.read_text() != text:
        path.write_text(text)


if __name__ == "__main__":
    main()

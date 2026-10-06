#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["fonttools==4.60.1"]
# ///
"""Rebuild the catalog's matched script fallback from its pinned upstream font.

Tilo Sans, as shipped, has no Cyrillic, which falls back to a face chosen to
match it (see crates/sdk/ui-core/assets/fonts/PROVENANCE.md). Only the script
the face serves ships, plus the punctuation, digits and marks its runs need,
so the catalog does not carry a second Latin.

    uv run tools/font-fallback-subsets.py          # rebuild into the checkout
    uv run tools/font-fallback-subsets.py --check  # rebuild and compare

The upstream files are fetched from google/fonts at a pinned commit and checked
against their SHA-256 before anything is derived from them. The output is
deterministic: head.modified keeps the upstream timestamp.
"""

import argparse
import hashlib
import io
import sys
import urllib.parse
import urllib.request
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont

GOOGLE_FONTS = "9710da1eacb3be272583c3224dcb70f9da6eadbb"
ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "crates/sdk/ui-core/assets/fonts"

# What every fallback run carries besides its own script: space, ASCII
# punctuation and digits, Latin-1 punctuation and signs, the apostrophe
# letter Ukrainian uses, combining marks, general punctuation, and the
# currency and letterlike signs that sit inside running text.
COMMON = [
    (0x0020, 0x0040),
    (0x005B, 0x0060),
    (0x007B, 0x007E),
    (0x00A0, 0x00BF),
    (0x00D7, 0x00D7),
    (0x00F7, 0x00F7),
    (0x02BC, 0x02BC),
    (0x0300, 0x036F),
    (0x2000, 0x206F),
    (0x20A0, 0x20C0),
    (0x2116, 0x2116),
    (0x2122, 0x2122),
]

FALLBACKS = [
    {
        "output": "Onest-Cyrillic[wght].ttf",
        "source": "ofl/onest/Onest[wght].ttf",
        "sha256": "966c5c29b4755da84b6854d5c21dd4eaa2420225d0e9874de602de176d4a9f31",
        "script": [(0x0400, 0x052F), (0x1C80, 0x1C8F), (0x2DE0, 0x2DFF), (0xA640, 0xA69F)],
    },
]


def fetch(path, sha256):
    url = f"https://raw.githubusercontent.com/google/fonts/{GOOGLE_FONTS}/{urllib.parse.quote(path)}"
    data = urllib.request.urlopen(url).read()
    actual = hashlib.sha256(data).hexdigest()
    if actual != sha256:
        raise SystemExit(f"{path}: SHA-256 {actual} is not the pinned {sha256}")
    return data


def codepoints(ranges):
    return [c for lo, hi in ranges for c in range(lo, hi + 1)]


def build(spec):
    font = TTFont(io.BytesIO(fetch(spec["source"], spec["sha256"])), recalcTimestamp=False)
    options = subset.Options()
    options.layout_features = ["*"]
    options.name_IDs = ["*"]
    options.name_languages = ["*"]
    options.name_legacy = True
    options.notdef_outline = True
    options.recalc_timestamp = False
    subsetter = subset.Subsetter(options)
    subsetter.populate(unicodes=codepoints(COMMON + spec["script"]))
    subsetter.subset(font)
    out = io.BytesIO()
    font.save(out)
    return out.getvalue()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="fail if the checkout differs")
    args = parser.parse_args()
    stale = []
    for spec in FALLBACKS:
        data = build(spec)
        target = CATALOG / spec["output"]
        digest = hashlib.sha256(data).hexdigest()
        print(f"{spec['output']}: {len(data)} bytes, sha256 {digest}")
        if args.check:
            if not target.is_file() or target.read_bytes() != data:
                stale.append(spec["output"])
        else:
            target.write_bytes(data)
    if stale:
        sys.exit(f"out of date: {', '.join(stale)}")


if __name__ == "__main__":
    main()

#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["fonttools==4.60.1", "brotli==1.2.1"]
# ///
"""Rebuild the web page's cuts of Tilo Sans from the catalog's pinned files.

A browser that opens a workspace's page has no Tilo Sans of its own, so the
host that serves the page serves the face beside it. The page carries nothing
it does not draw with: width stays at its default (the page never condenses
text), the weight axis stays whole (every role weight), and each style is cut
in two by script, so a page in English or German loads only the Latin file
and the browser fetches the other only when a page shows a letter from it.

    uv run tools/font-web-cuts.py          # rebuild candidates outside source
    uv run tools/font-web-cuts.py --check  # rebuild and compare

The sources are the catalog's own Tilo Sans files, checked against their
pinned SHA-256 before anything is derived from them. The output is
deterministic: head.modified keeps the source's timestamp. Each cut's
`unicode-range` in crates/ui/ui-core/src/composition/page.css is the one this
script prints, which `--check` holds it to.
"""

import argparse
import hashlib
import importlib.util
import io
import sys
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "crates/ui/ui-core/assets/fonts"
PAGE_CSS = ROOT / "crates/ui/ui-core/src/composition/page.css"

# Basic Latin and Latin-1 with the marks, punctuation and signs that sit in
# running text: everything English and German need, and most of what a
# Western European name does.
LATIN = [
    (0x0000, 0x00FF),
    (0x0131, 0x0131),
    (0x0152, 0x0153),
    (0x02BB, 0x02BC),
    (0x02C6, 0x02C6),
    (0x02DA, 0x02DA),
    (0x02DC, 0x02DC),
    (0x0304, 0x0304),
    (0x0308, 0x0308),
    (0x0329, 0x0329),
    (0x2000, 0x206F),
    (0x20AC, 0x20AC),
    (0x2122, 0x2122),
    (0x2191, 0x2191),
    (0x2193, 0x2193),
    (0x2212, 0x2212),
    (0x2215, 0x2215),
    (0xFEFF, 0xFEFF),
    (0xFFFD, 0xFFFD),
]

SOURCES = [
    ("TiloSans[wdth,wght].ttf", "TiloSans"),
    ("TiloSans-Italic[wdth,wght].ttf", "TiloSans-Italic"),
]


def codepoints(ranges):
    return {c for lo, hi in ranges for c in range(lo, hi + 1)}


def ranges_of(points):
    """`points` as the fewest `U+XXXX-YYYY` spans, for a `unicode-range`."""
    spans = []
    for point in sorted(points):
        if spans and spans[-1][1] + 1 == point:
            spans[-1][1] = point
        else:
            spans.append([point, point])
    return ",".join(f"U+{lo:04X}" if lo == hi else f"U+{lo:04X}-{hi:04X}" for lo, hi in spans)


def cut(source, unicodes):
    font = TTFont(io.BytesIO(source), recalcTimestamp=False)
    instanced = io.BytesIO()
    instancer.instantiateVariableFont(font, {"wdth": 100}).save(instanced)
    font = TTFont(instanced, recalcTimestamp=False)
    options = subset.Options()
    options.layout_features = ["*"]
    options.name_IDs = ["*"]
    options.name_languages = ["*"]
    options.name_legacy = True
    options.notdef_outline = True
    options.recalc_timestamp = False
    subsetter = subset.Subsetter(options)
    subsetter.populate(unicodes=sorted(unicodes))
    subsetter.subset(font)
    font.flavor = "woff2"
    out = io.BytesIO()
    font.save(out)
    return out.getvalue()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="fail if the catalog's cuts differ")
    parser.add_argument("--out", type=Path, default=ROOT / "target/font-candidates",
                        help="write regenerated binary candidates outside source")
    args = parser.parse_args()
    spec = importlib.util.spec_from_file_location("font_catalog", ROOT / "tools/font-catalog.py")
    catalog_module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(catalog_module)
    # The fixture catalog holds the pinned sources, every byte checked against its pin.
    pinned = catalog_module.prepared_root(CATALOG, "fixtures")
    reviewed = catalog_module.prepared_root(CATALOG, "web") if args.check else args.out
    reviewed.mkdir(parents=True, exist_ok=True)
    stale = []
    for file, stem in SOURCES:
        source = (pinned / file).read_bytes()
        covered = set(TTFont(io.BytesIO(source)).getBestCmap())
        latin = covered & codepoints(LATIN)
        for name, unicodes in [("Latin", latin), ("Extended", covered - latin)]:
            output = f"{stem}-{name}.woff2"
            data = cut(source, unicodes)
            digest = hashlib.sha256(data).hexdigest()
            print(f"{output}: {len(data)} bytes, sha256 {digest}")
            print(f"  unicode-range:{ranges_of(unicodes)}")
            target = reviewed / output
            if args.check:
                if not target.is_file() or target.read_bytes() != data:
                    stale.append(output)
                rule = f'url("/fonts/{output}") format("woff2");unicode-range:{ranges_of(unicodes)};'
                if rule not in PAGE_CSS.read_text():
                    stale.append(f"{PAGE_CSS.relative_to(ROOT)} ({output})")
            else:
                target.write_bytes(data)
    if stale:
        sys.exit(f"out of date: {', '.join(stale)}")


if __name__ == "__main__":
    main()

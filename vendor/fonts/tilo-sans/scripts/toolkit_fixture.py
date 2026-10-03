# /// script
# requires-python = ">=3.11"
# dependencies = ["fonttools>=4.60"]
# ///
"""Write the UI toolkit's test face for features Tilo Sans holds back.

    uv run vendor/fonts/tilo-sans/scripts/toolkit_fixture.py    # after build.py

The toolkit picks a text kind's OpenType features from what the face has
(crates/sdk/ui-core/src/font_catalog/faces.rs). The shipped Tilo Sans has
no distinct I and l (`ss13`, `cv01`, `cv02`) and no slashed zero (`zero`)
yet, so its tests use this face instead: the full build's Regular, a few
letters with those features and `tnum`, renamed so it can never be taken for
the shipped face. Written to
crates/sdk/ui-core/tests/fixtures/fonts/TiloSansFeaturesFixture-Regular.ttf.
"""

from __future__ import annotations

import io
import sys
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "fonts" / "full" / "TiloSans[wdth,wght].ttf"
OUT = ROOT.parent.parent.parent / "crates/sdk/ui-core/tests/fixtures/fonts/TiloSansFeaturesFixture-Regular.ttf"
FAMILY = "Tilo Sans Features Fixture"
POSTSCRIPT = "TiloSansFeaturesFixture-Regular"
TEXT = "Il1|0OĲĺłHnox "
FEATURES = ["ss13", "cv01", "cv02", "zero", "tnum"]


def main() -> int:
    if not SOURCE.exists():
        print(f"{SOURCE} is missing: run build.py first", file=sys.stderr)
        return 1
    font = TTFont(SOURCE, recalcTimestamp=False)
    font = instancer.instantiateVariableFont(font, {"wght": 400, "wdth": 100})
    options = subset.Options()
    options.layout_features = FEATURES
    options.name_IDs = [0, 1, 2, 3, 4, 5, 6, 13, 14]
    options.notdef_outline = True
    options.hinting = False
    options.recalc_timestamp = False
    subsetter = subset.Subsetter(options)
    subsetter.populate(text=TEXT)
    subsetter.subset(font)
    name = font["name"]
    for record in list(name.names):
        if record.nameID in (1, 4):
            name.setName(FAMILY if record.nameID == 1 else f"{FAMILY} Regular",
                         record.nameID, record.platformID, record.platEncID, record.langID)
        elif record.nameID == 3:
            name.setName(f"1.001;{POSTSCRIPT}", record.nameID, record.platformID, record.platEncID, record.langID)
        elif record.nameID == 6:
            name.setName(POSTSCRIPT, record.nameID, record.platformID, record.platEncID, record.langID)
    out = io.BytesIO()
    font.save(out)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_bytes(out.getvalue())
    print(f"{OUT.relative_to(ROOT.parent.parent.parent)}: {len(out.getvalue())} bytes")
    return 0


if __name__ == "__main__":
    sys.exit(main())

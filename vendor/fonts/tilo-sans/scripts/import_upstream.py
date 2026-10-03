"""Convert the pinned Instrument Sans Glyphs sources into the Tilo Sans UFO sources.

This records how sources/ was produced. It is not part of the edit/build loop:
running it again overwrites every master with upstream's outlines and discards
all Tilo Sans work. Use it only to re-derive the baseline from a newer
upstream commit, then re-apply Tilo Sans changes on top.

    uv run --with glyphsLib --with ufoLib2 --with fonttools \
        vendor/fonts/tilo-sans/scripts/import_upstream.py --force

The upstream checkout lives in vendor/fonts/tilo-sans/.upstream/ (ignored);
the script clones it at UPSTREAM_COMMIT when it is missing.
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
from pathlib import Path

import glyphsLib
import ufoLib2
from fontTools.designspaceLib import (
    AxisLabelDescriptor,
    DesignSpaceDocument,
)

UPSTREAM_URL = "https://github.com/Instrument/instrument-sans"
UPSTREAM_COMMIT = "7fa22308a3d0c94ee2b3cd537a1196b65db34a3e"

ROOT = Path(__file__).resolve().parent.parent
SOURCES = ROOT / "sources"
FEATURES = SOURCES / "features"
UPSTREAM = ROOT / ".upstream" / "instrument-sans"

FAMILY = "Tilo Sans"
FILE_FAMILY = "TiloSans"

COPYRIGHT = (
    "Copyright 2022 The Instrument Sans Project Authors "
    "(https://github.com/Instrument/instrument-sans). "
    "Copyright 2026 The Tilo Sans Project Authors."
)
DESIGNER = "Rodrigo Fuenzalida, Jordan Egstad (Instrument Sans); The Tilo Sans Project Authors"
DESCRIPTION = (
    "Tilo Sans extends Instrument Sans by Rodrigo Fuenzalida and Jordan Egstad "
    "(https://github.com/Instrument/instrument-sans) with additional scripts and "
    "forms for the Tilo desktop. Licensed under the SIL Open Font License 1.1."
)
LICENSE = (
    "This Font Software is licensed under the SIL Open Font License, Version 1.1. "
    "This license is available with a FAQ at: https://openfontlicense.org"
)
LICENSE_URL = "https://openfontlicense.org"

# Font-level lib keys that only matter to Glyphs.app or its plugins.
DROP_LIB_PREFIXES = ("com.schriftgestaltung.", "KernOn", "GSCornerRadius")

# STAT labels, as upstream's gftools config declared them.
WIDTH_LABELS = [("Condensed", 75, 0), ("SemiCondensed", 87.5, 0), ("Normal", 100, 2)]
WEIGHT_LABELS = [("Regular", 400, 2), ("Medium", 500, 0), ("SemiBold", 600, 0), ("Bold", 700, 0)]

SOURCE_FILES = {
    # upstream master name -> our UFO stem
    "Condensed Regular": "TiloSans-CondensedRegular",
    "Condensed Bold": "TiloSans-CondensedBold",
    "Regular": "TiloSans-Regular",
    "Bold": "TiloSans-Bold",
    "Condensed Italic": "TiloSans-CondensedItalic",
    "Condensed Bold Italic": "TiloSans-CondensedBoldItalic",
    "Italic": "TiloSans-Italic",
    "Bold Italic": "TiloSans-BoldItalic",
}


def ensure_upstream() -> None:
    if not UPSTREAM.exists():
        UPSTREAM.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["git", "clone", UPSTREAM_URL, str(UPSTREAM)], check=True)
    subprocess.run(["git", "-C", str(UPSTREAM), "fetch", "--quiet", "origin"], check=False)
    subprocess.run(["git", "-C", str(UPSTREAM), "checkout", "--quiet", UPSTREAM_COMMIT], check=True)


def rename_info(ufo: ufoLib2.Font, style: str) -> None:
    info = ufo.info
    info.familyName = FAMILY
    info.styleName = style
    if info.styleMapFamilyName:
        info.styleMapFamilyName = info.styleMapFamilyName.replace("Instrument Sans", FAMILY)
    info.copyright = COPYRIGHT
    info.trademark = None
    info.openTypeNameDesigner = DESIGNER
    info.openTypeNameDesignerURL = "https://www.rfuenzalida.com"
    info.openTypeNameManufacturer = "The Tilo Sans Project Authors"
    info.openTypeNameManufacturerURL = None
    info.openTypeNameDescription = DESCRIPTION
    info.openTypeNameLicense = LICENSE
    info.openTypeNameLicenseURL = LICENSE_URL
    info.openTypeOS2Type = []  # installable embedding, as upstream (fsType 0)
    info.versionMajor = 1
    info.versionMinor = 1
    info.openTypeNameVersion = None
    info.openTypeNameUniqueID = None
    info.postscriptFontName = None
    info.postscriptFullName = None


def drop_stray_slant(ufo: ufoLib2.Font) -> None:
    """Reset the 0.13 degree slant upstream's italic i/j components carry.

    It is a Glyphs UI leftover; the glyphsLib that built upstream's fonts
    ignored component slant, so their i has upright components. Keeping it
    would move the dot by a unit or two against every shipped Instrument Sans.
    """
    for glyph in ufo:
        for component in glyph.components:
            xx, xy, yx, yy, dx, dy = component.transformation
            if (xx, yy, xy) == (1, 1, 0) and 0 < abs(yx) < 0.01:
                component.transformation = (1, 0, 0, 1, dx, dy)


def clean_lib(ufo: ufoLib2.Font) -> None:
    # Today's glyphsLib classifies spacing accents (grave, acute, ...) as
    # bases; the build that produced upstream's fonts left them unclassified
    # and without anchors, and so does build.py.
    categories = ufo.lib.get("public.openTypeCategories", {})
    for name in list(categories):
        info = glyphsLib.glyphdata.get_glyph(name)
        if info.category == "Mark" and info.subCategory == "Spacing" and not ufo[name].anchors:
            del categories[name]
    for key in list(ufo.lib):
        if key.startswith(DROP_LIB_PREFIXES):
            del ufo.lib[key]
    for glyph in ufo:
        for key in list(glyph.lib):
            # originalWidth: Glyphs' width of a mark before glyphsLib zeroed it.
            if key.startswith("com.schriftgestaltung.") and key != "com.schriftgestaltung.Glyphs.ComponentInfo":
                del glyph.lib[key]


def split_features(text: str) -> dict[str, str]:
    """Split glyphsLib's features.fea into one file per top-level block."""
    blocks: dict[str, str] = {}
    lang = []
    body = text
    # Leading languagesystem statements.
    m = re.match(r"(?s)(.*?)(?=^feature |^lookup |^@)", body, re.M)
    if m:
        lang = [line for line in m.group(1).splitlines() if line.startswith("languagesystem")]
        body = body[m.end():]
    blocks["languagesystems"] = "\n".join(lang) + "\n"
    for fm in re.finditer(r"(?ms)^feature (\w+) \{\n(.*?)^\} \1;\n", body):
        tag, inner = fm.group(1), fm.group(2)
        # Upstream indents with a mix of tabs and four spaces; one style keeps
        # the upright and italic code byte-identical so both can share a file.
        inner = "\n".join(
            re.sub(r"^(?: {4})+", lambda s: "\t" * (len(s.group(0)) // 4), line)
            for line in inner.splitlines()
            if line.strip() != "# automatic"
        ).strip("\n")
        blocks[tag] = f"feature {tag} {{\n{inner}\n}} {tag};\n"
    leftovers = re.sub(r"(?ms)^feature (\w+) \{\n.*?^\} \1;\n", "", body).strip()
    if leftovers:
        raise SystemExit(f"unrecognised feature code outside feature blocks:\n{leftovers[:500]}")
    return blocks


def write_features(designspace_name: str, ufos: list[ufoLib2.Font]) -> str:
    texts = {ufo.features.text for ufo in ufos}
    if len(texts) != 1:
        raise SystemExit(f"{designspace_name}: masters carry different feature code")
    blocks = split_features(texts.pop())
    FEATURES.mkdir(parents=True, exist_ok=True)
    order = list(blocks)
    for tag, code in blocks.items():
        path = FEATURES / f"{tag}.fea"
        if path.exists() and path.read_text() != code:
            raise SystemExit(f"{path.name}: upright and italic differ; keep them in separate files")
        path.write_text(code)
    return "\n".join(f"include(features/{tag}.fea);" for tag in order) + "\n"


def convert(glyphs_name: str, designspace_name: str) -> None:
    font = glyphsLib.GSFont(str(UPSTREAM / "sources" / glyphs_name))
    ds = glyphsLib.to_designspace(
        font,
        ufo_module=ufoLib2,
        family_name=None,
        minimal=True,
        generate_GDEF=True,
        store_editor_state=False,
        write_skipexportglyphs=True,
        propagate_anchors=False,
    )
    out = DesignSpaceDocument()
    out.formatVersion = "5.0"
    for axis in ds.axes:
        labels = WIDTH_LABELS if axis.tag == "wdth" else WEIGHT_LABELS
        axis.axisLabels = [
            AxisLabelDescriptor(
                name=name,
                userValue=value,
                elidable=bool(flags & 2),
                linkedUserValue=700 if (axis.tag == "wght" and value == 400) else None,
            )
            for name, value, flags in labels
        ]
        out.addAxis(axis)

    ufos = []
    for source in ds.sources:
        master_name = source.font.info.styleName
        stem = SOURCE_FILES[master_name]
        rename_info(source.font, master_name)
        clean_lib(source.font)
        drop_stray_slant(source.font)
        source.filename = f"{stem}.ufo"
        source.path = str(SOURCES / source.filename)
        source.familyName = FAMILY
        source.name = master_name
        source.copyLib = source.copyInfo = source.copyGroups = source.copyFeatures = False
        ufos.append(source.font)
        out.addSource(source)

    include = write_features(designspace_name, ufos)
    for source in out.sources:
        ufo = source.font
        ufo.features.text = include
        target = Path(source.path)
        if target.exists():
            shutil.rmtree(target)
        ufo.save(target, overwrite=True)

    italic = "Italic" in glyphs_name
    width_names = {75: "Condensed", 88: "SemiCondensed", 100: ""}
    weight_names = {400: "Regular", 500: "Medium", 600: "SemiBold", 700: "Bold"}
    for instance in ds.instances:
        # Upstream's variable fonts name only the normal-width weights in fvar
        # (gftools dropped the rest); the narrower widths stay reachable
        # through the axis and are labelled in STAT.
        if instance.location["Width"] != 100:
            continue
        width = width_names[round(instance.location["Width"])]
        weight = weight_names[round(instance.location["Weight"])]
        if italic:
            weight = "Italic" if weight == "Regular" else f"{weight} Italic"
        instance.styleName = " ".join(part for part in (width, weight) if part)
        instance.familyName = FAMILY
        instance.filename = None
        instance.path = None
        instance.postScriptFontName = None
        instance.styleMapFamilyName = None
        instance.styleMapStyleName = None
        instance.name = None
        instance.lib.clear()
        out.addInstance(instance)

    out.write(SOURCES / designspace_name)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--force", action="store_true", help="overwrite existing sources")
    args = parser.parse_args()
    if SOURCES.exists() and any(SOURCES.glob("*.ufo")) and not args.force:
        print("sources/ already holds UFOs; pass --force to overwrite them with upstream", file=sys.stderr)
        return 1
    ensure_upstream()
    SOURCES.mkdir(parents=True, exist_ok=True)
    if FEATURES.exists():
        shutil.rmtree(FEATURES)
    convert("InstrumentSans.glyphs", "TiloSans.designspace")
    convert("InstrumentSans-Italic.glyphs", "TiloSans-Italic.designspace")
    ofl = (UPSTREAM / "OFL.txt").read_text().splitlines(keepends=True)
    ofl.insert(1, "Copyright 2026 The Tilo Sans Project Authors\n")
    (ROOT / "OFL.txt").write_text("".join(ofl))
    return 0


if __name__ == "__main__":
    sys.exit(main())

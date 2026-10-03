# /// script
# requires-python = ">=3.11"
# dependencies = ["ufo2ft>=3.9", "ufoLib2", "fonttools[ufo,unicode]>=4.60", "glyphsLib>=6.15", "scipy",
#                 "skia-pathops", "shapely>=2", "uharfbuzz", "numpy"]
# ///
"""Compile the Tilo Sans sources into variable TTFs.

    uv run vendor/fonts/tilo-sans/build.py            # upright + italic
    uv run vendor/fonts/tilo-sans/build.py --upright  # one of them
    uv run vendor/fonts/tilo-sans/build.py --check    # lint the sources only
    uv run vendor/fonts/tilo-sans/build.py --fast     # skip the slower checks while iterating

Output: vendor/fonts/tilo-sans/fonts/TiloSans[wdth,wght].ttf and
TiloSans-Italic[wdth,wght].ttf (ignored by git).

What the build fills in, so a new glyph needs only its .glif files (one per
master), its contents.plist entry, and its feature/kerning code:

* glyph order: glyphs missing from public.glyphOrder are appended, sorted by
  code point (an unencoded alternate sorts with its base glyph);
* production names: glyphs missing from public.postscriptNames get Glyphs'
  production name (A-cy -> uni0410, Alphatonos -> Alphatonos, ...);
* GDEF classes: glyphs missing from public.openTypeCategories are classified
  the way Glyphs does it (combining marks -> mark, ligatures with anchors ->
  ligature, anything with an attaching anchor of its own or through a
  component -> base);
* anchors: composites inherit their components' anchors (Glyphs' anchor
  propagation), so Ấ = A + circumflexcomb + acutecomb needs no anchors of its
  own. Anchors already on a composite win over propagated ones.

The lint step stops the build on errors a designer can make by hand (glyph
missing from a master, incompatible masters, duplicate code points) and prints
warnings for the subtler ones (mark with an advance width, wrong start point).

Two optical checks follow (skipped with --fast):

* scripts/components.py, on the sources: the parts of every accented glyph
  keep their distance (a tone clear of the breve under it, a capital accent
  clear of its letter) in every master and between them, the stacked tones
  lean like the master's own, and no stack rises above 1030;
* scripts/pairs.py, on the compiled fonts: no two neighbouring letters
  collide once kerned (a tone mark against an ascender, a horn against the
  next letter, ĩ against T). `scripts/kerning.py` writes the kerning that opens
  them. The fonts are written either way, so a failing pair can be looked at.
"""

from __future__ import annotations

import argparse
import plistlib
import subprocess
import sys
import time
import unicodedata
from pathlib import Path

import ufo2ft
import ufoLib2
from fontTools import ttLib
from fontTools.designspaceLib import DesignSpaceDocument
from fontTools.otlLib.builder import buildStatTable
from fontTools.pens.recordingPen import RecordingPen
from fontTools.ttLib.tables import ttProgram
from ufo2ft.filters.propagateAnchors import PropagateAnchorsFilter

sys.path.insert(0, str(Path(__file__).resolve().parent / "scripts"))
import components  # noqa: E402
import pairs  # noqa: E402

ROOT = Path(__file__).resolve().parent
SOURCES = ROOT / "sources"
FONTS = ROOT / "fonts"

FILTERS_KEY = "com.github.googlei18n.ufo2ft.filters"
CATEGORIES_KEY = "public.openTypeCategories"
ORDER_KEY = "public.glyphOrder"
PSNAMES_KEY = "public.postscriptNames"
GLYPHS_CATEGORY_KEY = "com.schriftgestaltung.Glyphs.category"
GLYPHS_SUBCATEGORY_KEY = "com.schriftgestaltung.Glyphs.subCategory"

WIDTH_VALUES = [
    {"value": 75, "name": "Condensed"},
    {"value": 87.5, "name": "SemiCondensed"},
    {"value": 100, "name": "Normal", "flags": 0x2},
]
WEIGHT_VALUES = [
    {"value": 400, "name": "Regular", "flags": 0x2, "linkedValue": 700},
    {"value": 500, "name": "Medium"},
    {"value": 600, "name": "SemiBold"},
    {"value": 700, "name": "Bold"},
]


class Family:
    def __init__(self, designspace: str, italic: bool):
        self.designspace = SOURCES / designspace
        self.italic = italic

    @property
    def output(self) -> Path:
        name = "TiloSans-Italic[wdth,wght].ttf" if self.italic else "TiloSans[wdth,wght].ttf"
        return FONTS / name


FAMILIES = {
    "upright": Family("TiloSans.designspace", italic=False),
    "italic": Family("TiloSans-Italic.designspace", italic=True),
}


# ---------------------------------------------------------------- sources


def load(family: Family) -> DesignSpaceDocument:
    ds = DesignSpaceDocument.fromfile(family.designspace)
    ds.loadSourceFonts(ufoLib2.Font.open)
    return ds


def default_font(ds: DesignSpaceDocument) -> ufoLib2.Font:
    return ds.findDefault().font


def glyph_info(name: str, unicodes: list[int]):
    from glyphsLib import glyphdata

    return glyphdata.get_glyph(name, unicodes=[f"{u:04X}" for u in unicodes])


def base_codepoint(font: ufoLib2.Font, name: str) -> int:
    """Code point a glyph sorts by: its own, or that of the name before the suffix."""
    probe = name
    while True:
        if probe in font and font[probe].unicodes:
            return font[probe].unicodes[0]
        if "." not in probe:
            break
        probe = probe.rsplit(".", 1)[0]
    info = glyph_info(probe, [])
    if info.unicode:
        return int(info.unicode, 16)
    return 0x110000


def has_attaching_anchor(font: ufoLib2.Font, name: str, seen: set[str] | None = None) -> bool:
    seen = seen or set()
    if name in seen or name not in font:
        return False
    seen.add(name)
    glyph = font[name]
    if any(a.name and not a.name.startswith("_") for a in glyph.anchors):
        return True
    return any(has_attaching_anchor(font, c.baseGlyph, seen) for c in glyph.components)


def is_spacing_mark(font: ufoLib2.Font, name: str) -> bool:
    glyph = font[name]
    info = glyph_info(name, list(glyph.unicodes))
    category = glyph.lib.get(GLYPHS_CATEGORY_KEY) or info.category
    sub_category = glyph.lib.get(GLYPHS_SUBCATEGORY_KEY) or info.subCategory
    return category == "Mark" and sub_category == "Spacing" and not glyph.anchors


def category_for(font: ufoLib2.Font, name: str) -> str | None:
    glyph = font[name]
    info = glyph_info(name, list(glyph.unicodes))
    category = glyph.lib.get(GLYPHS_CATEGORY_KEY) or info.category
    sub_category = glyph.lib.get(GLYPHS_SUBCATEGORY_KEY) or info.subCategory
    attaching = has_attaching_anchor(font, name)
    if sub_category == "Ligature" and attaching:
        return "ligature"
    if category == "Mark" and sub_category in ("Nonspacing", "Spacing Combining"):
        return "mark"
    if attaching:
        return "base"
    return None


def complete_lib(ds: DesignSpaceDocument) -> list[str]:
    """Fill glyph order, production names and GDEF classes for new glyphs."""
    default = default_font(ds)
    lib = default.lib
    order = list(lib.get(ORDER_KEY, []))
    known = set(order)
    added = sorted(
        (name for name in default.keys() if name not in known),
        key=lambda n: (base_codepoint(default, n), n),
    )
    order = [name for name in order if name in default] + added

    ps_names = dict(lib.get(PSNAMES_KEY, {}))
    used = set(ps_names.values())
    for name in order:
        if name in ps_names:
            continue
        production = glyph_info(name, list(default[name].unicodes)).production_name or name
        if production in used and ps_names.get(name) != production:
            production = name.replace("-", "_")
        ps_names[name] = production
        used.add(production)

    spacing_marks = sorted(name for name in default.keys() if is_spacing_mark(default, name))
    categories = dict(lib.get(CATEGORIES_KEY, {}))
    for name in default.keys():
        if name in spacing_marks:
            continue
        if name in categories:
            continue
        category = category_for(default, name)
        if category:
            categories[name] = category

    for source in ds.sources:
        font = source.font
        font.lib[ORDER_KEY] = order
        font.lib[PSNAMES_KEY] = ps_names
        font.lib[CATEGORIES_KEY] = categories
        # In place on the loaded masters, not as a compile filter: the
        # variable mark writer reads anchor positions from the masters
        # themselves, so a filter's propagated anchors would never reach it.
        # Spacing accents (grave, acute, ...) are drawn as a single mark
        # component but stand alone: they get no anchors, as upstream.
        PropagateAnchorsFilter(exclude=spacing_marks)(font)
        # Upstream's fonts carry no scaled, flipped or rotated components
        # (parenright is an outline, not a mirrored parenleft); renderers
        # disagree on how to round them.
        filters = [f for f in font.lib.get(FILTERS_KEY, []) if f.get("name") != "decomposeTransformedComponents"]
        filters.append({"name": "decomposeTransformedComponents", "pre": True})
        font.lib[FILTERS_KEY] = filters
    return added


# ---------------------------------------------------------------- lint


def lint(ds: DesignSpaceDocument, label: str) -> tuple[list[str], list[str]]:
    errors: list[str] = []
    warnings: list[str] = []
    default = default_font(ds)
    names = set(default.keys())

    for source in ds.sources:
        # A .glif that contents.plist does not list is silently ignored by
        # every UFO reader: the glyph would just be absent from the font.
        glyphs_dir = Path(source.path) / "glyphs"
        listed = set(plistlib.loads((glyphs_dir / "contents.plist").read_bytes()).values())
        unlisted = sorted(p.name for p in glyphs_dir.glob("*.glif") if p.name not in listed)
        if unlisted:
            errors.append(f"{source.filename}: .glif files missing from glyphs/contents.plist: {' '.join(unlisted[:20])}"
                          " (add `<key>NAME</key><string>FILE.glif</string>`, or save the glyph with ufoLib2)")

    for source in ds.sources:
        font = source.font
        here = set(font.keys())
        missing = sorted(names - here)
        extra = sorted(here - names)
        if missing:
            errors.append(f"{source.filename}: missing {len(missing)} glyph(s): {' '.join(missing[:20])}")
        if extra:
            errors.append(f"{source.filename}: glyph(s) absent from the default master: {' '.join(extra[:20])}")

    seen: dict[int, str] = {}
    for name in sorted(names):
        for u in default[name].unicodes:
            if u in seen:
                errors.append(f"U+{u:04X} is mapped by both {seen[u]} and {name}")
            seen[u] = name

    categories = default.lib.get(CATEGORIES_KEY, {})
    for name in sorted(names):
        for source in ds.sources:
            glyph = source.font[name] if name in source.font else None
            if glyph is None:
                continue
            for comp in glyph.components:
                if comp.baseGlyph not in source.font:
                    errors.append(f"{source.filename}: {name} uses missing component {comp.baseGlyph}")
            if categories.get(name) == "mark" and glyph.width != 0 and unicodedata.category(chr(glyph.unicodes[0]) if glyph.unicodes else "a") == "Mn":
                warnings.append(f"{source.filename}: combining mark {name} has advance {glyph.width}; marks are zero-width")

    # Structural compatibility: same contours, points, components and anchors.
    for name in sorted(names):
        shapes = {}
        for source in ds.sources:
            if name not in source.font:
                continue
            glyph = source.font[name]
            pen = RecordingPen()
            glyph.draw(pen)
            ops = tuple(op for op, _ in pen.value if op != "addComponent")
            comps = tuple(c.baseGlyph for c in glyph.components)
            anchors = tuple(sorted(a.name for a in glyph.anchors))
            shapes[source.filename] = (ops, comps, anchors)
        variants = set(shapes.values())
        if len(variants) > 1:
            ref = shapes[ds.findDefault().filename]
            for filename, shape in shapes.items():
                if shape == ref:
                    continue
                what = []
                if shape[0] != ref[0]:
                    what.append(f"outline ({len(shape[0])} vs {len(ref[0])} segments)")
                if shape[1] != ref[1]:
                    what.append(f"components {list(shape[1])} vs {list(ref[1])}")
                if shape[2] != ref[2]:
                    what.append(f"anchors {list(shape[2])} vs {list(ref[2])}")
                errors.append(f"{filename}: {name} is incompatible with the default master: {', '.join(what)}")
    return errors, warnings


def interpolation_report(family: Family) -> str:
    """fontTools' interpolatable check: wrong start points, contour order,
    kinks, contours that turn inside out between masters. The structural lint
    only proves the masters are compatible; this proves they interpolate
    cleanly."""
    result = subprocess.run(
        [sys.executable, "-m", "fontTools.varLib.interpolatable", str(family.designspace)],
        capture_output=True,
        text=True,
    )
    return (result.stdout + result.stderr).strip()


# ---------------------------------------------------------------- compile


def fix_stat(font: ttLib.TTFont, italic: bool) -> None:
    ital = {"value": 1, "name": "Italic"} if italic else {"value": 0, "name": "Roman", "flags": 0x2, "linkedValue": 1}
    axes = [
        {"tag": "wdth", "name": "Width", "ordering": 0, "values": WIDTH_VALUES},
        {"tag": "wght", "name": "Weight", "ordering": 1, "values": WEIGHT_VALUES},
        {"tag": "ital", "name": "Italic", "ordering": 2, "values": [ital]},
    ]
    if "STAT" in font:
        del font["STAT"]
    buildStatTable(font, axes, elidedFallbackName=2, macNames=False)


def fix_unhinted(font: ttLib.TTFont) -> None:
    """Smooth rendering without hints: what gftools gave upstream's fonts."""
    gasp = ttLib.newTable("gasp")
    gasp.gaspRange = {0xFFFF: 0x000F}
    font["gasp"] = gasp
    prep = ttLib.newTable("prep")
    program = ttProgram.Program()
    program.fromAssembly(["PUSHW[]", "511", "SCANCTRL[]", "PUSHB[]", "4", "SCANTYPE[]"])
    prep.program = program
    font["prep"] = prep
    font["maxp"].maxZones = 1


def drop_unused_names(font: ttLib.TTFont) -> None:
    used = {n for n in range(0, 26)}
    if "fvar" in font:
        for axis in font["fvar"].axes:
            used.add(axis.axisNameID)
        for inst in font["fvar"].instances:
            used.add(inst.subfamilyNameID)
            if inst.postscriptNameID != 0xFFFF:
                used.add(inst.postscriptNameID)
    stat = font["STAT"].table
    for axis in stat.DesignAxisRecord.Axis:
        used.add(axis.AxisNameID)
    for value in stat.AxisValueArray.AxisValue:
        used.add(value.ValueNameID)
    used.add(stat.ElidedFallbackNameID)
    if "GSUB" in font:
        for record in font["GSUB"].table.FeatureList.FeatureRecord:
            params = record.Feature.FeatureParams
            if params is None:
                continue
            if hasattr(params, "UINameID"):  # ssXX
                used.add(params.UINameID)
            for attr in ("FeatUILabelNameID", "FeatUITooltipTextNameID", "SampleTextNameID"):  # cvXX
                if getattr(params, attr, 0):
                    used.add(getattr(params, attr))
            first = getattr(params, "FirstParamUILabelNameID", 0)
            if first:
                used.update(range(first, first + params.NumNamedParameters))
    font["name"].names = [n for n in font["name"].names if n.nameID in used]


def compile_family(family: Family, ds: DesignSpaceDocument) -> Path:
    vf = ufo2ft.compileVariableTTF(
        ds,
        flattenComponents=True,
        useProductionNames=True,
        optimizeGvar=True,
    )
    fix_stat(vf, family.italic)
    fix_unhinted(vf)
    drop_unused_names(vf)
    FONTS.mkdir(parents=True, exist_ok=True)
    vf.save(family.output)
    return family.output


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--upright", action="store_true", help="build only the upright family")
    parser.add_argument("--italic", action="store_true", help="build only the italic family")
    parser.add_argument("--check", action="store_true", help="lint the sources; do not compile")
    parser.add_argument("--fast", action="store_true",
                        help="skip the interpolation, clearance and pair-collision checks while iterating; never for a commit")
    args = parser.parse_args()
    chosen = [k for k in ("upright", "italic") if getattr(args, k)] or ["upright", "italic"]

    failed = False
    compiled: list[str] = []
    if not args.fast:
        problems = components.run(quiet=True)
        for line in problems:
            print(f"error   [clearance] {line}", file=sys.stderr)
        if problems:
            print(f"error   [clearance] {len(problems)} accent problem(s) (too close, a stacked tone"
                  " leaning wrong, a stack too tall); see scripts/components.py", file=sys.stderr)
            failed = True
    for key in chosen:
        family = FAMILIES[key]
        started = time.monotonic()
        ds = load(family)
        added = complete_lib(ds)
        errors, warnings = lint(ds, key)
        for line in warnings:
            print(f"warning [{key}] {line}", file=sys.stderr)
        for line in errors:
            print(f"error   [{key}] {line}", file=sys.stderr)
        if added:
            print(f"[{key}] {len(added)} glyph(s) beyond the stored glyph order: {' '.join(added[:30])}{' ...' if len(added) > 30 else ''}")
        if errors:
            failed = True
            continue
        if not args.fast:
            report = interpolation_report(family)
            if report:
                print(f"error   [{key}] masters do not interpolate cleanly:\n{report}", file=sys.stderr)
                failed = True
                continue
        if args.check:
            print(f"[{key}] sources OK ({len(default_font(ds))} glyphs, {len(ds.sources)} masters)")
            continue
        out = compile_family(family, ds)
        print(f"[{key}] {out.relative_to(ROOT)}  ({time.monotonic() - started:.1f}s)")
        compiled.append(key)
    if compiled and not args.fast:
        collisions = pairs.run(families=compiled, quiet=True)
        for line in collisions:
            print(f"error   [pairs] {line}", file=sys.stderr)
        if collisions:
            print(f"error   [pairs] {len(collisions)} colliding pair(s); `uv run scripts/kerning.py` kerns them",
                  file=sys.stderr)
            failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

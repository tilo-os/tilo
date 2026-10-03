# /// script
# requires-python = ">=3.11"
# dependencies = ["ufo2ft>=3.9", "ufoLib2", "fonttools[ufo,unicode]>=4.60", "glyphsLib>=6.15", "scipy",
#                 "skia-pathops", "shapely>=2", "uharfbuzz", "numpy"]
# ///
"""Kern the Vietnamese letters by group, so no two neighbours collide.

    uv run vendor/fonts/tilo-sans/scripts/kerning.py           # regenerate, rebuild, check
    uv run vendor/fonts/tilo-sans/scripts/kerning.py --reset   # remove what it generated

The kerning it writes is derived, and every run starts by removing what the
previous run wrote, so it is safe to run again after any change to outlines
or to the hand-made kerning:

1. The horned letters' right-side groups (KO_ohorn KO_uhorn KO_Ohorn
   KO_Uhorn) kern like the plain letter's group (KO_o ...): every pair of
   KO_o is copied to KO_ohorn. Their horn is kept clear by their advance
   (scripts/vietnamese.py), so the copies need no correction for flat
   neighbours.
2. The fonts are built and every checked pair (scripts/pairs.py) is shaped
   at the eight masters. A pair whose inks come closer than pairs.MIN_GAP
   collides.
3. A colliding pair is opened on the side of its accented letter, as a
   group: the letter's tone variants at the same mark height form a kerning
   group of their own: one level of marks above the letter (á à ả ã â ă,
   Á Ì ...) or a stack (ấ ầ ẩ ẫ ắ ...), split out of the letter's group (KO_a
   becomes KO_a + KO_a__top + KO_a__stack on that side). The new group
   inherits every pair of the group it left, so nothing else moves, and
   gets, against the neighbour's group, the smallest kern that opens each of
   its members to pairs.MIN_GAP + pairs.FIX_MARGIN. All tone variants at that
   height therefore space alike (no ầ wider than ấ before the same letter).
   Letters that had no group on that side get one named VN_<letter>__top.
   Values below pairs.FIX_MARGIN do not occur: a kern is only written to
   open a collision.
4. Where a hand-made exception (glyph or glyph-group pair) governs a
   colliding pair, the exception is raised instead, and its original value
   is kept in the font's lib so --reset restores it.
5. The fonts are rebuilt and checked again at the masters and between them.

Generated groups end in "__top" or "__stack"; nothing else should.
"""

from __future__ import annotations

import argparse
import math
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

import ufoLib2

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pairs  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
SOURCES = ROOT / "sources"
UPRIGHT = ["Regular", "Bold", "CondensedRegular", "CondensedBold"]
ITALIC = ["Italic", "BoldItalic", "CondensedItalic", "CondensedBoldItalic"]
K1, K2 = "public.kern1.", "public.kern2."
HORN_GROUPS = {"KO_ohorn": "KO_o", "KO_uhorn": "KO_u", "KO_Ohorn": "KO_O", "KO_Uhorn": "KO_U"}
# exceptions this script raised: {"a|b": [original]} ([] when there was none)
RAISED_KEY = "so.tilo.kerning.raised"
BUCKETS = ("__top", "__stack")
LOG = None  # --log FILE: why each value was written


def load():
    return {m: ufoLib2.Font.open(SOURCES / f"TiloSans-{m}.ufo") for m in UPRIGHT + ITALIC}


def generated(name: str) -> bool:
    return name.startswith(("public.kern1.", "public.kern2.")) and name.endswith(BUCKETS)


def reset(fonts) -> None:
    """Remove the generated groups and pairs, restore raised exceptions, and
    copy the horned letters' pairs from their plain letters."""
    for f in fonts.values():
        for key, original in f.lib.pop(RAISED_KEY, {}).items():
            a, b = key.split("|")
            if not original:
                f.kerning.pop((a, b), None)
            else:
                f.kerning[(a, b)] = original[0]
        for (a, b) in [k for k in f.kerning if generated(k[0]) or generated(k[1])]:
            del f.kerning[(a, b)]
        for name in [n for n in f.groups if generated(n)]:
            members = f.groups.pop(name)
            base = name[: -len(next(s for s in BUCKETS if name.endswith(s)))]
            if not base.split(".", 2)[-1].startswith("VN_"):
                f.groups[base] = list(f.groups.get(base, [])) + [g for g in members if g not in f.groups.get(base, [])]
        for horn, plain in HORN_GROUPS.items():
            hg, pg = K1 + horn, K1 + plain
            for key in [k for k in f.kerning if k[0] == hg]:
                del f.kerning[key]
            for (a, b), v in list(f.kerning.items()):
                if a == pg:
                    f.kerning[(hg, b)] = v


def save(fonts) -> None:
    for f in fonts.values():
        f.save()


def build() -> None:
    subprocess.run(["uv", "run", "-q", str(ROOT / "build.py"), "--fast"], check=True,
                   stdout=subprocess.DEVNULL)


TOP_MARKS = ("acutecomb", "gravecomb", "hookabovecomb", "tildecomb", "circumflexcomb", "brevecomb",
             "dieresiscomb", "macroncomb", "caroncomb", "ringcomb", "dotaccentcomb", "hungarumlautcomb")


def letter_and_level(font, name):
    """(base letter, mark height above it): 0 none, 1 one level of marks,
    2 a stacked pair (circumflex or breve with a tone)."""
    g = font[name]
    if not g.components or name.split(".")[0] in ("i", "j"):
        return name, 0  # i's and j's own dot does not count
    base = g.components[0].baseGlyph
    level = 0
    for c in g.components[1:]:
        mark = c.baseGlyph.split(".")[0]
        if mark.startswith(("circumflexcomb_", "brevecomb_")):
            level = max(level, 2)
        elif mark in TOP_MARKS:
            level = max(level, 1)
    # a precomposed base (ohorn in ohornacute) passes its own level on
    if base in font and font[base].components:
        b2, l2 = letter_and_level(font, base)
        if l2:
            return b2, max(level, l2)
    return base, level


def measure():
    """{family: {master: {(a, b): (kern_now, need)}}} for colliding pairs."""
    out = {}
    for family, (font_file, masters) in (("upright", pairs.UPRIGHT), ("italic", pairs.ITALIC)):
        res = pairs.check(font_file, masters, pairs.pair_set(font_file), pairs.MIN_GAP)
        per = defaultdict(dict)
        for gap, master, text, need, need_fix, ga, gb, kern in res:
            if need > 0.5:
                per[master][(ga, gb)] = (kern, math.ceil(need_fix))
        out[family] = per
    return out


def apply(fonts, collisions) -> tuple[int, int]:
    ref = fonts["Regular"]
    k1 = {g: n for n, ms in ref.groups.items() if n.startswith(K1) for g in ms}
    k2 = {g: n for n, ms in ref.groups.items() if n.startswith(K2) for g in ms}

    def bucket(side, g):
        letter, level = letter_and_level(ref, g)
        if level == 0:
            return None
        current = (k1 if side == 1 else k2).get(g)
        prefix = K1 if side == 1 else K2
        base = current if current else prefix + "VN_" + letter
        return base + BUCKETS[level - 1]

    # which side of each colliding pair moves: the letter with more marks
    # above it, the left one on a tie
    moves = {}
    for family in collisions.values():
        for per in family.values():
            for (a, b) in per:
                la, lb = letter_and_level(ref, a)[1], letter_and_level(ref, b)[1]
                if la == 0 and lb == 0:
                    moves[(a, b)] = None
                else:
                    moves[(a, b)] = 1 if la >= lb else 2
    # split the buckets that move
    split = {}
    for (a, b), side in moves.items():
        if side is None:
            continue
        g = a if side == 1 else b
        split[(side, g)] = bucket(side, g)
    new_groups = defaultdict(set)
    for (side, g), name in split.items():
        new_groups[name].add(g)
    # the whole bucket moves, not just the colliding letters: every tone
    # variant at that height on that side (Vietnamese letters and their
    # stylistic-set forms; ä å ā keep their kerning)
    by_cp = {u: g.name for g in ref for u in g.unicodes}
    vn = {by_cp[ord(c)] for c in pairs.VIETNAMESE + pairs.VIETNAMESE.upper() if ord(c) in by_cp}
    candidates = {g.name for g in ref if g.name.split(".")[0] in vn}
    for name in list(new_groups):
        side = 1 if name.startswith(K1) else 2
        for g in candidates:
            if g in ref and bucket(side, g) == name:
                new_groups[name].add(g)
    origin = {}
    for name, members in new_groups.items():
        base = name[: -len(next(s for s in BUCKETS if name.endswith(s)))]
        origin[name] = base if base in ref.groups else None
        for f in fonts.values():
            if origin[name]:
                f.groups[base] = [g for g in f.groups[base] if g not in members]
            f.groups[name] = sorted(members, key=lambda g: (letter_and_level(ref, g), g))
    k1 = {g: n for n, ms in ref.groups.items() if n.startswith(K1) for g in ms}
    k2 = {g: n for n, ms in ref.groups.items() if n.startswith(K2) for g in ms}
    # inherited pairs
    for f in fonts.values():
        orig = dict(f.kerning)
        for name, base in origin.items():
            if not base:
                continue
            first = name.startswith(K1)
            for (a, b), v in orig.items():
                if first and a == base:
                    f.kerning[(name, b)] = v
                elif not first and b == base:
                    f.kerning[(a, name)] = v
        # pairs between two split groups
        for (a, b), v in list(f.kerning.items()):
            for name, base in origin.items():
                if base and not name.startswith(K1) and b == base and generated(a):
                    f.kerning[(a, name)] = v

    def lookup(f, a, b):
        ga, gb = k1.get(a, a), k2.get(b, b)
        for key in ((a, b), (a, gb), (ga, b), (ga, gb)):
            if key in f.kerning:
                return key, f.kerning[key]
        return (ga, gb), 0

    # the values: per master, the smallest kern opening every member pair
    raised = 0
    written = 0
    for family, per_master in collisions.items():
        masters = UPRIGHT if family == "upright" else ITALIC
        targets = defaultdict(dict)  # key -> master -> value
        why = defaultdict(list)
        for master, per in per_master.items():
            f = fonts[master]
            for (a, b), (kern_now, need) in per.items():
                key, _ = lookup(f, a, b)
                want = kern_now + need
                targets[key][master] = max(targets[key].get(master, -10**9), want)
                why[key].append((master, a, b, kern_now, need))
        if LOG:
            with open(LOG, "a") as log:
                for key, items in sorted(why.items()):
                    log.write(f"{family} {key[0]} {key[1]}: {dict(targets[key])}\n")
                    for item in sorted(items, key=lambda i: -i[4])[:6]:
                        log.write(f"    {item}\n")
        for key, by_master in targets.items():
            for m in masters:
                f = fonts[m]
                now = f.kerning.get(key)
                value = max(by_master.get(m, now if now is not None else 0), now if now is not None else -10**9)
                if now is None or value != now:
                    raised += remember(f, key, now)
                    f.kerning[key] = value
                    written += 1
    return written, raised


def remember(f, key, original) -> int:
    """Note a pair this script changes that reset() would not remove (one
    without a generated group), so reset() can put it back."""
    if generated(key[0]) or generated(key[1]):
        return 0
    raised = f.lib.setdefault(RAISED_KEY, {})
    if "|".join(key) in raised:
        return 0
    raised["|".join(key)] = [] if original is None else [original]
    return 1


def corners(location):
    """The masters whose values a location interpolates."""
    wdth, wght = location
    widths = {75: ["Condensed"], 100: [""]}.get(wdth, ["Condensed", ""])
    return [(w, b) for w in widths for b in ("Regular", "Bold")]


def bump(fonts) -> int:
    """Open the pairs that still collide between the masters: their value
    grows by the shortfall in the masters around the location."""
    ref = fonts["Regular"]
    k1 = {g: n for n, ms in ref.groups.items() if n.startswith(K1) for g in ms}
    k2 = {g: n for n, ms in ref.groups.items() if n.startswith(K2) for g in ms}
    grow = defaultdict(int)  # (master, key) -> units
    for family, (font_file, masters) in (("upright", pairs.UPRIGHT), ("italic", pairs.ITALIC)):
        res = pairs.check(font_file, pairs.BETWEEN, pairs.pair_set(font_file), pairs.MIN_GAP)
        for gap, where, text, need, need_fix, a, b, kern in res:
            if need <= 0.5:
                continue
            f = ref
            ga, gb = k1.get(a, a), k2.get(b, b)
            key = next((k for k in ((a, b), (a, gb), (ga, b), (ga, gb)) if k in f.kerning), (ga, gb))
            for w, b_ in corners(pairs.BETWEEN[where]):
                name = w + b_
                if family == "italic":
                    name = {"Regular": "Italic", "Bold": "BoldItalic", "CondensedRegular": "CondensedItalic",
                            "CondensedBold": "CondensedBoldItalic"}[name]
                grow[(name, key)] = max(grow[(name, key)], math.ceil(need) + 2)
    for (m, key), units in grow.items():
        f = fonts[m]
        family = ITALIC if m in ITALIC else UPRIGHT
        for other in family:
            if key not in fonts[other].kerning:
                remember(fonts[other], key, None)
                fonts[other].kerning[key] = 0
        remember(f, key, f.kerning[key])
        f.kerning[key] += units
    return len(grow)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--reset", action="store_true", help="only remove the generated kerning")
    parser.add_argument("--log", help="write the colliding pairs behind each value to this file")
    args = parser.parse_args()
    global LOG
    LOG = args.log
    if LOG:
        Path(LOG).write_text("")
    fonts = load()
    reset(fonts)
    save(fonts)
    if args.reset:
        return 0
    build()
    collisions = measure()
    n = sum(len(p) for fam in collisions.values() for p in fam.values())
    print(f"{n} colliding pair(s) at the masters before kerning")
    written, raised = apply(fonts, collisions)
    save(fonts)
    groups = [g for g in fonts["Regular"].groups if generated(g)]
    print(f"{len(groups)} kerning group(s) split out, {written} value(s) written, {raised} exception(s) raised")
    build()
    for _ in range(3):
        grown = bump(fonts)
        if not grown:
            break
        save(fonts)
        print(f"{grown} value(s) raised for the instances between the masters")
        build()
    left = pairs.run(quiet=True)
    for line in left:
        print("COLLIDE " + line)
    print(f"{len(left)} colliding pair(s) left (masters and instances between them)")
    return 1 if left else 0


if __name__ == "__main__":
    sys.exit(main())

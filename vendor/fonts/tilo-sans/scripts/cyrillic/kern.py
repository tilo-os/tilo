"""Kerning groups and pairs for the Cyrillic glyphs.

Look-alikes join their Latin base glyph's groups; Cyrillic-specific sides
join the Latin group whose profile they share; the few sides without a Latin
counterpart get Cyrillic groups whose values are derived per master from a
Latin source (row or column), so the kerning follows Instrument's own.
"""

from __future__ import annotations

import math

from fontTools.misc import plistlib

K1, K2 = "public.kern1.", "public.kern2."

# glyph -> (kern1 group, kern2 group); None = no group.
CY_GROUPS = {
    "Be-cy": ("KO_B", None), "Ge-cy": ("KO_T", None), "Gje-cy": ("KO_T", None), "Geupturn-cy": ("KO_T", None),
    "Gestroke-cy": ("KO_T", None), "De-cy": ("KO_H", "KO_El-cy"), "El-cy": (None, "KO_El-cy"), "Lje-cy": ("KO_Softsign-cy", "KO_El-cy"),
    "Tse-cy": ("KO_H", "KO_H"), "Shcha-cy": ("KO_H", "KO_H"), "Yeru-cy": ("KO_H", None),
    "Zhe-cy": ("KO_K", "KO_X"), "Kadescender-cy": ("KO_Kadescender-cy", None), "Ze-cy": ("KO_B", "KO_three"),
    "Ereversed-cy": ("KO_O", "KO_three"), "Yu-cy": ("KO_O", None), "Ef-cy": ("KO_O", "KO_O"), "Schwa-cy": ("KO_O", "KO_O"),
    "Obarred-cy": ("KO_O", "KO_O"), "E-cy": ("KO_C", "KO_O"), "U-cy": ("KO_Y", "KO_Y"), "Ushort-cy": ("KO_Y", "KO_Y"),
    "Ustraightstroke-cy": ("KO_Y", "KO_Y"), "Hardsign-cy": ("KO_Softsign-cy", "KO_T"), "Softsign-cy": ("KO_Softsign-cy", None),
    "Nje-cy": ("KO_Softsign-cy", None), "Che-cy": (None, "KO_U"), "Ya-cy": (None, "KO_Ya-cy"), "Tshe-cy": (None, "KO_T"), "Dje-cy": (None, "KO_T"),
    "Io-cy": ("KO_E", None), "Iegrave-cy": ("KO_E", None), "Kje-cy": ("KO_K", None),
    # lowercase
    "be-cy": ("KO_be-cy", "KO_be-cy"), "ve-cy": ("KO_p", "KO_n-cy"), "ge-cy": ("KO_r", "KO_n-cy"), "gje-cy": ("KO_r", "KO_n-cy"),
    "geupturn-cy": ("KO_r", "KO_n-cy"), "gestroke-cy": ("KO_r", None), "de-cy": ("KO_de-cy", "KO_de-cy"),
    "io-cy": ("KO_ecaron", "KO_odieresis"), "iegrave-cy": ("KO_egrave", "KO_e"),
    "zhe-cy": ("KO_k", "KO_zhe-cy"), "ze-cy": ("KO_p", "KO_s"), "ii-cy": ("KO_u", "KO_n-cy"), "iishort-cy": ("KO_u", "KO_n-cy"),
    "iigrave-cy": ("KO_u", "KO_n-cy"), "ka-cy": ("KO_k", "KO_n-cy"), "kje-cy": ("KO_k", "KO_n-cy"),
    "kadescender-cy": ("KO_kadescender-cy", "KO_n-cy"), "el-cy": ("KO_u", "KO_el-cy"), "lje-cy": ("KO_p", "KO_el-cy"),
    "em-cy": ("KO_u", "KO_n-cy"), "en-cy": ("KO_u", "KO_n-cy"), "endescender-cy": ("KO_u", "KO_n-cy"),
    "nje-cy": ("KO_p", "KO_n-cy"), "pe-cy": ("KO_u", "KO_n-cy"), "er-cy": ("KO_p", "KO_n-cy"),
    "te-cy": ("KO_te-cy", "KO_te-cy"), "ef-cy": ("KO_ef-cy", "KO_ef-cy"), "tse-cy": ("KO_u", "KO_n-cy"),
    "che-cy": ("KO_u", "KO_n-cy"), "sha-cy": ("KO_u", "KO_n-cy"), "shcha-cy": ("KO_u", "KO_n-cy"),
    "dzhe-cy": ("KO_u", "KO_n-cy"), "hardsign-cy": ("KO_p", "KO_t"), "yeru-cy": ("KO_u", "KO_n-cy"),
    "softsign-cy": ("KO_p", "KO_n-cy"), "ereversed-cy": ("KO_o", "KO_s"), "yu-cy": ("KO_o", "KO_n-cy"),
    "ya-cy": ("KO_u", "KO_ya-cy"), "e-cy": ("KO_c", "KO_e"), "dje-cy": ("KO_j", None), "tshe-cy": ("KO_n", None),
    "schwa-cy": ("KO_o", "KO_e"), "obarred-cy": ("KO_o", "KO_e"), "ustraight-cy": ("KO_y", "KO_v"),
    "ustraightstroke-cy": ("KO_y", "KO_v"), "ushort-cy": ("KO_y", "KO_v"), "yi-cy": (None, "KO_idieresis"),
    # language forms
    "de-cy.loclBGR": ("KO_u", "KO_g"), "ii-cy.loclBGR": ("KO_u", "KO_u"), "iishort-cy.loclBGR": ("KO_u", "KO_u"),
    "iigrave-cy.loclBGR": ("KO_u", "KO_u"), "ka-cy.loclBGR": ("KO_k", None), "pe-cy.loclBGR": ("KO_n", "KO_n-cy"),
    "te-cy.loclBGR": ("KO_n", "KO_n-cy"), "tse-cy.loclBGR": ("KO_u", "KO_u"), "sha-cy.loclBGR": ("KO_u", "KO_u"),
    "shcha-cy.loclBGR": ("KO_u", "KO_u"), "zhe-cy.loclBGR": ("KO_k", "KO_zhe-cy"), "el-cy.loclBGR": (None, "KO_el-cy"),
    "yu-cy.loclBGR": ("KO_o", None),
    "be-cy.loclSRB": ("KO_be-cy", "KO_be-cy"), "ge-cy.loclSRB": ("KO_r", "KO_n-cy"), "gje-cy.loclSRB": ("KO_r", "KO_n-cy"), "de-cy.loclSRB": ("KO_de-cy", "KO_de-cy"),
    "pe-cy.loclSRB": ("KO_u", "KO_n-cy"), "te-cy.loclSRB": ("KO_te-cy", "KO_te-cy"),
}

# Cyrillic groups and the Latin source of their values, per style:
# (Latin group or glyph, factor).
# Ь's right side: its bowl sits low and leaves the top open, so letters
# with a top bar or open arms (Т У Ч V Y, quotes) tuck in over it nearly as
# far as over L's arm; everything else sees B's lower bowl.
# (A trailing * matches every group with that prefix: Instrument splits the
# quotes over several groups, and ’ and ” must kern alike.)
OPEN_TOP = ["KO_T", "KO_Y", "KO_V", "KO_W", "KO_U", "KO_quote*", "KO_asterisk", "KO_question"]
# Я's left side: a round bowl at the top over a leg that runs out at the
# foot, so only letters overhanging at the top tuck in, about as far as
# over O; elsewhere the leg keeps it at 0.
OVER_TOP = ["KO_T", "KO_Y", "KO_V", "KO_W", "KO_quote*"]
NEW_KERN1 = {
    "KO_te-cy": {"upright": ("KO_r", 1.0), "italic": ("KO_n", 1.0)},
    "KO_de-cy": {"upright": ("KO_u", 1.0), "italic": ("KO_o", 1.0)},
    # б ф: o's right side, held off where the flag or the stem rises (FLOOR1)
    "KO_be-cy": {"upright": ("KO_o", 1.0), "italic": ("KO_o", 1.0)},
    "KO_ef-cy": {"upright": ("KO_o", 1.0), "italic": ("KO_o", 1.0)},
    # Қ қ: K's and k's right side, held off where the tail runs out (FLOOR1)
    "KO_Kadescender-cy": {"upright": ("KO_K", 1.0), "italic": ("KO_K", 1.0)},
    "KO_kadescender-cy": {"upright": ("KO_k", 1.0), "italic": ("KO_k", 1.0)},
    "KO_Softsign-cy": {"upright": [("KO_L", 0.55, OPEN_TOP), ("KO_B", 1.0, None)],
                       "italic": [("KO_L", 0.55, OPEN_TOP), ("KO_B", 1.0, None)]},
}
NEW_KERN2 = {
    "KO_n-cy": {"upright": ("n", 1.0), "italic": ("n", 1.0)},
    # Л Д: the stem stands about a stem's width in from the foot; after A
    # the foot tucks under A's diagonal like a little of O's round
    "KO_El-cy": {"upright": [("KO_O", 0.4, ["KO_A"]), ("KO_A", 0.25, None)],
                 "italic": [("KO_O", 0.4, ["KO_A"]), ("KO_A", 0.25, None)]},
    "KO_Ya-cy": {"upright": [("KO_O", 0.73, OVER_TOP)], "italic": [("KO_O", 0.73, OVER_TOP)]},
    "KO_el-cy": {"upright": ("n", 1.0), "italic": ("n", 1.0)},
    "KO_de-cy": {"upright": ("n", 1.0), "italic": ("KO_e", 1.0)},
    "KO_te-cy": {"upright": ("KO_t", 1.0), "italic": ("n", 1.0)},
    # б ф я: e's left side, held off where an ascender or a foot reaches
    # out (FLOOR2)
    "KO_be-cy": {"upright": ("KO_e", 1.0), "italic": ("KO_e", 1.0)},
    "KO_ef-cy": {"upright": ("KO_e", 1.0), "italic": ("KO_e", 1.0)},
    "KO_ya-cy": {"upright": ("KO_e", 1.0), "italic": ("KO_e", 1.0)},
    # ж: x's left side, held off where its lower arm reaches further out
    "KO_zhe-cy": {"upright": ("KO_x", 1.0), "italic": ("KO_x", 1.0)},
}

# Sides whose Latin source tucks a neighbour in further than their ink
# allows: a round bowl with an ascender (б, ф, the italic д drawn as a
# mirrored 6) or a bowl over a foot that runs out (я Я). Their kerning takes
# the Latin source's value, but in the band of heights where that part
# sits, no less than keeps, against every Cyrillic letter and every
# punctuation mark or figure on the other side, `share` of the white that
# neighbour keeps in the same band from a Latin reference glyph at
# Instrument's own kerning (`ref`: h, a straight ascender on the left; l on
# the right; x and X, a diagonal foot); and nowhere less than FLOOR_MIN
# units. So under Т's bar б's flag keeps about half the white h's ascender
# keeps, while its bowl still tucks in like o; К keeps as much white from
# я's foot as from x's. Measured on the outlines construct.py writes
# (ink.py), in every master.
#   group: (reference glyph, share, band bottom, band top)
FLOOR2 = {"KO_be-cy": ("h", 0.55, 500, 800), "KO_de-cy": ("h", 0.55, 500, 800),
          "KO_ef-cy": ("h", 0.55, 520, 800),
          "KO_ya-cy": ("x", 0.8, -20, 200), "KO_Ya-cy": ("X", 0.8, -20, 280),
          "KO_zhe-cy": ("x", 0.8, -20, 530)}
FLOOR1 = {"KO_be-cy": ("l", 0.55, 500, 800), "KO_de-cy": ("l", 0.55, 500, 800),
          "KO_ef-cy": ("l", 0.55, 520, 800),
          "KO_Kadescender-cy": ("K", 1.0, -220, 140), "KO_kadescender-cy": ("k", 1.0, -220, 140)}
FLOOR_MIN = 28  # pairs.MIN_GAP + pairs.FIX_MARGIN, and a little for the instances between


def _key(side, name):
    if name.startswith("KO_"):
        return (K1 if side == 1 else K2) + name
    return name


def apply_floors(kerning, groups, ink):
    """Raise the FLOOR1/FLOOR2 groups' pairs to what their ink allows (see
    FLOOR2); returns how many pairs were raised."""
    member1, member2 = {}, {}
    for g, members in groups.items():
        for n in members:
            (member1 if g.startswith(K1) else member2)[n] = g

    def sides(prefix, member):
        """Every key on one side (groups, and glyphs in none) with its
        members that meet Cyrillic text."""
        out = {}
        for g, members in groups.items():
            if g.startswith(prefix):
                keep = [n for n in members if ink.kind(n)]
                if keep:
                    out[g] = keep
        for n in list(ink.c.font.keys()) + list(ink.built):
            if n not in member and ink.kind(n):
                out[n] = [n]
        return out

    def value(a, b):
        """The kerning HarfBuzz applies between glyphs a and b."""
        for x in (a, member1.get(a)):
            for y in (b, member2.get(b)):
                if x and y and y in kerning.get(x, {}):
                    return kerning[x][y]
        return 0

    firsts, seconds = sides(K1, member1), sides(K2, member2)
    # white wider than between two n's needs no holding off: the floors bind
    # only where the reference glyph comes close
    l = ink.c.lc
    open_white = l["lsb"] + (l["W"] - l["Rx1"])
    raised = 0
    for floors, side in ((FLOOR2, 2), (FLOOR1, 1)):
        for gname, (ref, share, ylo, yhi) in floors.items():
            key = (K2 if side == 2 else K1) + gname
            mine = groups.get(key, [])
            for okey, members in (firsts if side == 2 else seconds).items():
                need = -math.inf
                for m in members:
                    pair = (lambda g: (m, g)) if side == 2 else (lambda g: (g, m))
                    ref_gap = ink.gap(*pair(ref), value(*pair(ref)), ylo, yhi)
                    for g in mine:
                        base = ink.gap(*pair(g))
                        if math.isfinite(base):
                            need = max(need, FLOOR_MIN - base)
                        band = ink.gap(*pair(g), 0, ylo, yhi)
                        if math.isfinite(ref_gap) and math.isfinite(band):
                            need = max(need, min(share * ref_gap, open_white) - band)
                first, second = (okey, key) if side == 2 else (key, okey)
                if need > value(first, second) + 0.5:
                    kerning.setdefault(first, {})[second] = int(math.ceil(need))
                    raised += 1
    return raised


def update_kerning(ufo, italic, cy_names, lookalikes, extra=None, ink=None):
    groups = plistlib.loads((ufo / "groups.plist").read_bytes())
    kerning = plistlib.loads((ufo / "kerning.plist").read_bytes())
    style = "italic" if italic else "upright"
    mine = set(cy_names)
    new1 = {K1 + g for g in NEW_KERN1}
    new2 = {K2 + g for g in NEW_KERN2}

    # start clean: drop earlier Cyrillic additions
    for g in list(groups):
        if g in new1 or g in new2:
            del groups[g]
        else:
            groups[g] = [n for n in groups[g] if n not in mine]
    for k in list(kerning):
        if k in new1 or k in mine:
            del kerning[k]
            continue
        kerning[k] = {s: v for s, v in kerning[k].items() if s not in new2 and s not in mine}

    member1, member2 = {}, {}
    for g, members in groups.items():
        for n in members:
            (member1 if g.startswith(K1) else member2)[n] = g
    assign = {}
    for name, base in lookalikes.items():
        assign[name] = (member1.get(base), member2.get(base))
    for name, (k1, k2) in CY_GROUPS.items():
        assign[name] = (_key(1, k1) if k1 else None, _key(2, k2) if k2 else None)
    for name, (k1, k2) in sorted(assign.items(), reverse=True):
        for k in (k1, k2):
            if k is None:
                continue
            groups.setdefault(k, [])
            if name not in groups[k]:
                groups[k].insert(0, name)

    def value(first, second):
        return kerning.get(first, {}).get(second, 0)

    def matches(group, patterns):
        name = group[len(K1):]  # K1 and K2 have the same length
        return any(name.startswith(p[:-1]) if p.endswith("*") else name == p for p in patterns)

    def src(k, table, prefix, side, other=None):
        """The Latin source of a Cyrillic group's values; rules given as a
        list [(source, factor, groups on the other side or None)] apply the
        first entry whose groups match (None matches all), else nothing."""
        if k.startswith(prefix) and k[len(prefix):] in table:
            rule = table[k[len(prefix):]][style]
            if isinstance(rule, list):
                for s, f, only in rule:
                    if only is None or (other is not None and matches(other, only)):
                        return _key(side, s), f
                return None, 0.0
            s, f = rule
            return _key(side, s), f
        return k, 1.0

    seconds = set()
    for row in kerning.values():
        seconds.update(row)
    seconds = sorted(s for s in seconds if s.startswith(K2)) + sorted(new2)
    firsts = sorted(k for k in kerning) + sorted(new1)
    for first in firsts:
        for second in seconds:
            if first not in new1 and second not in new2:
                continue
            f_src, f_fac = src(first, NEW_KERN1, K1, 1, second)
            s_src, s_fac = src(second, NEW_KERN2, K2, 2, first)
            if f_src is None or s_src is None:
                continue
            v = value(f_src, s_src) * f_fac * s_fac
            if abs(v) >= 5:
                kerning.setdefault(first, {})[second] = int(round(v))
    if ink is not None:
        apply_floors(kerning, groups, ink)
    for (first, second), v in (extra or {}).items():
        first = _key(1, first) if first.startswith("KO_") else first
        second = _key(2, second) if second.startswith("KO_") else second
        if v:
            kerning.setdefault(first, {})[second] = int(round(v))
        else:
            kerning.get(first, {}).pop(second, None)
    for k in list(kerning):
        if not kerning[k]:
            del kerning[k]
    for g in list(groups):
        if not groups[g]:
            del groups[g]
    (ufo / "groups.plist").write_bytes(plistlib.dumps(groups, sort_keys=True))
    (ufo / "kerning.plist").write_bytes(plistlib.dumps(kerning, sort_keys=True))

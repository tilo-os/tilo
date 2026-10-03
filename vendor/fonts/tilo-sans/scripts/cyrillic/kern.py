"""Kerning groups and pairs for the Cyrillic glyphs.

Look-alikes join their Latin base glyph's groups; Cyrillic-specific sides
join the Latin group whose profile they share; the few sides without a Latin
counterpart get Cyrillic groups whose values are derived per master from a
Latin source (row or column), so the kerning follows Instrument's own.
"""

from __future__ import annotations

from fontTools.misc import plistlib

K1, K2 = "public.kern1.", "public.kern2."

# glyph -> (kern1 group, kern2 group); None = no group.
CY_GROUPS = {
    "Be-cy": ("KO_B", None), "Ge-cy": ("KO_T", None), "Gje-cy": ("KO_T", None), "Geupturn-cy": ("KO_T", None),
    "Gestroke-cy": ("KO_T", None), "De-cy": (None, "KO_El-cy"), "El-cy": (None, "KO_El-cy"), "Lje-cy": ("KO_B", "KO_El-cy"),
    "Zhe-cy": ("KO_K", "KO_X"), "Kadescender-cy": ("KO_K", None), "Ze-cy": ("KO_B", "KO_three"),
    "Ereversed-cy": ("KO_O", "KO_three"), "Yu-cy": ("KO_O", None), "Ef-cy": ("KO_O", "KO_O"), "Schwa-cy": ("KO_O", "KO_O"),
    "Obarred-cy": ("KO_O", "KO_O"), "E-cy": ("KO_C", "KO_O"), "U-cy": ("KO_Y", "KO_Y"), "Ushort-cy": ("KO_Y", "KO_Y"),
    "Ustraightstroke-cy": ("KO_Y", "KO_Y"), "Hardsign-cy": ("KO_B", "KO_T"), "Softsign-cy": ("KO_B", None),
    "Nje-cy": ("KO_B", None), "Che-cy": (None, "KO_U"), "Tshe-cy": (None, "KO_T"), "Dje-cy": (None, "KO_T"),
    "Io-cy": ("KO_E", None), "Iegrave-cy": ("KO_E", None), "Kje-cy": ("KO_K", None),
    # lowercase
    "be-cy": ("KO_p", "KO_e"), "ve-cy": ("KO_p", "KO_n-cy"), "ge-cy": ("KO_r", "KO_n-cy"), "gje-cy": ("KO_r", "KO_n-cy"),
    "geupturn-cy": ("KO_r", "KO_n-cy"), "gestroke-cy": ("KO_r", None), "de-cy": ("KO_de-cy", "KO_de-cy"),
    "io-cy": ("KO_ecaron", "KO_odieresis"), "iegrave-cy": ("KO_egrave", "KO_e"),
    "zhe-cy": ("KO_k", "KO_x"), "ze-cy": ("KO_p", "KO_s"), "ii-cy": ("KO_u", "KO_n-cy"), "iishort-cy": ("KO_u", "KO_n-cy"),
    "iigrave-cy": ("KO_u", "KO_n-cy"), "ka-cy": ("KO_k", "KO_n-cy"), "kje-cy": ("KO_k", "KO_n-cy"),
    "kadescender-cy": ("KO_k", "KO_n-cy"), "el-cy": ("KO_u", "KO_el-cy"), "lje-cy": ("KO_p", "KO_el-cy"),
    "em-cy": ("KO_u", "KO_n-cy"), "en-cy": ("KO_u", "KO_n-cy"), "endescender-cy": ("KO_u", "KO_n-cy"),
    "nje-cy": ("KO_p", "KO_n-cy"), "pe-cy": ("KO_u", "KO_n-cy"), "er-cy": ("KO_p", "KO_n-cy"),
    "te-cy": ("KO_te-cy", "KO_te-cy"), "ef-cy": ("KO_o", "KO_e"), "tse-cy": ("KO_u", "KO_n-cy"),
    "che-cy": ("KO_u", "KO_u"), "sha-cy": ("KO_u", "KO_n-cy"), "shcha-cy": ("KO_u", "KO_n-cy"),
    "dzhe-cy": ("KO_u", "KO_n-cy"), "hardsign-cy": ("KO_p", "KO_t"), "yeru-cy": ("KO_u", "KO_n-cy"),
    "softsign-cy": ("KO_p", "KO_n-cy"), "ereversed-cy": ("KO_o", "KO_s"), "yu-cy": ("KO_o", "KO_n-cy"),
    "ya-cy": ("KO_u", "KO_e"), "e-cy": ("KO_c", "KO_e"), "dje-cy": ("KO_j", None), "tshe-cy": ("KO_n", None),
    "schwa-cy": ("KO_o", "KO_e"), "obarred-cy": ("KO_o", "KO_e"), "ustraight-cy": ("KO_y", "KO_v"),
    "ustraightstroke-cy": ("KO_y", "KO_v"), "ushort-cy": ("KO_y", "KO_v"), "yi-cy": (None, "KO_idieresis"),
    # language forms
    "de-cy.loclBGR": ("KO_u", "KO_g"), "ii-cy.loclBGR": ("KO_u", "KO_u"), "iishort-cy.loclBGR": ("KO_u", "KO_u"),
    "iigrave-cy.loclBGR": ("KO_u", "KO_u"), "ka-cy.loclBGR": ("KO_k", None), "pe-cy.loclBGR": ("KO_n", "KO_n-cy"),
    "te-cy.loclBGR": ("KO_n", "KO_n-cy"), "tse-cy.loclBGR": ("KO_u", "KO_u"), "sha-cy.loclBGR": ("KO_u", "KO_u"),
    "shcha-cy.loclBGR": ("KO_u", "KO_u"), "zhe-cy.loclBGR": ("KO_k", None), "el-cy.loclBGR": (None, "KO_el-cy"),
    "yu-cy.loclBGR": ("KO_o", None), "ze-cy.loclBGR": (None, "KO_s"),
    "be-cy.loclSRB": ("KO_p", "KO_e"), "ge-cy.loclSRB": ("KO_r", "KO_n-cy"), "de-cy.loclSRB": ("KO_de-cy", "KO_de-cy"),
    "pe-cy.loclSRB": ("KO_u", "KO_n-cy"), "te-cy.loclSRB": ("KO_te-cy", "KO_te-cy"),
}

# Cyrillic groups and the Latin source of their values, per style:
# (Latin group or glyph, factor).
NEW_KERN1 = {
    "KO_te-cy": {"upright": ("KO_r", 1.0), "italic": ("KO_n", 1.0)},
    "KO_de-cy": {"upright": ("KO_u", 1.0), "italic": ("KO_o", 1.0)},
}
NEW_KERN2 = {
    "KO_n-cy": {"upright": ("n", 1.0), "italic": ("n", 1.0)},
    "KO_El-cy": {"upright": ("KO_A", 0.45), "italic": ("KO_A", 0.45)},
    "KO_el-cy": {"upright": ("KO_A", 0.45), "italic": ("KO_A", 0.45)},
    "KO_de-cy": {"upright": ("KO_A", 0.45), "italic": ("KO_e", 1.0)},
    "KO_te-cy": {"upright": ("KO_t", 1.0), "italic": ("n", 1.0)},
}


def _key(side, name):
    if name.startswith("KO_"):
        return (K1 if side == 1 else K2) + name
    return name


def update_kerning(ufo, italic, cy_names, lookalikes):
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

    def src(k, table, prefix, side):
        if k.startswith(prefix) and k[len(prefix):] in table:
            s, f = table[k[len(prefix):]][style]
            return _key(side, s), f
        return k, 1.0

    seconds = set()
    for row in kerning.values():
        seconds.update(row)
    seconds = sorted(s for s in seconds if s.startswith(K2)) + sorted(new2)
    firsts = sorted(k for k in kerning) + sorted(new1)
    for first in firsts:
        f_src, f_fac = src(first, NEW_KERN1, K1, 1)
        for second in seconds:
            if first not in new1 and second not in new2:
                continue
            s_src, s_fac = src(second, NEW_KERN2, K2, 2)
            v = value(f_src, s_src) * f_fac * s_fac
            if abs(v) >= 5:
                kerning.setdefault(first, {})[second] = int(round(v))
    for k in list(kerning):
        if not kerning[k]:
            del kerning[k]
    for g in list(groups):
        if not groups[g]:
            del groups[g]
    (ufo / "groups.plist").write_bytes(plistlib.dumps(groups, sort_keys=True))
    (ufo / "kerning.plist").write_bytes(plistlib.dumps(kerning, sort_keys=True))

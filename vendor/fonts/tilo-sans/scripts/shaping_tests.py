# /// script
# requires-python = ">=3.11"
# dependencies = ["fonttools>=4.60", "uharfbuzz>=0.45"]
# ///
"""HarfBuzz shaping tests for the built fonts (fonts/default/ and fonts/full/).

    uv run vendor/fonts/tilo-sans/scripts/shaping_tests.py

Each check runs at the four corner masters and Medium/SemiBold, upright and
italic:

  vietnamese   every Vietnamese letter draws the same whether typed
               precomposed (NFC) or decomposed (NFD): same glyph ink, same
               advance; the stacked tones ligate; ư/ơ from u/o + U+031B
  marks        the Vietnamese tones and dot below typed as combining marks
               after each Vietnamese vowel, and the Greek tonos and
               dialytika after each Greek vowel: above marks clear the
               letter's top, the dot below its bottom, and stay over its ink
  greek        tonos and dialytika letters NFC = NFD; the ano teleia form of
               U+00B7 after a Greek letter, the centred dot after a space
  il           ss13 turns I and l (and their accented forms) into the
               distinct forms, cv01 only I, cv02 only l, zero the slashed
               zero; in fonts/default/ these features do nothing
  cyrillic     locl: Bulgarian (bg) and Serbian/Macedonian (sr, mk) forms
               in fonts/full/; fonts/default/ has no Cyrillic at all
  default      the default font shapes every Latin, Vietnamese and Greek
               test string exactly like the full font

Exit status 1 when any check fails.
"""

from __future__ import annotations

import sys
import unicodedata
from pathlib import Path

import uharfbuzz as hb
from fontTools.pens.boundsPen import BoundsPen
from fontTools.ttLib import TTFont

ROOT = Path(__file__).resolve().parent.parent
FONTS = ROOT / "fonts"
FILES = {"upright": "TiloSans[wdth,wght].ttf", "italic": "TiloSans-Italic[wdth,wght].ttf"}
LOCATIONS = [(100, 400), (100, 500), (100, 600), (100, 700), (75, 400), (75, 700)]

VIETNAMESE = "aàáảãạăằắẳẵặâầấẩẫậbcdđeèéẻẽẹêềếểễệghiìíỉĩịklmnoòóỏõọôồốổỗộơờớởỡợpqrstuùúủũụưừứửữựvxyỳýỷỹỵ"
GREEK_LETTERS = "ΑΒΓΔΕΖΗΘΙΚΛΜΝΞΟΠΡΣΤΥΦΧΨΩαβγδεζηθικλμνξοπρσςτυφχψω"
GREEK_ACCENTED = "ΆΈΉΊΌΎΏΪΫάέήίόύώϊϋΐΰ"
# letters with their marks, typed as separate combining characters
VIETNAMESE_BASES = "aăâeêioôơuưyAĂÂEÊIOÔƠUƯY"
VIETNAMESE_MARKS = "\u0300\u0301\u0303\u0309\u0323"
GREEK_BASES = "αεηιουωΑΕΗΙΟΥΩ"
GREEK_MARKS = "\u0301\u0308"
CYRILLIC = "абвгдежзийклмнопрстуфхцчшщъыьэюяАБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯ"

failures: list[str] = []


def fail(msg: str) -> None:
    failures.append(msg)


class Shaper:
    def __init__(self, path: Path):
        self.path = path
        self.tt = TTFont(path)
        self.order = self.tt.getGlyphOrder()
        self.cmap = self.tt.getBestCmap()
        self.face = hb.Face(hb.Blob.from_file_path(str(path)))
        self.fonts = {}

    def font(self, loc):
        if loc not in self.fonts:
            f = hb.Font(self.face)
            f.set_variations({"wdth": loc[0], "wght": loc[1]})
            self.fonts[loc] = (f, self.tt.getGlyphSet(location={"wdth": loc[0], "wght": loc[1]}))
        return self.fonts[loc]

    def shape(self, text, loc, features=None, lang=None):
        f, _ = self.font(loc)
        buf = hb.Buffer()
        buf.add_str(text)
        buf.guess_segment_properties()
        if lang:
            buf.language = lang
        hb.shape(f, buf, features or {})
        return [(self.order[i.codepoint], p.x_advance, p.x_offset, p.y_offset)
                for i, p in zip(buf.glyph_infos, buf.glyph_positions)]

    def boxes(self, text, loc, features=None, lang=None):
        """Ink box of each shaped glyph, positioned; and the total advance."""
        _, gs = self.font(loc)
        x = 0
        out = []
        for name, adv, dx, dy in self.shape(text, loc, features, lang):
            pen = BoundsPen(gs)
            gs[name].draw(pen)
            if pen.bounds:
                x0, y0, x1, y1 = pen.bounds
                out.append((name, (x + dx + x0, dy + y0, x + dx + x1, dy + y1)))
            else:
                out.append((name, None))
            x += adv
        return out, x


def union(boxes):
    bs = [b for _, b in boxes if b]
    if not bs:
        return None
    return (min(b[0] for b in bs), min(b[1] for b in bs), max(b[2] for b in bs), max(b[3] for b in bs))


def close(a, b, tol=2):
    return a is not None and b is not None and all(abs(p - q) <= tol for p, q in zip(a, b))


def check_nfc_nfd(sh, chars, label, key):
    for loc in LOCATIONS:
        for ch in chars:
            nfd = unicodedata.normalize("NFD", ch)
            if nfd == ch:
                continue
            a, adv_a = sh.boxes(ch, loc)
            b, adv_b = sh.boxes(nfd, loc)
            if not close(union(a), union(b)) or abs(adv_a - adv_b) > 1:
                fail(f"{label} {key} {loc}: {ch} NFC {union(a)} adv {adv_a} != NFD {union(b)} adv {adv_b} "
                     f"({[n for n, _ in a]} vs {[n for n, _ in b]})")


def check_marks(sh, bases, marks, key):
    names = {sh.cmap.get(ord(m)) for m in marks}
    for loc in [(100, 400), (100, 700), (75, 700)]:
        for base in bases:
            if ord(base) not in sh.cmap:
                continue
            plain, _ = sh.boxes(base, loc)
            pb = union(plain)
            for m in marks:
                if ord(m) not in sh.cmap:
                    continue
                boxes, _ = sh.boxes(base + m, loc)
                if len(boxes) == 1:
                    continue  # composed into a precomposed glyph
                mark = [b for n, b in boxes if n in names or n.endswith(("comb", ".case", ".narrow")) or "comb" in n]
                if not mark or mark[-1] is None:
                    continue
                mb = mark[-1]
                cx = (mb[0] + mb[2]) / 2
                if not (pb[0] - 60 <= cx <= pb[2] + 60):
                    fail(f"marks {key} {loc}: {base}+U+{ord(m):04X} mark centre {cx:.0f} off the letter {pb[0]:.0f}..{pb[2]:.0f}")
                if m != "\u0323" and mb[1] < pb[3] - 40:
                    fail(f"marks {key} {loc}: {base}+U+{ord(m):04X} top mark bottom {mb[1]:.0f} below letter top {pb[3]:.0f}")
                if m == "̣" and mb[3] > pb[1] + 10:
                    fail(f"marks {key} {loc}: {base}+U+{ord(m):04X} dot below top {mb[3]:.0f} above letter bottom {pb[1]:.0f}")


def glyphs(sh, text, features=None, lang=None, loc=(100, 400)):
    return [n for n, *_ in sh.shape(text, loc, features, lang)]


def main() -> int:
    default = {k: Shaper(FONTS / "default" / v) for k, v in FILES.items()}
    full = {k: Shaper(FONTS / "full" / v) for k, v in FILES.items()}
    counts = {}

    for key in FILES:
        for which, sh in (("default", default[key]), ("full", full[key])):
            n0 = len(failures)
            missing = [c for c in VIETNAMESE + VIETNAMESE.upper() + GREEK_LETTERS + GREEK_ACCENTED + "₫·;"
                       if ord(c) not in sh.cmap]
            if missing:
                fail(f"coverage {which} {key}: missing {''.join(missing)}")
            check_nfc_nfd(sh, VIETNAMESE + VIETNAMESE.upper(), f"vietnamese {which}", key)
            check_nfc_nfd(sh, GREEK_ACCENTED, f"greek {which}", key)
            for word in ("ấ", "Ấ", "ặ", "Ặ", "ễ", "Ễ", "ở", "Ữ"):
                g = glyphs(sh, unicodedata.normalize("NFD", word))
                if len(g) > 1 and not any("_" in n for n in g):
                    fail(f"vietnamese {which} {key}: {word} decomposed gives {g}, no stacked mark")
            for src, want in (("ơ", "ohorn"), ("Ư", "Uhorn")):
                if glyphs(sh, src)[0] != want:
                    fail(f"vietnamese {which} {key}: {src!r} -> {glyphs(sh, src)}")
            check_marks(sh, VIETNAMESE_BASES, VIETNAMESE_MARKS, f"{which} {key}")
            check_marks(sh, GREEK_BASES, GREEK_MARKS, f"{which} {key}")
            # ano teleia: after a Greek letter, not after a space
            if glyphs(sh, "ν·")[1] not in ("anoteleia", "uni0387"):
                fail(f"greek {which} {key}: ν· -> {glyphs(sh, 'ν·')}")
            if glyphs(sh, "α · β")[2] in ("anoteleia", "uni0387"):
                fail(f"greek {which} {key}: spaced middle dot became the ano teleia")
            counts[(which, key)] = len(failures) - n0

        d, f = default[key], full[key]
        # I/l and zero
        on = glyphs(f, "Il0Í", {"ss13": True, "zero": True})
        if on[:3] != ["I.ss13", "l.ss13", "zero.zero"] or not on[3].endswith(".ss13"):
            fail(f"il full {key}: Il0Í with ss13+zero -> {on}")
        if glyphs(f, "Il", {"cv01": True}) != ["I.ss13", "l"]:
            fail(f"il full {key}: cv01 -> {glyphs(f, 'Il', {'cv01': True})}")
        if glyphs(f, "Il", {"cv02": True}) != ["I", "l.ss13"]:
            fail(f"il full {key}: cv02 -> {glyphs(f, 'Il', {'cv02': True})}")
        for ch in "ĨỈỊ":
            if not glyphs(f, ch, {"ss13": True})[0].endswith(".ss13"):
                fail(f"il full {key}: {ch} has no ss13 form")
        if glyphs(d, "Il0", {"ss13": True, "zero": True, "cv01": True}) != glyphs(d, "Il0"):
            fail(f"il default {key}: ss13/zero/cv01 still change glyphs in the default font")
        # Cyrillic locl
        if any(ord(c) in d.cmap for c in CYRILLIC):
            fail(f"cyrillic default {key}: the default font maps Cyrillic")
        for lang, text, want in (("bg", "д", "loclBGR"), ("bg", "ж", "loclBGR"), ("sr", "б", "loclSRB"),
                                 ("mk", "г", "loclSRB"), ("sr", "п", "loclSRB")):
            g = glyphs(f, text, lang=lang)
            if not g[0].endswith(want):
                fail(f"cyrillic full {key}: {text} in {lang} -> {g}")
        if glyphs(f, "д")[0].endswith(("loclBGR", "loclSRB")):
            fail(f"cyrillic full {key}: д without a language takes a local form")
        # default == full for everything the default ships
        texts = ["Tiếng Việt có dấu: Kỹ thuật, nước Mỹ, Tự, Từ, Tố, Ẫ Ẵ Ễ Ỗ, đồng ₫",
                 "Ξεσκεπάζω την ψυχοφθόρα βδελυγμία· Ευρωπαϊκή, προϊόν, ΐ ΰ",
                 "The quick brown fox jumps over the lazy dog. 0123456789 (ÀÉÎÕÜ) “quotes” — fi fl",
                 unicodedata.normalize("NFD", "Tiếng Việt Ặ ở Ữ ά ΐ")]
        for loc in LOCATIONS:
            for t in texts:
                for feats in ({}, {"ss01": True}, {"case": True}, {"ss02": True, "ss03": True}):
                    if d.shape(t, loc, feats) != f.shape(t, loc, feats):
                        fail(f"default {key} {loc} {feats}: shapes differently from full: {t[:30]}")
                        break

    for (which, key), n in sorted(counts.items()):
        print(f"{which:8s} {key:8s} Vietnamese NFC=NFD, Greek NFC=NFD, marks, ano teleia: {'ok' if n == 0 else f'{n} failure(s)'}")
    for line in failures:
        print("FAIL " + line)
    print(f"{len(failures)} failure(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

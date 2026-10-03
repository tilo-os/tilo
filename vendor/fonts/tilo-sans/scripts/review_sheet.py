# /// script
# requires-python = ">=3.11"
# dependencies = ["fonttools>=4.60"]
# ///
"""Review sheets for a script Tilo Sans may ship: Tilo Sans beside the
fallback Tilo would otherwise use, so a reader of the script can decide.

    uv run vendor/fonts/tilo-sans/scripts/review_sheet.py --script greek
    uv run vendor/fonts/tilo-sans/scripts/review_sheet.py --script cyrillic

Writes proofs/final/<script>-review/ (tracked):

  glyphs-Regular.png, glyphs-Bold.png
      every letter large: Tilo Sans on top, the fallback under it, in cells
  ui-light.png, ui-dark.png
      interface strings (menus, buttons, settings, notifications, mixed with
      Latin) at 14, 16, 20 and 32 px
  text-light.png, text-dark.png
      running paragraphs at 14, 16, 20 and 32 px, upright and italic

Left column: Tilo Sans with the script (fonts/full/). Right column: what Tilo
draws without it: Tilo Sans for Latin, figures and punctuation, and the
fallback for the script's letters (Geologica at 0.85 x the weight for Greek,
Onest with its ss02 forms for Cyrillic), scaled to Tilo's x-height the way
the toolkit scales fallbacks. Bulgarian and Serbian lines carry their
language tag, so both columns show their local forms.
"""

from __future__ import annotations

import argparse
import html
import sys
import tempfile
from pathlib import Path

from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))
import proof  # noqa: E402

FULL = ROOT / "fonts" / "full"
OUT = ROOT / "proofs" / "final"

SCRIPTS = {
    "greek": {
        "ranges": "U+0370-03FF, U+1F00-1FFF",
        "fallback": proof.MOCK_FONTS / "cand-geologica.ttf",
        "fallback_name": "Geologica (weight x0.85)",
        "weight_scale": 0.85,
        "ss02": False,
        "groups": [
            ("Capitals", "ΑΒΓΔΕΖΗΘΙΚΛΜΝΞΟΠΡΣΤΥΦΧΨΩ"),
            ("Lowercase", "αβγδεζηθικλμνξοπρσςτυφχψω"),
            ("Tonos and dialytika", "ΆΈΉΊΌΎΏΪΫάέήίόύώϊϋΐΰ;·"),
        ],
        "ui": [
            ("menu", "Αρχείο   Επεξεργασία   Προβολή   Παράθυρο   Βοήθεια"),
            ("buttons", "Αποθήκευση · Ακύρωση · Διαγραφή · Μετονομασία · Κοινή χρήση"),
            ("mail", "Εισερχόμενα (12)   Απεσταλμένα   Πρόχειρα   Ανεπιθύμητα   Κάδος"),
            ("sign-in", "Όνομα χρήστη · Κωδικός πρόσβασης · Ξεχάσατε τον κωδικό σας;"),
            ("settings", "Ρυθμίσεις → Ήχος και ειδοποιήσεις → Οθόνη & φωτεινότητα"),
            ("status", "Δεν υπάρχει σύνδεση στο διαδίκτυο. Η αλλαγή αποθηκεύτηκε τοπικά."),
            ("mixed", "Το «Προϋπολογισμός 2026.pdf» (4,2 MB) μεταφορτώθηκε στο Tilo Drive"),
            ("caps", "ΡΥΘΜΙΣΕΙΣ ΑΠΟΡΡΗΤΟΥ · ΝΕΟ ΜΗΝΥΜΑ · ΕΥΡΩΠΑΪΚΗ ΕΝΩΣΗ"),
            ("dialytika", "προϊόν, πρωτεΐνη, αϋπνία, Μαΐου, ταΐζω, ευρωπαϊκός, ΐ ΰ"),
        ],
        "text": [
            ("el", "Το Tilo συγχρονίζει τα αρχεία, τις φωτογραφίες και τις σημειώσεις σας σε όλες "
                   "τις συσκευές σας. Όταν αλλάζετε κάτι στο λάπτοπ, η αλλαγή εμφανίζεται αμέσως στο "
                   "τηλέφωνο· αν δεν υπάρχει σύνδεση, αποθηκεύεται τοπικά και στέλνεται μόλις "
                   "επανέλθει το δίκτυο. Κανένα αρχείο δεν φεύγει από τις συσκευές σας χωρίς την άδειά σας;"),
            ("el", "Ξεσκεπάζω την ψυχοφθόρα βδελυγμία. Γαζέες και μυρτιές δεν θα βρω πια στο χρυσαφί "
                   "ξέφωτο. Η έκδοση 2.4 του Tilo Mail υποστηρίζει IMAP, OAuth και κρυπτογράφηση PGP."),
        ],
    },
    "cyrillic": {
        "ranges": "U+0400-052F, U+1C80-1C8F, U+2DE0-2DFF, U+A640-A69F",
        "fallback": proof.MOCK_FONTS / "cand-onest.ttf",
        "fallback_name": "Onest (ss02)",
        "weight_scale": 1.0,
        "ss02": True,
        "groups": [
            ("Capitals", "АБВГДЕЁЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯ"),
            ("Lowercase", "абвгдеёжзийклмнопрстуфхцчшщъыьэюя"),
            ("Ukrainian, Belarusian, Serbian, Macedonian", "ҐЄІЇЎЂЋЉЊЏЈЅЃЌґєіїўђћљњџјѕѓќ"),
            ("Kazakh", "ӘҒҚҢӨҰҮҺәғқңөұүһ"),
            ("Bulgarian forms (lang bg)", "бвгдежзийклмнптцчшщъюя"),
            ("Serbian forms (lang sr)", "бгдпт"),
        ],
        "ui": [
            ("menu", "Файл   Правка   Вид   Окно   Справка"),
            ("buttons", "Сохранить · Отмена · Удалить · Убрать · Переименовать"),
            ("mail", "Входящие (12)   Отправленные   Черновики   Спам   Корзина"),
            ("sign-in", "Имя пользователя · Пароль · Забыли пароль? · Где мои файлы?"),
            ("status", "Подключение к сети отсутствует. Изменения сохранены на устройстве."),
            ("mixed", "Файл «Бюджет 2026.pdf» (4,2 МБ) загружен в Tilo Drive"),
            ("uk", "Налаштування · Зберегти · Її їжак · Ґанок"),
            ("be", "Налады · Захаваць · Ўвайсці · Абнавіць"),
            ("bg", "Настройки · Запазване · Изтриване · Търсене"),
            ("sr", "Подешавања · Сачувај · Обриши · Ђорђе, љубав, њега, џеп"),
            ("kk", "Параметрлер · Сақтау · Қазақстан · әлем, ғылым, үй, ұл, өз"),
        ],
        "text": [
            ("ru", "Tilo синхронизирует файлы, фотографии и заметки на всех ваших устройствах. Если вы "
                   "что-то измените на ноутбуке, изменение сразу появится на телефоне; без сети оно "
                   "сохранится локально и отправится, как только связь вернётся. Съешь же ещё этих "
                   "мягких французских булок, да выпей чаю."),
            ("uk", "Чуєш їх, доцю, га? Кумедна ж ти, прощайся без ґольфів! Її ім’я — Ярина."),
            ("be", "У Іўі худы жвавы чорт у зялёнай камізэльцы пабег падʼесці фаршу з юшкай."),
            ("bg", "Под южно дърво, цъфтящо в синьо, бягаше малко пухкаво зайче."),
            ("sr", "Љубазни фењерџија чађавог лица хоће да ми покаже штос."),
            ("kk", "Қазақстан — Орталық Азиядағы мемлекет; әр өңірдің өз дәстүрлері бар."),
        ],
    },
}

WEIGHTS = {"Regular": 400, "SemiBold": 600, "Bold": 700}


def fallback_instances(spec, tmp: Path) -> dict[tuple[int, bool], Path]:
    """Static fallback fonts per (Tilo weight, italic): the weight scaled,
    Onest's ss02 forms moved into the cmap."""
    out = {}
    src = TTFont(spec["fallback"])
    axes = {a.axisTag for a in src["fvar"].axes}
    for weight in WEIGHTS.values():
        for italic in (False, True):
            if italic and "slnt" not in axes:
                continue
            loc = {"wght": round(weight * spec["weight_scale"])}
            if "slnt" in axes:
                loc["slnt"] = -12 if italic else 0
            font = instancer.instantiateVariableFont(TTFont(spec["fallback"]), loc)
            if spec["ss02"]:
                apply_feature(font, "ss02")
            path = tmp / f"fb-{weight}-{int(italic)}.ttf"
            font.save(path)
            out[(weight, italic)] = path
    return out


def apply_feature(font: TTFont, tag: str) -> None:
    """Bake a stylistic set's single substitutions into the cmap."""
    gsub = font["GSUB"].table
    lookups = set()
    for fr in gsub.FeatureList.FeatureRecord:
        if fr.FeatureTag == tag:
            lookups.update(fr.Feature.LookupListIndex)
    mapping = {}
    for i in sorted(lookups):
        lookup = gsub.LookupList.Lookup[i]
        for st in lookup.SubTable:
            if lookup.LookupType == 7:
                st = st.ExtSubTable
            if hasattr(st, "mapping"):
                mapping.update(st.mapping)
    for table in font["cmap"].tables:
        if table.isUnicode():
            table.cmap = {cp: mapping.get(g, g) for cp, g in table.cmap.items()}


def faces(spec, fb: dict, theme_ranges: str) -> list[str]:
    tilo_xh = proof.x_height(FULL / "TiloSans[wdth,wght].ttf")
    out = [
        f"@font-face {{ font-family: 'Tilo Sans'; src: url('file://{FULL / 'TiloSans[wdth,wght].ttf'}'); font-weight: 100 900; font-style: normal; }}",
        f"@font-face {{ font-family: 'Tilo Sans'; src: url('file://{FULL / 'TiloSans-Italic[wdth,wght].ttf'}'); font-weight: 100 900; font-style: italic; }}",
        # Tilo without the script, for the fallback column
        f"@font-face {{ font-family: 'Tilo Base'; src: url('file://{FULL / 'TiloSans[wdth,wght].ttf'}'); font-weight: 100 900; font-style: normal; unicode-range: U+0000-036F, U+0530-1EFF, U+2000-2DDF, U+2E00-A63F, U+A6A0-FFFF; }}",
        f"@font-face {{ font-family: 'Tilo Base'; src: url('file://{FULL / 'TiloSans-Italic[wdth,wght].ttf'}'); font-weight: 100 900; font-style: italic; unicode-range: U+0000-036F, U+0530-1EFF, U+2000-2DDF, U+2E00-A63F, U+A6A0-FFFF; }}",
    ]
    for (weight, italic), path in fb.items():
        scale = tilo_xh / proof.x_height(path)
        out.append(f"@font-face {{ font-family: 'Fallback'; src: url('file://{path}'); font-weight: {weight}; "
                   f"font-style: {'italic' if italic else 'normal'}; size-adjust: {scale * 100:.3f}%; unicode-range: {theme_ranges}; }}")
    return out


CSS = """
.cols {{ display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); column-gap: 48px; }}
.col h3 {{ font: 600 12px/1.3 'Tilo Sans'; color: {ink2}; text-transform: uppercase; letter-spacing: .06em; margin-bottom: 10px; }}
.tilo {{ font-family: 'Tilo Sans'; }}
.fb {{ font-family: 'Tilo Base', 'Fallback'; }}
.size {{ font: 500 11px/1.3 'Tilo Sans'; color: {ink2}; margin: 18px 0 6px; border-top: 1px solid {hair}; padding-top: 6px; }}
.ui div {{ margin: .28em 0; }}
.ui .k {{ font: 500 10px 'Tilo Sans'; color: {ink2}; display: inline-block; width: 64px; vertical-align: middle; }}
.p {{ margin: 0 0 .7em; }}
.cells {{ display: flex; flex-wrap: wrap; border-top: 1px solid {hair}; border-left: 1px solid {hair}; margin: 6px 0 20px; }}
.c {{ width: 136px; border-right: 1px solid {hair}; border-bottom: 1px solid {hair}; text-align: center; padding: 6px 0 4px; }}
.c .a, .c .b {{ font-size: 84px; line-height: 116px; height: 116px; }}
.c .b {{ color: {ink2}; border-top: 1px dashed {hair}; }}
.c .n {{ font: 500 10px 'Tilo Sans'; color: {ink2}; }}
.legend {{ font: 500 12px/1.5 'Tilo Sans'; color: {ink2}; margin: 4px 0 8px; }}
"""


def css(theme):
    c = proof.COLORS[theme]
    return CSS.format(ink2=c["ink2"], hair=c["hair"])


def lang_attr(code):
    return f' lang="{code}"' if code else ""


def glyph_page(script, spec, weight_name, fb):
    weight = WEIGHTS[weight_name]
    body = [f"<h1>Tilo Sans {script.title()}, {weight_name}: Tilo Sans (top) and {html.escape(spec['fallback_name'])} (bottom, grey)</h1>",
            "<div class='legend'>Both at Tilo's x-height. The fallback is what Tilo draws if this script is not shipped.</div>"]
    for title, chars in spec["groups"]:
        lang = "bg" if "lang bg" in title else "sr" if "lang sr" in title else ""
        body.append(f"<h2>{html.escape(title)}</h2><div class='cells'{lang_attr(lang)}>")
        for ch in chars:
            body.append(f"<div class='c'><div class='a tilo' style='font-weight:{weight}'>{html.escape(ch)}</div>"
                        f"<div class='b fb' style='font-weight:{weight}'>{html.escape(ch)}</div>"
                        f"<div class='n'>U+{ord(ch):04X}</div></div>")
        body.append("</div>")
    return "\n".join(body)


def columns(spec, inner):
    return ("<div class='cols'>"
            f"<div class='col'><h3>Tilo Sans</h3>{inner('tilo')}</div>"
            f"<div class='col'><h3>Tilo Sans Latin + {html.escape(spec['fallback_name'])}</h3>{inner('fb')}</div>"
            "</div>")


def ui_page(script, spec):
    body = [f"<h1>Tilo Sans {script.title()}: interface strings</h1>"]
    for size in (14, 16, 20, 32):
        def inner(cls, size=size):
            rows = []
            for i, (key, s) in enumerate(spec["ui"]):
                weight = 600 if key in ("buttons", "menu") else 400
                lang = key if key in ("bg", "sr", "uk", "be", "kk") else ""
                rows.append(f"<div class='{cls}' style='font-size:{size}px;font-weight:{weight}'{lang_attr(lang)}>"
                            f"{html.escape(s)}</div>")
            rows.append(f"<div class='{cls}' style='font-size:{size}px;font-weight:700'{lang_attr('')}>"
                        f"{html.escape(spec['ui'][0][1].split()[0])} — {html.escape(spec['ui'][4][1].split('.')[0])}</div>")
            return f"<div class='ui'>{''.join(rows)}</div>"
        body.append(f"<div class='size'>{size} px · 400, menus and buttons 600, last line 700</div>")
        body.append(columns(spec, inner))
    return "\n".join(body)


def text_page(script, spec):
    body = [f"<h1>Tilo Sans {script.title()}: running text</h1>"]
    for size in (14, 16, 20, 32):
        def inner(cls, size=size):
            out = []
            for i, (lang, s) in enumerate(spec["text"] if size < 32 else spec["text"][:2]):
                style = "italic" if i == 1 else "normal"
                out.append(f"<p class='p {cls}' style='font-size:{size}px;line-height:1.45;font-style:{style}'{lang_attr(lang)}>"
                           f"{html.escape(s)}</p>")
            return "".join(out)
        body.append(f"<div class='size'>{size} px · 400, second paragraph italic</div>")
        body.append(columns(spec, inner))
    return "\n".join(body)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--script", choices=sorted(SCRIPTS), required=True)
    args = ap.parse_args()
    spec = SCRIPTS[args.script]
    out = OUT / f"{args.script}-review"
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=proof.PROOFS) as tmp:
        fb = fallback_instances(spec, Path(tmp))
        ff = faces(spec, fb, spec["ranges"])
        for theme in ("light", "dark"):
            extra = f"<style>{css(theme)}</style>"
            for name, body, width in (
                [(f"glyphs-{w}", glyph_page(args.script, spec, w, fb), 1760) for w in ("Regular", "Bold") if theme == "light"]
                + [(f"ui-{theme}", ui_page(args.script, spec), 1760), (f"text-{theme}", text_page(args.script, spec), 1760)]
            ):
                markup = proof.page(extra + body, ff, theme=theme, width=width)
                proof.render(markup, out / f"{name}.png", width=width)
                print(out / f"{name}.png")
    return 0


if __name__ == "__main__":
    sys.exit(main())

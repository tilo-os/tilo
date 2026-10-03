# /// script
# requires-python = ">=3.11"
# dependencies = ["fonttools>=4.60"]
# ///
"""Render Tilo Sans proofs as 2x PNGs with headless Chromium.

    uv run vendor/fonts/tilo-sans/proof.py --set vietnamese
    uv run vendor/fonts/tilo-sans/proof.py --set greek --fallback geologica
    uv run vendor/fonts/tilo-sans/proof.py --set cyrillic --fallback onest
    uv run vendor/fonts/tilo-sans/proof.py --chars "IlI1|" --features ss13 --name il
    uv run vendor/fonts/tilo-sans/proof.py --set vietnamese --italic
    uv run vendor/fonts/tilo-sans/proof.py --set latin --fallback instrument   # regression vs upstream

Pages (each a PNG under proofs/work/<name>/, or proofs/sheets/<name>/ with
--final, the only proof directory git tracks):

  grid-<instance>.png  every glyph at 160 px with its advance box, metric
                       lines and name; one page per instance (Regular, Medium,
                       SemiBold, Bold at normal width; Condensed Regular and
                       Condensed Bold); missing glyphs are struck in red
  text-light.png       running text at 13/14/16/20/26 px, light and dark,
  text-dark.png        regular body plus a SemiBold heading per size
  compare-<fb>.png     the same text and glyphs in Tilo Sans and a fallback
                       (Onest ss02, Geologica light, Noto Sans, upstream
                       Instrument Sans or a font file), scaled to equal
                       x-height, at UI and display sizes
  context-<style>.png  spacing strings (HH?HOH?OHO?OO, nn?non?ono?oo with the
                       script's own control letters) and real words: Regular,
                       Bold, Condensed Regular at 32 px, Regular at 14 px

The fonts come from fonts/ (build.py); the proof stops if a source is newer
than the font it would show.
"""

from __future__ import annotations

import argparse
import html
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unicodedata
from dataclasses import dataclass
from pathlib import Path

from fontTools.ttLib import TTFont

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent.parent.parent
FONTS = ROOT / "fonts"
SOURCES = ROOT / "sources"
PROOFS = ROOT / "proofs"
CHROME = Path.home() / ".cache/ms-playwright/chromium_headless_shell-1243/chrome-headless-shell-linux64/chrome-headless-shell"
# Candidate comparison fonts (Onest, Geologica); override with TILO_MOCK_FONTS.
MOCK_FONTS = Path(os.environ.get("TILO_MOCK_FONTS", REPO / "target/mock/fonts"))

UPRIGHT = FONTS / "TiloSans[wdth,wght].ttf"
ITALIC = FONTS / "TiloSans-Italic[wdth,wght].ttf"
UPSTREAM = ROOT / ".upstream/instrument-sans/fonts/variable"


@dataclass(frozen=True)
class Instance:
    name: str
    wght: int
    wdth: float


INSTANCES = [
    Instance("Regular", 400, 100),
    Instance("Medium", 500, 100),
    Instance("SemiBold", 600, 100),
    Instance("Bold", 700, 100),
    Instance("Condensed-Regular", 400, 75),
    Instance("Condensed-Bold", 700, 75),
]


@dataclass(frozen=True)
class Fallback:
    label: str
    path: Path
    features: str = ""
    weight_scale: float = 1.0  # fallback weight = ours * scale
    italic_path: Path | None = None


def fallbacks() -> dict[str, Fallback]:
    noto = REPO / "crates/sdk/ui-core/assets/fonts"
    return {
        "onest": Fallback("Onest ss02", MOCK_FONTS / "cand-onest.ttf", "'ss02' 1"),
        "geologica": Fallback("Geologica light", MOCK_FONTS / "cand-geologica.ttf", weight_scale=0.85),
        "noto": Fallback("Noto Sans", noto / "NotoSans[wght].ttf", italic_path=noto / "NotoSans-Italic[wght].ttf"),
        "instrument": Fallback("Instrument Sans (upstream)", UPSTREAM / "InstrumentSans[wdth,wght].ttf",
                               italic_path=UPSTREAM / "InstrumentSans-Italic[wdth,wght].ttf"),
    }


# ------------------------------------------------------------------ text sets


def vietnamese_letters() -> str:
    bases = "AĂÂEÊIOÔƠUƯYaăâeêioôơuưy"
    tones = ["", "̀", "́", "̉", "̃", "̣"]
    out = []
    for base in bases:
        for tone in tones:
            out.append(unicodedata.normalize("NFC", base + tone))
    return "".join(out) + "Đđ"


def char_range(start: int, end: int, skip: set[int] = frozenset()) -> str:
    return "".join(chr(u) for u in range(start, end + 1) if u not in skip and unicodedata.category(chr(u)) != "Cn")


GREEK = (
    char_range(0x0391, 0x03A9, {0x03A2}) + "ΆΈΉΊΌΎΏΪΫ"
    + char_range(0x03B1, 0x03C9) + "άέήίόύώϊϋΐΰ" + "΄΅·;"
)
CYRILLIC = (
    char_range(0x0410, 0x044F) + "Ёё"
    + "ЄІЇҐєіїґ"  # Ukrainian
    + "Ўў"  # Belarusian
    + "ЂЈЉЊЋЏђјљњћџ"  # Serbian
    + "ЃЅЌѓѕќ"  # Macedonian
    + "ӘҒҚҢӨҰҮҺәғқңөұүһ"  # Kazakh
    + "№"
)
LATIN = char_range(0x41, 0x5A) + char_range(0x61, 0x7A) + "0123456789"

SAMPLES = {
    "latin": [
        ("en", "The quick brown fox jumps over the lazy dog. Sphinx of black quartz, judge my vow."),
        ("en", "Inbox — 3 unread · Settings › Accounts › Sign-in options (2FA enabled). Last sync 12:45."),
    ],
    "vietnamese": [
        ("vi", "Tiếng Việt có dấu: Cộng hòa Xã hội chủ nghĩa Việt Nam — Độc lập, Tự do, Hạnh phúc."),
        ("vi", "Thư mục đã được đồng bộ. Bạn có muốn mở tệp này bằng ứng dụng khác không?"),
        ("vi", "Ở ngoài trời mưa rơi nhẹ, những người thợ đã nghỉ trưa. Quyển sách được ưa thích nhất."),
        ("vi", "ỐỒỔỖỘ ốồổỗộ ỚỜỞỠỢ ớờởỡợ ỨỪỬỮỰ ứừửữự ẤẦẨẪẬ ấầẩẫậ ẮẰẲẴẶ ắằẳẵặ ẾỀỂỄỆ ếềểễệ"),
    ],
    "greek": [
        ("el", "Ξεσκεπάζω την ψυχοφθόρα βδελυγμία. Γαζέες και μυρτιές δεν θα βρω πια στο χρυσαφί ξέφωτο."),
        ("el", "Ο φάκελος συγχρονίστηκε. Θέλετε να ανοίξετε αυτό το αρχείο με άλλη εφαρμογή;"),
        ("el", "ΡΥΘΜΙΣΕΙΣ · ΛΟΓΑΡΙΑΣΜΟΙ · Ϊ Ϋ ϊ ϋ ΐ ΰ · Άνοιξη, Ήλιος, Ώρα, Έξοδος, Ίδιο, Όλα, Ύψος."),
    ],
    "cyrillic": [
        ("ru", "Съешь же ещё этих мягких французских булок, да выпей чаю. Папка синхронизирована."),
        ("uk", "Чуєш їх, доцю, га? Кумедна ж ти, прощайся без ґольфів! Відкрити файл іншою програмою?"),
        ("be", "У рудога вераб'я ў сховішчы пад фатэлем ляжаць нейкія гаючыя зёлкі."),
        ("bg", "Жълтата дюля беше щастлива, че пухът, който цъфна, замръзна като гьон."),
        ("sr", "Ђаче, уштеду плаћај жаљењем због џиновских цифара. Љубазни фењерџија чађавог лица."),
        ("kk", "Қазақстан Республикасы — тәуелсіз мемлекет. Өңірдегі ұлттық құндылықтар және үміт."),
    ],
}

CONTROLS = {
    "latin": ("H", "O", "n", "o"),
    "cyrillic": ("Н", "О", "н", "о"),
    "greek": ("Η", "Ο", "π", "ο"),
}

SETS = {
    "latin": (LATIN, "latin"),
    "vietnamese": (vietnamese_letters(), "vietnamese"),
    "greek": (GREEK, "greek"),
    "cyrillic": (CYRILLIC, "cyrillic"),
}


def script_of(ch: str) -> str:
    name = unicodedata.name(ch, "")
    if "CYRILLIC" in name:
        return "cyrillic"
    if "GREEK" in name:
        return "greek"
    return "latin"


# ------------------------------------------------------------------ fonts


def check_fresh(paths: list[Path]) -> None:
    newest = 0.0
    newest_path = None
    for dirpath, _, files in os.walk(SOURCES):
        for name in files:
            mtime = os.stat(os.path.join(dirpath, name)).st_mtime
            if mtime > newest:
                newest, newest_path = mtime, os.path.join(dirpath, name)
    for path in paths:
        if not path.exists():
            sys.exit(f"{path.relative_to(REPO)} is missing. Build first:\n  uv run vendor/fonts/tilo-sans/build.py")
        if path.stat().st_mtime < newest:
            sys.exit(f"{path.name} is older than {Path(newest_path).relative_to(REPO)}. Rebuild first:\n"
                     "  uv run vendor/fonts/tilo-sans/build.py")


def x_height(path: Path) -> float:
    font = TTFont(path, lazy=True)
    xh = getattr(font["OS/2"], "sxHeight", 0)
    if not xh:
        glyph_set = font.getGlyphSet()
        from fontTools.pens.boundsPen import BoundsPen

        pen = BoundsPen(glyph_set)
        glyph_set[font.getBestCmap()[ord("x")]].draw(pen)
        xh = pen.bounds[3]
    return xh / font["head"].unitsPerEm


def cmap_names(path: Path) -> dict[int, str]:
    return TTFont(path, lazy=True).getBestCmap()


# ------------------------------------------------------------------ html

COLORS = {
    "light": {"bg": "oklch(0.995 0.002 255)", "ink": "oklch(0.18 0.006 255)", "ink2": "oklch(0.43 0.008 255)",
              "hair": "oklch(0 0 0 / 0.10)", "guide": "oklch(0.62 0.12 255 / 0.35)", "box": "oklch(0.75 0.10 165 / 0.18)"},
    "dark": {"bg": "oklch(0.215 0.008 255)", "ink": "oklch(0.96 0.004 255)", "ink2": "oklch(0.76 0.006 255)",
             "hair": "oklch(1 0 0 / 0.10)", "guide": "oklch(0.72 0.12 255 / 0.40)", "box": "oklch(0.74 0.14 165 / 0.16)"},
}


def font_face(family: str, path: Path, size_adjust: float = 1.0) -> str:
    return (f"@font-face {{ font-family: '{family}'; src: url('file://{path}'); "
            f"font-weight: 100 900; font-stretch: 50% 200%; size-adjust: {size_adjust * 100:.3f}%; }}")


def page(body: str, faces: list[str], theme: str = "light", width: int = 1400) -> str:
    c = COLORS[theme]
    return f"""<!doctype html><html><head><meta charset="utf-8"><style>
{chr(10).join(faces)}
* {{ box-sizing: border-box; margin: 0; padding: 0; }}
html {{ background: {c['bg']}; }}
body {{ width: {width}px; padding: 32px 40px 40px; background: {c['bg']}; color: {c['ink']};
        font-family: 'Tilo Sans'; -webkit-font-smoothing: antialiased; }}
.meta {{ font: 500 12px/1.4 'Tilo Sans'; color: {c['ink2']}; letter-spacing: 0.01em; }}
h1 {{ font: 600 20px/1.3 'Tilo Sans'; margin-bottom: 4px; }}
h2 {{ font: 600 13px/1.3 'Tilo Sans'; color: {c['ink2']}; margin: 22px 0 8px; text-transform: uppercase; letter-spacing: 0.06em; }}
.grid {{ display: flex; flex-wrap: wrap; gap: 0; margin-top: 16px; border-top: 1px solid {c['hair']}; border-left: 1px solid {c['hair']}; }}
.cell {{ width: 220px; height: 300px; position: relative; border-right: 1px solid {c['hair']}; border-bottom: 1px solid {c['hair']}; overflow: hidden; }}
.cell .g {{ position: absolute; left: 0; right: 0; top: 18px; text-align: center; font-size: 160px; line-height: 250px; height: 250px; }}
.cell .g span {{ background: {c['box']}; }}
.cell .line {{ position: absolute; left: 0; right: 0; height: 0; border-top: 1px solid {c['guide']}; }}
.cell .lab {{ position: absolute; left: 8px; right: 8px; bottom: 6px; font: 500 11px/1.3 'Tilo Sans'; color: {c['ink2']}; display: flex; justify-content: space-between; }}
.cell.missing {{ background: oklch(0.65 0.2 25 / 0.15); }}
.cell.missing .lab {{ color: oklch(0.6 0.2 25); }}
.row {{ display: flex; gap: 24px; align-items: baseline; margin: 6px 0; }}
.row .k {{ width: 140px; flex: none; }}
.block {{ margin: 10px 0 18px; }}
</style></head><body>{body}</body></html>"""


OUR_CMAP: set[int] = set()


def ours(text: str) -> str:
    """Escape text for a Tilo Sans line, striking characters the font lacks:
    Chromium would draw them from a system font and pass them off as ours."""
    out = []
    for ch in text:
        if ch.isspace() or ord(ch) in OUR_CMAP:
            out.append(html.escape(ch))
        else:
            out.append(f'<span style="color:oklch(0.6 0.2 25); text-decoration: line-through">{html.escape(ch)}</span>')
    return "".join(out)


def css_font(wght: int, wdth: float, features: str = "", italic: bool = False) -> str:
    feats = f"font-feature-settings: {features};" if features else ""
    return f"font-variation-settings: 'wght' {wght}, 'wdth' {wdth}; font-weight: {wght}; {feats}"


def grid_page(chars: list[str], inst: Instance, args, names: dict[int, str]) -> str:
    # Metric lines at 160 px for a 1000-UPM font. The line box is 250 px high
    # and centred on the em box (ascent 970 + descent 250 = 1220 units).
    size = 160
    line_h = 250
    content = (970 + 250) / 1000 * size
    baseline = (line_h - content) / 2 + 970 / 1000 * size + 18
    lines = [baseline, baseline - 510 / 1000 * size, baseline - 720 / 1000 * size,
             baseline - 740 / 1000 * size, baseline + 205 / 1000 * size]
    cells = []
    for ch in chars:
        cp = ord(ch[0]) if len(ch) == 1 else None
        name = names.get(cp, "") if cp is not None else ""
        missing = cp is not None and cp not in names
        label = f"U+{cp:04X}" if cp is not None else " ".join(f"U+{ord(c):04X}" for c in ch)
        guides = "".join(f'<div class="line" style="top:{y:.1f}px"></div>' for y in lines)
        cells.append(
            f'<div class="cell{" missing" if missing else ""}">{guides}'
            f'<div class="g" lang="{args.lang}"><span>{html.escape(ch)}</span></div>'
            f'<div class="lab"><span>{html.escape(name or ("MISSING" if missing else ""))}</span><span>{label}</span></div></div>'
        )
    title = f"{args.title} — {inst.name.replace('-', ' ')}{' Italic' if args.italic else ''}"
    meta = f"wght {inst.wght} · wdth {inst.wdth:g} · {len(chars)} glyphs{(' · features ' + args.features) if args.features else ''} · lines: baseline, x-height 510, cap 720, ascender 740, descender −205"
    style = css_font(inst.wght, inst.wdth, args.feature_css)
    return f'<h1>{html.escape(title)}</h1><div class="meta">{meta}</div><div class="grid" style="{style}">{"".join(cells)}</div>'


def text_page(samples: list[tuple[str, str]], args, theme: str) -> str:
    out = [f"<h1>{html.escape(args.title)} — running text, {theme}{' italic' if args.italic else ''}</h1>",
           '<div class="meta">body at Regular 400, heading at SemiBold 600, normal width; line-height 1.45</div>']
    style = args.feature_css and f"font-feature-settings: {args.feature_css};"
    for size in (13, 14, 16, 20, 26):
        out.append(f"<h2>{size} px</h2>")
        heading = samples[0][1].split(".")[0]
        out.append(f'<div lang="{samples[0][0]}" style="{css_font(600, 100)} {style} font-size:{size}px; line-height:1.3; margin-bottom:4px">{ours(heading)}</div>')
        for lang, text in samples:
            out.append(f'<p lang="{lang}" style="{css_font(400, 100)} {style} font-size:{size}px; line-height:1.45; max-width:{size * 48}px; margin-bottom:4px">{ours(text)}</p>')
    return "".join(out)


def compare_page(chars: list[str], samples: list[tuple[str, str]], args, fb: Fallback, fb_family: str) -> str:
    ours_style = f"font-family:'Tilo Sans'; {args.feature_css and 'font-feature-settings:' + args.feature_css + ';'}"

    def theirs(wght: int) -> str:
        # single quotes: the style sits in a double-quoted HTML attribute
        feats = f"font-feature-settings: {fb.features.replace(chr(34), chr(39))};" if fb.features else ""
        w = round(wght * fb.weight_scale)
        return f"font-family:'{fb_family}'; font-variation-settings: 'wght' {w}; font-weight:{w}; {feats}"

    out = [f"<h1>{html.escape(args.title)} — Tilo Sans vs {html.escape(fb.label)}</h1>",
           f'<div class="meta">fallback scaled to Tilo Sans’ x-height; fallback weight = Tilo weight × {fb.weight_scale:g}'
           f'{(" · fallback features " + html.escape(fb.features)) if fb.features else ""}</div>']
    # Spaced, so contextual forms (Greek all-caps tonos removal, ligatures)
    # do not hide the glyphs under comparison.
    glyph_line = " ".join(chars)
    for size, wght in ((96, 400), (96, 700), (48, 400)):
        out.append(f"<h2>glyphs {size} px · wght {wght}</h2>")
        for label, style in (("Tilo Sans", ours_style + css_font(wght, 100)), (fb.label, theirs(wght))):
            out.append(f'<div class="row"><div class="k meta">{html.escape(label)}</div>'
                       f'<div lang="{args.lang}" style="{style} font-size:{size}px; line-height:1.25; overflow-wrap:anywhere">{(ours if label == "Tilo Sans" else html.escape)(glyph_line)}</div></div>')
    for size in (13, 14, 16, 20, 26):
        out.append(f"<h2>{size} px</h2>")
        for lang, text in samples[:3]:
            for label, style in (("Tilo Sans", ours_style + css_font(400, 100)), (fb.label, theirs(400))):
                out.append(f'<div class="row"><div class="k meta">{html.escape(label)}</div>'
                           f'<div lang="{lang}" style="{style} font-size:{size}px; line-height:1.45">{(ours if label == "Tilo Sans" else html.escape)(text)}</div></div>')
    return "".join(out)


CONTEXT_SETTINGS = [("Regular", 400, 100, 32), ("Bold", 700, 100, 32), ("Condensed-Regular", 400, 75, 32), ("Regular-14px", 400, 100, 14)]


def context_page(chars: list[str], samples: list[tuple[str, str]], args, setting) -> str:
    label, wght, wdth, size = setting
    out = [f"<h1>{html.escape(args.title)} — context and spacing, {label.replace('-', ' ')}</h1>",
           f'<div class="meta">wght {wght} · wdth {wdth:g} · {size} px · each glyph between its script’s straight and round control letters; then words</div>']
    lines = []
    for ch in chars:
        script = script_of(ch[0])
        H, O, n, o = CONTROLS[script]
        if ch.isupper() or (not ch.islower() and not ch.isalpha()):
            lines.append(f"{H}{H}{ch}{H}{O}{H}{ch}{O}{H}{O}{ch}{O}{O}")
        else:
            lines.append(f"{n}{n}{ch}{n}{o}{n}{ch}{o}{n}{o}{ch}{o}{o}")
    style = args.feature_css and f"font-feature-settings: {args.feature_css};"
    out.append(f'<div class="block" lang="{args.lang}" style="{css_font(wght, wdth)} {style} font-size:{size}px; line-height:1.35; display:flex; flex-wrap:wrap; column-gap:{size}px; margin-top:16px">'
               + "".join(f"<span>{ours(line)}</span>" for line in lines) + "</div>")
    for lang, text in samples:
        out.append(f'<p lang="{lang}" style="{css_font(wght, wdth)} {style} font-size:{size}px; line-height:1.4; margin-bottom:6px">{ours(text)}</p>')
    return "".join(out)


# ------------------------------------------------------------------ render


def chrome(args_list: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run([str(CHROME), "--no-sandbox", "--hide-scrollbars", "--disable-gpu",
                           "--virtual-time-budget=4000", *args_list], capture_output=True, text=True, timeout=120)


def render(markup: str, out: Path, width: int = 1400) -> Path:
    """Two passes: measure the laid-out height, then screenshot at 2x."""
    out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=PROOFS) as tmp:
        src = Path(tmp) / (out.stem + ".html")
        measure = markup.replace(
            "</body>",
            "<script>document.fonts.ready.then(()=>{document.body.setAttribute('data-h',"
            "Math.ceil(document.documentElement.scrollHeight));});</script></body>")
        src.write_text(measure)
        dom = chrome(["--dump-dom", f"--window-size={width},800", f"file://{src}"]).stdout
        height = 1200
        marker = 'data-h="'
        if marker in dom:
            height = int(dom.split(marker, 1)[1].split('"', 1)[0])
        height = min(height, 16000)
        src.write_text(markup)
        result = chrome([f"--window-size={width},{height}", "--force-device-scale-factor=2",
                         f"--screenshot={out}", f"file://{src}"])
        if not out.exists():
            sys.exit(f"chromium did not write {out}:\n{result.stderr[-2000:]}")
    if args_keep_html:
        (out.parent / "html").mkdir(exist_ok=True)
        (out.parent / "html" / (out.stem + ".html")).write_text(markup)
    return out


args_keep_html = False


def main() -> int:
    global args_keep_html
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--set", choices=sorted(SETS), help="a predefined character set and sample text")
    parser.add_argument("--chars", help="characters to proof (added to --set); spaces are ignored")
    parser.add_argument("--text", action="append", default=[], help="sample text (repeatable); replaces the set's samples")
    parser.add_argument("--lang", default="", help="BCP 47 language for the glyph pages (locl), e.g. bg, sr, vi")
    parser.add_argument("--features", default="", help="OpenType features for Tilo Sans, e.g. 'ss13' or 'ss13,case'")
    parser.add_argument("--fallback", action="append", default=[],
                        help="onest | geologica | noto | instrument | path/to/font.ttf (repeatable); default per set")
    parser.add_argument("--italic", action="store_true", help="proof the italic")
    parser.add_argument("--pages", default="grid,text,compare,context", help="comma list of grid,text,compare,context")
    parser.add_argument("--instances", default="", help="comma list of instance names; default all six")
    parser.add_argument("--name", help="output folder name; default the set name")
    parser.add_argument("--final", action="store_true", help="write to proofs/sheets/<name>/ (tracked in git)")
    parser.add_argument("--keep-html", action="store_true", help="also keep the generated HTML next to the PNGs")
    args = parser.parse_args()
    args_keep_html = args.keep_html

    if not CHROME.exists():
        sys.exit(f"headless Chromium not found at {CHROME}")
    if not args.set and not args.chars:
        parser.error("give --set or --chars")

    font_path = ITALIC if args.italic else UPRIGHT
    check_fresh([font_path])

    chars: list[str] = []
    samples: list[tuple[str, str]] = []
    script_key = "latin"
    if args.set:
        set_chars, script_key = SETS[args.set]
        chars += list(set_chars)
        samples = list(SAMPLES[script_key])
    if args.chars:
        chars += [c for c in args.chars if not c.isspace() and c not in chars]
    if args.text:
        samples = [(args.lang or "und", t) for t in args.text]
    if not samples:
        samples = list(SAMPLES["latin"])
    if not args.lang:
        args.lang = {"vietnamese": "vi", "greek": "el", "cyrillic": "ru"}.get(args.set or "", "en")
    args.feature_css = ", ".join(f"'{f.strip()}' 1" for f in args.features.split(",") if f.strip())
    name = args.name or args.set or "custom"
    args.title = f"Tilo Sans · {name}"
    out_dir = (PROOFS / "sheets" / name) if args.final else (PROOFS / "work" / name)
    if args.italic:
        out_dir = out_dir.parent / f"{out_dir.name}-italic"
    PROOFS.mkdir(exist_ok=True)

    faces = [font_face("Tilo Sans", font_path)]
    our_xh = x_height(font_path)
    names = cmap_names(font_path)
    OUR_CMAP.update(names)
    pages = [p.strip() for p in args.pages.split(",") if p.strip()]
    wanted = [i for i in INSTANCES if not args.instances or i.name in args.instances.split(",")]
    written: list[Path] = []

    if "grid" in pages:
        # Pages of at most 6 rows of 6 so a sheet stays legible when viewed whole.
        per_page = 36
        for inst in wanted:
            for start in range(0, len(chars), per_page):
                part = chars[start:start + per_page]
                suffix = f"-{start // per_page + 1}" if len(chars) > per_page else ""
                body = grid_page(part, inst, args, names)
                written.append(render(page(body, faces, width=6 * 220 + 82), out_dir / f"grid-{inst.name}{suffix}.png", 6 * 220 + 82))
    if "text" in pages:
        for theme in ("light", "dark"):
            written.append(render(page(text_page(samples, args, theme), faces, theme), out_dir / f"text-{theme}.png"))
    if "compare" in pages:
        chosen = args.fallback or {"greek": ["geologica"], "cyrillic": ["onest"], "vietnamese": ["noto"]}.get(args.set or "", ["instrument"])
        table = fallbacks()
        for key in chosen:
            fb = table.get(key) or Fallback(Path(key).stem, Path(key).resolve())
            path = (fb.italic_path or fb.path) if args.italic else fb.path
            if not path.exists():
                sys.exit(f"fallback font {path} not found")
            scale = our_xh / x_height(path)
            fb_faces = faces + [font_face("Fallback", path, scale)]
            label = key if key in table else Path(key).stem
            written.append(render(page(compare_page(chars, samples, args, fb, "Fallback"), fb_faces), out_dir / f"compare-{label}.png"))
    if "context" in pages:
        for setting in CONTEXT_SETTINGS:
            body = context_page(chars, samples, args, setting)
            written.append(render(page(body, faces), out_dir / f"context-{setting[0]}.png"))

    for path in written:
        print(path.relative_to(REPO))
    return 0


if __name__ == "__main__":
    sys.exit(main())

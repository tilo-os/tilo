# /// script
# requires-python = ">=3.11"
# dependencies = ["fonttools>=4.60", "pillow>=10"]
# ///
"""Confusable characters with the distinct forms on, at UI sizes and large.

    uv run vendor/fonts/tilo-sans/scripts/il_sheet.py            # -> proofs/work/il-sheet/
    uv run vendor/fonts/tilo-sans/scripts/il_sheet.py --final    # -> proofs/sheets/il-sheet/

Pages:

  ui-<theme>.png      the confusable sets (I l 1 | ! i j, O 0 o, rn m, cl d)
                      and identifier strings at 12, 13 and 14 px, rendered at
                      1x (one CSS pixel per device pixel, as on a standard
                      display) and enlarged 3x without smoothing, so every
                      pixel the reader would get is visible. Regular, Bold and
                      Condensed Bold, upright and italic; each line with
                      ss13 and zero off, then on.
  ui2x-<theme>.png    the same at 2x (HiDPI), not enlarged
  pairs-<style>.png   the tight pairs of the serif I and tailed l at 96 px:
                      their diagonal neighbours (1 4 ¿ accents carons) and
                      their side-by-side ones, off and on

Uses fonts/full/ (everything, as review sees it).
"""

from __future__ import annotations

import argparse
import html
import sys
import tempfile
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))
import proof  # noqa: E402

FULL = ROOT / "fonts" / "full"
UPRIGHT = FULL / "TiloSans[wdth,wght].ttf"
ITALIC = FULL / "TiloSans-Italic[wdth,wght].ttf"

STYLES = [  # label, wght, wdth, italic
    ("Regular", 400, 100, False),
    ("Bold", 700, 100, False),
    ("Condensed Bold", 700, 75, False),
    ("Italic", 400, 100, True),
    ("Bold Italic", 700, 100, True),
    ("Condensed Bold Italic", 700, 75, True),
]
SIZES = (12, 13, 14)

CONFUSABLE = [
    "Il1|!ij  I1 1I Il lI l1 1l I| l| I! l! Ii lj il1",
    "O0o  0O O0 o0 0o  rn m rnm  cl d cld  vv w",
]
IDENTIFIERS = [
    "Illinois1 lIl1I|l Ill1Il ID-I1l0O ItemId IfNull AB4I1 X0I9 l4 I4 4I",
    "¿Isla? Iï ïl lï Iì íI řI ťI ľA ľT tI IJ l·l Ĳl ŁI ł4 Ỉ Ị Ĩ",
    "user_Il1 pass: lI1O0o mail.l1@x.io 10.01.2010 0xI1lO0 v1.10l",
]
PAIRS_DIAGONAL = "I1 1I I4 4I ¿I Iï ïI íI Iì řI ťI ťÎ ľI ľT ľY ľ7 ľï lï ïl lî îl ł4 l4 l1"
PAIRS_SIDE = "II IJ lI tI ll LI Il1 lIl HIH HlH nIn nln ÏÏ ÎÏ ĪĪ IO OI Io Ie It Iv lv lt l0 0I"


def faces() -> list[str]:
    return [
        f"@font-face {{ font-family: 'TS'; src: url('file://{UPRIGHT}'); font-weight: 100 900; font-stretch: 50% 200%; font-style: normal; }}",
        f"@font-face {{ font-family: 'TS'; src: url('file://{ITALIC}'); font-weight: 100 900; font-stretch: 50% 200%; font-style: italic; }}",
    ]


def font_css(wght, wdth, italic, on):
    feats = "'ss13', 'zero'" if on else "normal"
    return (f"font-family: 'TS'; font-weight: {wght}; font-stretch: {wdth}%; "
            f"font-style: {'italic' if italic else 'normal'}; font-feature-settings: {feats};")


def page(body: str, theme: str, width: int) -> str:
    c = proof.COLORS[theme]
    return f"""<!doctype html><html><head><meta charset="utf-8"><style>
{chr(10).join(faces())}
* {{ box-sizing: border-box; margin: 0; padding: 0; }}
html, body {{ background: {c['bg']}; color: {c['ink']}; }}
body {{ width: {width}px; padding: 12px 14px; font-family: 'TS'; }}
.k {{ font: 500 9px/1.2 'TS'; color: {c['ink2']}; margin-top: 8px; }}
.l {{ white-space: pre; line-height: 1.45; }}
.off {{ color: {c['ink2']}; }}
h2 {{ font: 600 11px/1.3 'TS'; color: {c['ink2']}; margin: 14px 0 2px; text-transform: uppercase; letter-spacing: 0.06em; }}
.big {{ white-space: pre; line-height: 1.25; }}
</style></head><body>{body}</body></html>"""


def shot(markup: str, out: Path, width: int, scale: int) -> None:
    out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=proof.PROOFS) as tmp:
        src = Path(tmp) / (out.stem + ".html")
        measure = markup.replace(
            "</body>",
            "<script>document.fonts.ready.then(()=>{document.body.setAttribute('data-h',"
            "Math.ceil(document.documentElement.scrollHeight));});</script></body>")
        src.write_text(measure)
        dom = proof.chrome(["--dump-dom", f"--window-size={width},800", f"file://{src}"]).stdout
        height = 1200
        if 'data-h="' in dom:
            height = int(dom.split('data-h="', 1)[1].split('"', 1)[0])
        src.write_text(markup)
        r = proof.chrome([f"--window-size={width},{min(height, 16000)}", f"--force-device-scale-factor={scale}",
                          f"--screenshot={out}", f"file://{src}"])
        if not out.exists():
            sys.exit(f"chromium did not write {out}:\n{r.stderr[-2000:]}")


def ui_body() -> str:
    out = []
    for label, wght, wdth, italic in STYLES:
        out.append(f"<h2>{label}</h2>")
        for size in SIZES:
            out.append(f'<div class="k">{size} px · grey: ss13 and zero off · ink: on</div>')
            for text in CONFUSABLE + IDENTIFIERS:
                t = html.escape(text)
                out.append(f'<div class="l off" style="{font_css(wght, wdth, italic, False)} font-size:{size}px">{t}</div>')
                out.append(f'<div class="l" style="{font_css(wght, wdth, italic, True)} font-size:{size}px">{t}</div>')
    return "".join(out)


def pairs_body(wght, wdth, italic) -> str:
    out = []
    for title, text in (("diagonal neighbours", PAIRS_DIAGONAL), ("side by side", PAIRS_SIDE)):
        out.append(f"<h2>{title} · grey: off · ink: ss13 and zero on</h2>")
        words = text.split()
        for i in range(0, len(words), 8):
            chunk = html.escape("  ".join(words[i:i + 8]))
            out.append(f'<div class="big off" style="{font_css(wght, wdth, italic, False)} font-size:96px">{chunk}</div>')
            out.append(f'<div class="big" style="{font_css(wght, wdth, italic, True)} font-size:96px">{chunk}</div>')
    return "".join(out)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--final", action="store_true", help="write to proofs/sheets/il-sheet/")
    ap.add_argument("--pages", default="ui,ui2x,pairs")
    a = ap.parse_args()
    for f in (UPRIGHT, ITALIC):
        if not f.exists():
            sys.exit(f"{f} missing: run build.py first")
    newest = max(p.stat().st_mtime for p in proof.SOURCES.rglob("*") if p.is_file())
    if min(UPRIGHT.stat().st_mtime, ITALIC.stat().st_mtime) < newest:
        sys.exit("fonts/full/ is older than the sources: run build.py first")
    out_dir = proof.PROOFS / ("sheets" if a.final else "work") / "il-sheet"
    pages = a.pages.split(",")
    for theme in ("light", "dark"):
        if "ui" in pages:
            p = out_dir / f"ui-{theme}.png"
            shot(page(ui_body(), theme, 620), p, 620, 1)
            im = Image.open(p)
            im.resize((im.width * 3, im.height * 3), Image.NEAREST).save(p)
            print(p)
        if "ui2x" in pages:
            p = out_dir / f"ui2x-{theme}.png"
            shot(page(ui_body(), theme, 620), p, 620, 2)
            print(p)
    if "pairs" in pages:
        for label, wght, wdth, italic in STYLES:
            p = out_dir / f"pairs-{label.replace(' ', '-')}.png"
            shot(page(pairs_body(wght, wdth, italic), "light", 1500), p, 1500, 1)
            print(p)
    return 0


if __name__ == "__main__":
    sys.exit(main())

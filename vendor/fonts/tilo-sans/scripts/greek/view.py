# /// script
# dependencies = ["ufoLib2", "fonttools"]
# ///
"""Quick UFO viewer: renders glyphs straight from the sources (no build).

view.py OUT.png --masters Regular,Bold --glyphs "alpha beta" --text "αβγ" --size 160
"""
import argparse
import subprocess
from pathlib import Path

import ufoLib2
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.boundsPen import ControlBoundsPen

SRC = Path(__file__).resolve().parents[2] / "sources"
WORK = Path(__file__).resolve().parents[2] / "proofs" / "work" / "greek-dev"
CHROME = "/home/quinten/.cache/ms-playwright/chromium_headless_shell-1243/chrome-headless-shell-linux64/chrome-headless-shell"


def glyph_path(font, name):
    pen = SVGPathPen(font)
    font[name].draw(pen)
    return pen.getCommands()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--masters", default="Regular")
    ap.add_argument("--glyphs", default="")
    ap.add_argument("--text", action="append", default=[])
    ap.add_argument("--size", type=float, default=160)
    ap.add_argument("--tsize", type=float, default=64)
    ap.add_argument("--width", type=int, default=1600)
    ap.add_argument("--height", type=int, default=1200)
    ap.add_argument("--dark", action="store_true")
    ap.add_argument("--outline", action="store_true")
    args = ap.parse_args()

    fg, bg = ("#e8e8e8", "#1c1c1e") if args.dark else ("#111", "#fff")
    html = [f"<html><body style='margin:8px;background:{bg};font:11px sans-serif;color:#888'>"]
    for m in args.masters.split(","):
        font = ufoLib2.Font.open(SRC / f"TiloSans-{m}.ufo")
        cmap = {}
        for g in font:
            for u in g.unicodes:
                cmap[chr(u)] = g.name
        html.append(f"<div>{m}</div>")
        names = args.glyphs.split()
        if names:
            s = args.size / 1000
            row = []
            for n in names:
                if n not in font:
                    row.append(f"<span style='color:red'>{n}?</span>")
                    continue
                w = font[n].width or 300
                d = glyph_path(font, n)
                lines = "".join(
                    f"<line x1='-50' x2='{w+50}' y1='{y}' y2='{y}' stroke='{c}' stroke-width='3'/>"
                    for y, c in ((0, "#4a90e2"), (510, "#e24a4a"), (720, "#4ae24a"), (-205, "#aaa"), (740, "#ddd"))
                )
                fill = "none' stroke='#000' stroke-width='3" if args.outline else fg
                pts = ""
                if args.outline:
                    for c in font[n].contours:
                        for p in c.points:
                            r = 7 if p.type else 4
                            col = "#e00" if p.type else "#08f"
                            pts += f"<circle cx='{p.x}' cy='{p.y}' r='{r}' fill='{col}'/>"
                svg = (f"<svg width='{(w+60)*s}' height='{1050*s}' viewBox='-30 -260 {w+60} 1050' style='margin:2px'>"
                       f"<g transform='scale(1,-1) translate(0,-530)'>"
                       f"<rect x='0' y='-205' width='{w}' height='945' fill='{'#2a2a2e' if args.dark else '#f3f3f7'}'/>{lines}"
                       f"<path d='{d}' fill='{fill}'/>{pts}</g></svg>")
                row.append(f"<span style='display:inline-block;text-align:center'>{svg}<br>{n}</span>")
            html.append("<div style='display:flex;flex-wrap:wrap'>" + "".join(row) + "</div>")
        for text in args.text:
            s = args.tsize / 1000
            x = 0
            parts = []
            for ch in text:
                n = cmap.get(ch)
                if n is None:
                    x += 300
                    continue
                parts.append(f"<path transform='translate({x},0)' d='{glyph_path(font, n)}' fill='{fg}'/>")
                x += font[n].width
            svg = (f"<svg width='{x*s+10}' height='{1100*s}' viewBox='0 -280 {x+10} 1100'>"
                   f"<g transform='scale(1,-1) translate(0,-540)'>{''.join(parts)}</g></svg>")
            html.append(f"<div>{svg}</div>")
    html.append("</body></html>")
    out = WORK / args.out
    out.parent.mkdir(parents=True, exist_ok=True)
    htmlp = out.with_suffix(".html")
    htmlp.write_text("".join(html))
    subprocess.run([CHROME, "--no-sandbox", "--hide-scrollbars", f"--window-size={args.width},{args.height}",
                    f"--screenshot={out}", f"file://{htmlp}"], capture_output=True)
    print(out)


if __name__ == "__main__":
    main()

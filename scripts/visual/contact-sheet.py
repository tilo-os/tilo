#!/usr/bin/env python3
"""Compose the per-app sweep PNGs into one labelled contact sheet and report
which apps painted.

Whether an app rendered is taken from the compositor's own per-surface
frame counters, recorded by the sweep (`frames` in results.json).
`frame_seq > 0` means the compositor took a frame from that client.

Pixel clustering below is only a cross-check for rows with no counters.
Apps start empty by design, so a first frame can resemble the placeholder.

For fallback rows, an app with no frame gets two centred text lines (the
app-id and "waiting for first frame") over a flat card. These shots are
near-identical, differing only in the app-id glyphs.

So: cluster the shots by near-identity. Any group of >= 2 shots agreeing
on >= 99.0% of pixels is the placeholder class. A shot with no near-twin
rendered something only it could have rendered.
"""
from __future__ import annotations

import argparse
import json
import pathlib
import sys

from PIL import Image, ImageChops, ImageDraw, ImageFont

# Pillow moved the resampling constants onto Image.Resampling and dropped
# the old top-level aliases. Bind them once so this runs on either.
_RS = getattr(Image, "Resampling", Image)
BILINEAR = _RS.BILINEAR
LANCZOS = _RS.LANCZOS

# Two shots agreeing on at least this fraction of pixels are the same
# picture for our purposes. The placeholder differs between apps only in
# the app-id text; rendered content differs elsewhere.
SAME_PICTURE = 0.990
# Downscale before comparing: kills PNG-level noise, keeps layout.
CMP_SIZE = (320, 180)

CELL_W, CELL_H = 520, 293
LABEL_H = 30
PAD = 8
BG = (17, 19, 26)
FG = (232, 234, 240)
DIM = (150, 156, 170)
PLACEHOLDER_TINT = (196, 92, 92)


# Per-channel tolerance: absorbs resampling and dither noise without
# letting different pictures count as equal.
CHANNEL_TOLERANCE = 6


def load_cmp(path: pathlib.Path) -> Image.Image:
    im = Image.open(path)
    im.load()  # force decode here so a truncated file raises now, not later
    return im.convert("RGB").resize(CMP_SIZE, BILINEAR)


def agreement(a: Image.Image, b: Image.Image) -> float:
    """Fraction of pixels whose largest channel difference is within
    tolerance. Done through PIL's C paths — a Python loop over 57k
    pixels for every pair of ~47 shots is minutes of pure interpreter."""
    diff = ImageChops.difference(a, b)
    r, g, bl = diff.split()
    # Per-pixel max across channels.
    worst = ImageChops.lighter(ImageChops.lighter(r, g), bl)
    hist = worst.histogram()
    same = sum(hist[: CHANNEL_TOLERANCE + 1])
    return same / (a.width * a.height)


def classify(
    shots: dict[str, pathlib.Path],
) -> tuple[dict[str, bool], list[list[str]], set[str]]:
    """-> ({app: rendered_real_content}, clusters, unreadable)"""
    names = sorted(shots)
    data = {}
    unreadable: set[str] = set()
    for n in names:
        try:
            data[n] = load_cmp(shots[n])
        except Exception as e:  # noqa: BLE001
            # An unreadable shot is its own outcome. Folding it into
            # "placeholder" would understate the fleet on the strength of
            # a truncated file.
            print(f"  ! could not read {shots[n]}: {e}", file=sys.stderr)
            unreadable.add(n)
    names = [n for n in names if n in data]

    unassigned = list(names)
    clusters: list[list[str]] = []
    while unassigned:
        head = unassigned.pop(0)
        group = [head]
        rest = []
        for other in unassigned:
            if agreement(data[head], data[other]) >= SAME_PICTURE:
                group.append(other)
            else:
                rest.append(other)
        unassigned = rest
        clusters.append(group)

    clusters.sort(key=len, reverse=True)
    placeholder = set()
    # A cluster of two or more identical pictures is the placeholder
    # class: distinct apps cannot draw the same pixels.
    for group in clusters:
        if len(group) >= 2:
            placeholder.update(group)
    return {n: n not in placeholder for n in names}, clusters, unreadable


def font(size: int):
    for cand in (
        "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    ):
        if pathlib.Path(cand).exists():
            try:
                return ImageFont.truetype(cand, size)
            except Exception:  # noqa: BLE001
                pass
    return ImageFont.load_default()


def short(package_id: str) -> str:
    return package_id.rsplit(".", 1)[-1]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--sweep", type=pathlib.Path, required=True,
                    help="sweep --out directory holding results.json + PNGs")
    ap.add_argument("--out", type=pathlib.Path, required=True)
    ap.add_argument("--cols", type=int, default=6)
    args = ap.parse_args()

    results = json.loads((args.sweep / "results.json").read_text())
    shots = {}
    for pkg, r in results.items():
        png = r.get("png")
        if png and pathlib.Path(png).exists():
            shots[pkg] = pathlib.Path(png)
        else:
            cand = args.sweep / f"{pkg}.png"
            if cand.exists():
                shots[pkg] = cand

    print(f"packages in results.json: {len(results)}   with a PNG: {len(shots)}")
    cluster_map, clusters, unreadable = classify(shots)

    # Ground truth first. `frame_seq` counts frames the compositor
    # actually took from the client, so it settles what a screenshot can
    # only suggest: an app that legitimately starts empty renders a
    # near-blank tile that looks like the placeholder, and two such apps
    # look like each other. Clustering is kept only as a cross-check.
    real_map: dict[str, bool] = {}
    painted_errors: dict[str, str] = {}
    unverified: set[str] = set()
    for pkg in shots:
        frames = (results.get(pkg) or {}).get("frames")
        if isinstance(frames, list) and frames:
            real_map[pkg] = any((f or {}).get("frame_seq", 0) > 0 for f in frames)
            for f in frames:
                if (f or {}).get("last_error"):
                    painted_errors[pkg] = str(f["last_error"])
        else:
            # No answer from the compositor - fall back to the picture
            # and say the number is weaker for this row.
            real_map[pkg] = cluster_map.get(pkg, False)
            unverified.add(pkg)

    disagreements = [
        pkg
        for pkg in shots
        if pkg not in unverified
        and pkg not in unreadable
        and cluster_map.get(pkg, False) != real_map.get(pkg, False)
    ]

    # Every package gets a cell, including the ones with no shot at all —
    # a sheet that quietly drops failures would overstate the fleet.
    cells = sorted(results)
    cols = args.cols
    rows = (len(cells) + cols - 1) // cols
    sheet_w = cols * (CELL_W + PAD) + PAD
    header_h = 64
    sheet_h = header_h + rows * (CELL_H + LABEL_H + PAD) + PAD
    sheet = Image.new("RGB", (sheet_w, sheet_h), BG)
    draw = ImageDraw.Draw(sheet)

    f_label = font(16)
    f_small = font(12)
    f_title = font(24)

    n_real = sum(1 for p in cells if real_map.get(p))
    n_placeholder = sum(
        1 for p in cells if p in shots and p not in unreadable and not real_map.get(p)
    )
    n_unreadable = len(unreadable)
    n_noshot = len(cells) - len(shots)
    n_unverified = len(unverified)

    draw.text((PAD + 4, 14), "Tilo applications - launched through the compositor",
              font=f_title, fill=FG)
    draw.text((PAD + 4, 44),
              f"{len(cells)} installed · {n_real} rendered real content · "
              f"{n_placeholder} still on the first-frame placeholder · "
              f"{n_noshot} never adopted a window"
              + (f" · {n_unreadable} unreadable capture" if n_unreadable else "")
              + (f" · {n_unverified} unverified" if n_unverified else ""),
              font=f_small, fill=DIM)

    for i, pkg in enumerate(cells):
        cx = PAD + (i % cols) * (CELL_W + PAD)
        cy = header_h + (i // cols) * (CELL_H + LABEL_H + PAD)
        path = shots.get(pkg)
        if path:
            try:
                im = Image.open(path).convert("RGB")
                im.thumbnail((CELL_W, CELL_H), LANCZOS)
                off_x = cx + (CELL_W - im.width) // 2
                off_y = cy + (CELL_H - im.height) // 2
                sheet.paste(im, (off_x, off_y))
            except Exception:  # noqa: BLE001
                path = None
        if not path:
            draw.rectangle([cx, cy, cx + CELL_W, cy + CELL_H], fill=(30, 32, 40))
            draw.text((cx + 12, cy + CELL_H // 2 - 8), "no window adopted",
                      font=f_small, fill=DIM)

        real = real_map.get(pkg, False)
        name = short(pkg)
        draw.text((cx + 4, cy + CELL_H + 5), name, font=f_label, fill=FG)
        if pkg in unreadable:
            tag, colour = "unreadable", (210, 170, 90)
        elif path:
            if real:
                tag = "live" if pkg not in unverified else "live?"
                colour = (120, 200, 140)
            else:
                tag = "placeholder" if pkg not in unverified else "placeholder?"
                colour = PLACEHOLDER_TINT
        else:
            tag, colour = "no window", DIM
        tw = draw.textlength(tag, font=f_small)
        draw.text((cx + CELL_W - tw - 4, cy + CELL_H + 8), tag, font=f_small, fill=colour)
        draw.rectangle([cx, cy, cx + CELL_W, cy + CELL_H + LABEL_H],
                       outline=(48, 52, 64))

    args.out.parent.mkdir(parents=True, exist_ok=True)
    sheet.save(args.out)

    print(f"\nreal content : {n_real}")
    print(f"placeholder  : {n_placeholder}")
    print(f"no window    : {n_noshot}")
    if n_unreadable:
        print(f"unreadable   : {n_unreadable}")
    if n_unverified:
        print(f"unverified   : {n_unverified} (no frame counters; picture only)")
    if painted_errors:
        print("\ncapture errors the compositor reported:")
        for pkg, err in sorted(painted_errors.items()):
            print(f"  {short(pkg)}: {err}")
    if disagreements:
        print("\nclustering disagreed with the frame counters for:")
        for pkg in disagreements:
            print(f"  {short(pkg)} (counters win)")
    print("\nnear-identical clusters (>=2 members are placeholders):")
    for g in clusters:
        if len(g) >= 2:
            print(f"  [{len(g)}] " + ", ".join(short(x) for x in g))
    print(f"\nwrote {args.out}  ({sheet_w}x{sheet_h})")
    return 0


sys.exit(main())

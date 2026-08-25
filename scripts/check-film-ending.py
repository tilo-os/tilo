#!/usr/bin/env python3
"""Read the end of a demo film and say what is actually on the document page.

    python3 scripts/check-film-ending.py <film.mp4>

The film's last beat places one square photograph into a shared document. The
rig that drives the film has reported that beat as passing by reading the
agent's log and the document's CRDT, and has been wrong three times running: a
picture placed twice, a picture left landscape because the crop ran with a null
argument, an editor pane that read "No photograph open" throughout. Every one
of those passed the existing checks, because none of the existing checks looked
at the screen. This one looks at the screen and at nothing else.

It reports, for the final frames:

  * how many distinct placed pictures are on the page  (expected: exactly 1)
  * the aspect ratio of each                           (expected: square +/-5%)

and exits non-zero, with one line saying why, when either expectation fails.

HOW IT MEASURES, AND WHAT IT ASSUMES
------------------------------------
Three steps, each a plain measurement rather than any kind of recognition.

1. *The page.* The document page is the largest near-white region in the frame:
   bright (every channel above `PAGE_MIN_LEVEL`) and unsaturated (channel
   spread below `PAGE_MAX_SPREAD`). The frame is boxed down by `PAGE_SCALE`,
   the biggest connected bright blob is taken, and its edges are then walked
   back out at full resolution. A picture sitting in the page does not split
   the blob, because the page's margins run around it.
     Assumes: a dark desktop, one bright page, and the page being the largest
     bright thing in frame. Fails loudly (exit 2) if no such region exists.

2. *Ink.* Inside the page, the background is the most common near-white colour
   there; a pixel is "ink" if any channel differs from it by more than
   `INK_DELTA`. Measured on the ground-truth film: photograph interior 0.998
   ink, a line of body text 0.096, empty page 0.018.

3. *Pictures.* Ink alone does not separate a picture from a paragraph, so the
   page is cut into `CELL`x`CELL` blocks and a block counts as *solid* only if
   at least `SOLID_FRACTION` of its pixels are ink. Glyph strokes are thin and
   leave their blocks unsolid; a photograph fills its blocks completely. What
   survives is a mask in which a photograph is one dense rectangle and a
   paragraph is a scattering of nothing. Connected components of that mask are
   kept when they are at least `MIN_SIDE` px on a side, at least
   `MIN_PAGE_AREA` of the page, at least `MIN_BBOX_FILL` full inside their own
   bounding box, and textured (luminance stddev over `MIN_TEXTURE`, so a flat
   filled table cell is not mistaken for a photograph). Each survivor's box is
   then re-measured at full resolution, which is what the reported ratios are
   computed from.
     Assumes: a placed picture is a filled rectangle of mostly-non-white pixels
     with some tonal variation, and stacked pictures are separated by at least
     `CELL` px of page. A picture that is itself near-white throughout (snow,
     a blank scan) would be missed - it would be reported as no picture, which
     is a failure, not a pass.

A picture whose box touches the page edge is marked `clipped`: the page is
scrolled, or too short to hold what was placed in it, so its measured height is
only a lower bound. That is itself something wrong with the frame, so a clipped
picture is not excused from the square test.

The last frame is measured, plus two frames shortly before it, and the count
they agree on wins - one frame can catch a caret blink or a half-drawn paste.
Every number is printed, so a human can see whether the measurement itself is
sane before believing the verdict.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tempfile
from collections import deque

try:
    from PIL import Image, ImageChops, ImageStat
except ImportError:  # pragma: no cover - environment problem, not a film problem
    sys.stderr.write("check-film-ending: needs Pillow\n")
    raise SystemExit(2)


# --- what "page", "ink" and "picture" mean, in numbers ----------------------

PAGE_SCALE = 4  # frame is boxed down by this before hunting for the page
PAGE_MIN_LEVEL = 190  # every channel above this counts as bright
PAGE_MAX_SPREAD = 40  # ...and this unsaturated
PAGE_EDGE_FRACTION = 0.5  # a page edge is the outermost >=50%-bright line

INK_DELTA = 20  # channel distance from the page background

CELL = 4  # page is diced into CELLxCELL blocks
SOLID_FRACTION = 0.70  # ...and a block is solid at this much ink
MIN_SIDE = 40  # a picture is at least this many px on a side
MIN_PAGE_AREA = 0.005  # ...and this much of the page
MIN_BBOX_FILL = 0.60  # ...and this full inside its own bounding box
MIN_TEXTURE = 4.0  # ...and has at least this much luminance stddev
EDGE_FRACTION = 0.70  # a picture edge is the outermost >=70%-ink line

SQUARE_TOLERANCE = 0.05  # 5%: 0.95 .. 1.05 counts as square
CLIP_TOLERANCE = 3  # px from the page edge that counts as clipped

EXPECTED_PICTURES = 1

EXIT_OK = 0
EXIT_WRONG = 1  # the screen is not what the film's ending claims
EXIT_UNMEASURABLE = 2  # could not measure - no film, no page, no ffmpeg


# --- pixel plumbing ---------------------------------------------------------


def bright_mask(img):
    """0/255 mask of bright, unsaturated pixels - i.e. page, not desktop."""
    r, g, b = img.split()
    low = ImageChops.darker(ImageChops.darker(r, g), b)
    high = ImageChops.lighter(ImageChops.lighter(r, g), b)
    spread = ImageChops.difference(high, low)
    return ImageChops.multiply(
        low.point(lambda v: 255 if v > PAGE_MIN_LEVEL else 0),
        spread.point(lambda v: 255 if v < PAGE_MAX_SPREAD else 0),
    )


def ink_mask(img, bg):
    """0/255 mask of pixels that differ from the page background."""
    d = ImageChops.difference(img, Image.new("RGB", img.size, bg))
    r, g, b = d.split()
    worst = ImageChops.lighter(ImageChops.lighter(r, g), b)
    return worst.point(lambda v: 255 if v > INK_DELTA else 0)


def flat(img):
    """Pixels of a single-band image as one flat sequence."""
    reader = getattr(img, "get_flattened_data", None) or img.getdata
    return reader()


def col_means(mask, box):
    """Per-column mean of a 0/255 mask over `box`, computed in C."""
    x0, y0, x1, y1 = box
    if x1 <= x0 or y1 <= y0:
        return []
    strip = mask.crop(box).resize((x1 - x0, 1), Image.BOX)
    return [v / 255.0 for v in flat(strip)]


def row_means(mask, box):
    """Per-row mean of a 0/255 mask over `box`, computed in C."""
    x0, y0, x1, y1 = box
    if x1 <= x0 or y1 <= y0:
        return []
    strip = mask.crop(box).resize((1, y1 - y0), Image.BOX)
    return [v / 255.0 for v in flat(strip)]


def components(bits, w, h):
    """4-connected components of a 0/1 grid; yields (count, x0, y0, x1, y1)."""
    seen = bytearray(w * h)
    for start in range(w * h):
        if not bits[start] or seen[start]:
            continue
        seen[start] = 1
        queue = deque([start])
        n = 0
        x0 = x1 = start % w
        y0 = y1 = start // w
        while queue:
            i = queue.popleft()
            n += 1
            x, y = i % w, i // w
            if x < x0:
                x0 = x
            if x > x1:
                x1 = x
            if y < y0:
                y0 = y
            if y > y1:
                y1 = y
            if x > 0 and bits[i - 1] and not seen[i - 1]:
                seen[i - 1] = 1
                queue.append(i - 1)
            if x + 1 < w and bits[i + 1] and not seen[i + 1]:
                seen[i + 1] = 1
                queue.append(i + 1)
            if y > 0 and bits[i - w] and not seen[i - w]:
                seen[i - w] = 1
                queue.append(i - w)
            if y + 1 < h and bits[i + w] and not seen[i + w]:
                seen[i + w] = 1
                queue.append(i + w)
        yield n, x0, y0, x1, y1


# --- the three measurements -------------------------------------------------


def find_page(img):
    """Bounding box of the document page, or None."""
    w, h = img.size
    sw, sh = w // PAGE_SCALE, h // PAGE_SCALE
    small = bright_mask(img.resize((sw, sh), Image.BOX))
    bits = bytearray(1 if v else 0 for v in flat(small))
    best = max(components(bits, sw, sh), key=lambda c: c[0], default=None)
    if best is None or best[0] * PAGE_SCALE * PAGE_SCALE < 0.01 * w * h:
        return None

    _, cx0, cy0, cx1, cy1 = best
    # Walk each edge back out at full resolution: coming in from just outside
    # the coarse box, the page starts at the first line that is at least half
    # bright. A picture inside the page never takes a line below half, because
    # the margins to either side of it stay paper.
    full = bright_mask(img)
    reach = PAGE_SCALE * 3
    x0 = max(0, cx0 * PAGE_SCALE - reach)
    y0 = max(0, cy0 * PAGE_SCALE - reach)
    x1 = min(w, (cx1 + 1) * PAGE_SCALE + reach)
    y1 = min(h, (cy1 + 1) * PAGE_SCALE + reach)

    cols = col_means(full, (x0, y0, x1, y1))
    rows = row_means(full, (x0, y0, x1, y1))
    inside_c = [i for i, v in enumerate(cols) if v >= PAGE_EDGE_FRACTION]
    inside_r = [i for i, v in enumerate(rows) if v >= PAGE_EDGE_FRACTION]
    if not inside_c or not inside_r:
        return None
    return (
        x0 + inside_c[0],
        y0 + inside_r[0],
        x0 + inside_c[-1] + 1,
        y0 + inside_r[-1] + 1,
    )


def page_background(page):
    """The page's own paper colour: its most common near-white pixel value."""
    w, h = page.size
    sample = page.resize((max(1, w // 3), max(1, h // 3)), Image.NEAREST)
    colours = sample.getcolors(1 << 20) or []
    colours.sort(reverse=True)
    for _, c in colours:
        if min(c) > 180:
            return c
    return None


def run_around(values, seed, threshold):
    """The contiguous run of >=threshold values that contains `seed`.

    A run, not the outermost hits in the window: two pictures stacked with a
    thin gap of page between them would otherwise each measure as one block
    reaching into the other, because the window is deliberately opened wider
    than the coarse box in order to find the true edge.
    """
    if not values:
        return None
    seed = max(0, min(seed, len(values) - 1))
    if values[seed] < threshold:
        hits = [i for i, v in enumerate(values) if v >= threshold]
        if not hits:
            return None
        seed = min(hits, key=lambda i: abs(i - seed))
    lo = hi = seed
    while lo > 0 and values[lo - 1] >= threshold:
        lo -= 1
    while hi + 1 < len(values) and values[hi + 1] >= threshold:
        hi += 1
    return lo, hi


def refine(mask, box, limit):
    """Re-measure a coarse box at full resolution off the ink mask."""
    cx0, cy0, cx1, cy1 = box
    lx0, ly0, lx1, ly1 = limit
    wx0, wy0 = max(lx0, cx0 - 2 * CELL), max(ly0, cy0 - 2 * CELL)
    wx1, wy1 = min(lx1, cx1 + 2 * CELL), min(ly1, cy1 + 2 * CELL)

    rows = row_means(mask, (cx0, wy0, cx1, wy1))
    span = run_around(rows, (cy0 + cy1) // 2 - wy0, EDGE_FRACTION)
    if span is None:
        return None
    y0, y1 = wy0 + span[0], wy0 + span[1] + 1

    cols = col_means(mask, (wx0, y0, wx1, y1))
    span = run_around(cols, (cx0 + cx1) // 2 - wx0, EDGE_FRACTION)
    if span is None:
        return None
    return wx0 + span[0], y0, wx0 + span[1] + 1, y1


def find_pictures(img, page_box):
    """Every placed picture inside the page, measured in frame coordinates."""
    px0, py0, px1, py1 = page_box
    page = img.crop(page_box)
    pw, ph = page.size
    bg = page_background(page)
    if bg is None:
        return None, None

    ink = ink_mask(page, bg)
    gw, gh = pw // CELL, ph // CELL
    if gw < 4 or gh < 4:
        return [], bg
    grid = ink.resize((gw, gh), Image.BOX)
    level = SOLID_FRACTION * 255
    solid = bytearray(1 if v >= level else 0 for v in flat(grid))

    found = []
    for n, cx0, cy0, cx1, cy1 in components(solid, gw, gh):
        cells_w, cells_h = cx1 - cx0 + 1, cy1 - cy0 + 1
        if n / (cells_w * cells_h) < MIN_BBOX_FILL:
            continue
        box = refine(
            ink,
            (cx0 * CELL, cy0 * CELL, (cx1 + 1) * CELL, (cy1 + 1) * CELL),
            (0, 0, pw, ph),
        )
        if box is None:
            continue
        x0, y0, x1, y1 = box
        pic_w, pic_h = x1 - x0, y1 - y0
        if pic_w < MIN_SIDE or pic_h < MIN_SIDE:
            continue
        if pic_w * pic_h < MIN_PAGE_AREA * pw * ph:
            continue
        texture = ImageStat.Stat(page.crop(box).convert("L")).stddev[0]
        if texture < MIN_TEXTURE:
            continue
        found.append(
            {
                "box": (px0 + x0, py0 + y0, px0 + x1, py0 + y1),
                "w": pic_w,
                "h": pic_h,
                "ratio": pic_w / pic_h,
                "texture": texture,
                "clipped": (
                    x0 <= CLIP_TOLERANCE
                    or y0 <= CLIP_TOLERANCE
                    or x1 >= pw - CLIP_TOLERANCE
                    or y1 >= ph - CLIP_TOLERANCE
                ),
            }
        )
    found.sort(key=lambda p: (p["box"][1], p["box"][0]))
    return found, bg


def is_square(ratio):
    return max(ratio, 1.0 / ratio) - 1.0 <= SQUARE_TOLERANCE


def measure(path):
    """Measure one frame. Returns a dict, or None if there is no page in it."""
    img = Image.open(path).convert("RGB")
    page_box = find_page(img)
    if page_box is None:
        return None
    pictures, bg = find_pictures(img, page_box)
    if pictures is None:
        return None
    return {"size": img.size, "page": page_box, "bg": bg, "pictures": pictures}


# --- getting the frames out of the film -------------------------------------


def extract(film, into, at, tail):
    """Decode only the frames we need: the tail of the film, or around `at`."""
    pattern = os.path.join(into, "f_%04d.png")
    seek = ["-ss", "%.3f" % at] if at is not None else ["-sseof", "-%.3f" % tail]
    limit = ["-frames:v", "12"] if at is not None else []
    base = ["ffmpeg", "-v", "error", "-y", *seek, "-i", film, *limit]
    err = ""
    for pacing in (["-fps_mode", "passthrough"], ["-vsync", "0"]):
        done = subprocess.run(base + pacing + [pattern], capture_output=True, text=True)
        frames = sorted(
            os.path.join(into, f) for f in os.listdir(into) if f.endswith(".png")
        )
        if frames:
            return frames
        err = done.stderr
        if done.returncode == 0:
            break
    sys.stderr.write("check-film-ending: ffmpeg decoded no frames from %s\n" % film)
    if err:
        sys.stderr.write(err)
    return []


def frame_rate(film):
    """Frames per second, for labelling how far back a measured frame is."""
    try:
        out = subprocess.run(
            [
                "ffprobe", "-v", "error",
                "-select_streams", "v:0",
                "-show_entries", "stream=r_frame_rate",
                "-of", "default=noprint_wrappers=1:nokey=1",
                film,
            ],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
        num, _, den = out.partition("/")
        return float(num) / float(den or 1)
    except Exception:
        return 30.0


# --- reporting --------------------------------------------------------------


def describe(picture, index):
    x0, y0, x1, y1 = picture["box"]
    return (
        "  picture %d: %dx%d px at (%d,%d)-(%d,%d)  ratio %.3f  %s  texture %.1f%s"
        % (
            index,
            picture["w"], picture["h"],
            x0, y0, x1, y1,
            picture["ratio"],
            "SQUARE" if is_square(picture["ratio"]) else "NOT SQUARE",
            picture["texture"],
            "  CLIPPED by the page edge" if picture["clipped"] else "",
        )
    )


def faults(pictures, expected):
    """One phrase per thing the frame gets wrong; empty means the frame is right."""
    out = []
    if len(pictures) != expected:
        out.append(
            "%s on the page (expected %d)"
            % (
                "no picture" if not pictures else "%d pictures" % len(pictures),
                expected,
            )
        )
    for i, p in enumerate(pictures, 1):
        if not is_square(p["ratio"]):
            out.append(
                "picture %d is %dx%d, ratio %.2f, not square%s"
                % (
                    i, p["w"], p["h"], p["ratio"],
                    " (clipped by the page edge)" if p["clipped"] else "",
                )
            )
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(
        description="Check, from the pixels, what the film's last frame shows "
        "on the document page.",
    )
    ap.add_argument("film", help="the finished film (mp4)")
    ap.add_argument(
        "--at",
        type=float,
        default=None,
        metavar="SECONDS",
        help="measure here instead of at the end (e.g. to check an empty page)",
    )
    ap.add_argument(
        "--tail",
        type=float,
        default=0.5,
        metavar="SECONDS",
        help="how much of the film's tail to decode (default: 0.5)",
    )
    ap.add_argument(
        "--expect-pictures",
        type=int,
        default=EXPECTED_PICTURES,
        metavar="N",
        help="how many pictures the page should hold (default: 1)",
    )
    args = ap.parse_args(argv)

    if not os.path.exists(args.film):
        sys.stderr.write("check-film-ending: no such film: %s\n" % args.film)
        return EXIT_UNMEASURABLE

    fps = frame_rate(args.film)
    with tempfile.TemporaryDirectory(prefix="film-ending-") as tmp:
        frames = extract(args.film, tmp, args.at, args.tail)
        if not frames:
            return EXIT_UNMEASURABLE

        # The last frame, and two shortly before it, spaced far enough apart to
        # straddle a caret blink or a half-drawn paste.
        step = max(1, len(frames) // 3)
        offsets = sorted({0, step, 2 * step} & set(range(len(frames))))
        picked = sorted(len(frames) - 1 - i for i in offsets)

        print("film: %s" % args.film)
        print(
            "decoded %d frames %s, measuring %d of them (%.0f fps)"
            % (
                len(frames),
                "from %.2fs" % args.at
                if args.at is not None
                else "from the last %.2fs" % args.tail,
                len(picked),
                fps,
            )
        )

        results = []
        for i in picked:
            back = (len(frames) - 1 - i) / fps
            if args.at is not None:
                label = "%.2fs+%.2fs" % (args.at, i / fps)
            else:
                label = "last frame" if i == len(frames) - 1 else "-%.2fs" % back
            r = measure(frames[i])
            print()
            if r is None:
                print("%s: no document page found in this frame" % label)
                results.append((label, None))
                continue
            px0, py0, px1, py1 = r["page"]
            print(
                "%s: page %dx%d px at (%d,%d)-(%d,%d), paper rgb%s, %d picture(s)"
                % (
                    label,
                    px1 - px0, py1 - py0, px0, py0, px1, py1,
                    r["bg"], len(r["pictures"]),
                )
            )
            for n, p in enumerate(r["pictures"], 1):
                print(describe(p, n))
            results.append((label, r))

    measured = [(label, r) for label, r in results if r is not None]
    print()
    if not measured:
        print("UNMEASURABLE: no document page in any measured frame")
        return EXIT_UNMEASURABLE

    counts = [len(r["pictures"]) for _, r in measured]
    agreed = max(set(counts), key=counts.count)
    label, chosen = [(l, r) for l, r in measured if len(r["pictures"]) == agreed][-1]

    if len(set(counts)) > 1:
        print(
            "frames disagree on the count %s; going with %d, from %s"
            % (counts, agreed, label)
        )
    problems = faults(chosen["pictures"], args.expect_pictures)
    if problems:
        print("FAIL (%s): %s" % (label, "; ".join(problems)))
        return EXIT_WRONG
    print(
        "OK (%s): one picture, %dx%d, ratio %.3f - square within %d%%"
        % (
            label,
            chosen["pictures"][0]["w"],
            chosen["pictures"][0]["h"],
            chosen["pictures"][0]["ratio"],
            int(SQUARE_TOLERANCE * 100),
        )
    )
    return EXIT_OK


if __name__ == "__main__":
    raise SystemExit(main())

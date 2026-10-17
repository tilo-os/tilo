#!/usr/bin/env python3
"""Generate Tilo's dependency-free UKI splash as a 24-bit BMP."""

import math
import re
import struct
import sys

WIDTH, HEIGHT = 1024, 768
# systemd-stub clears the whole screen to black and centres this fixed-size
# tile on it, at whatever mode the firmware chose. Anything but black at the
# tile's edge shows as a box floating in the middle of a larger panel, so the
# background is black and the glow fades out before it reaches an edge.
BACKGROUND = (0, 0, 0)
GLOW_CENTER_Y = HEIGHT * 0.44
GLOW_RADIUS = min(WIDTH / 2, GLOW_CENTER_Y, HEIGHT - GLOW_CENTER_Y) - 8
MARK_CENTER = (WIDTH / 2, HEIGHT * 0.43)
MARK_SCALE = 1.4

# Tilo's mark, path for path as the brand's mark.svg draws it in its 100 x 100
# view box, in the colours of mark-dark.svg, the variant for dark grounds. It
# is rasterised here so the UKI build stays independent of SVG tools.
MARK_SIDES = (
    "M10.98 31.85 Q10.979892448752274 27.96648782251284 14.4 26.12 L20.26 22.97 "
    "Q24.843373493975903 20.50602409638554 24.84 25.71 L24.84 74.29 "
    "Q24.843373493975903 79.49397590361446 20.26 77.03 L14.4 73.88 "
    "Q10.979892448752274 72.03351217748715 10.98 68.15 Z",
    "M89.02 31.85 Q89.02010755124772 27.96648782251284 85.6 26.12 L79.74 22.97 "
    "Q75.1566265060241 20.50602409638554 75.16 25.71 L75.16 74.29 "
    "Q75.1566265060241 79.49397590361446 79.74 77.03 L85.6 73.88 "
    "Q89.02010755124772 72.03351217748715 89.02 68.15 Z",
)
# The central block: left, top, right, bottom and corner radius.
MARK_BLOCK = (29.0, 16.0, 71.0, 84.0, 6.0)
MARK_SIDE_COLOR = (0x3B, 0x43, 0x76)
MARK_BLOCK_COLOR = (0x6F, 0x82, 0xFF)


def path_polygon(data, steps=24):
    """Flatten path data made of absolute M, L, Q and Z commands."""
    tokens = re.findall(r"[MLQZ]|-?[0-9.]+", data)
    points, index, command = [], 0, None
    while index < len(tokens):
        if tokens[index].isalpha():
            command = tokens[index]
            index += 1
            continue
        count = 4 if command == "Q" else 2
        values = [float(value) for value in tokens[index:index + count]]
        index += count
        if command == "Q":
            (x0, y0), (cx, cy, x1, y1) = points[-1], values
            for step in range(1, steps + 1):
                t = step / steps
                u = 1 - t
                points.append((u * u * x0 + 2 * u * t * cx + t * t * x1,
                               u * u * y0 + 2 * u * t * cy + t * t * y1))
        else:
            points.append((values[0], values[1]))
    return points


def rounded_rect_polygon(left, top, right, bottom, radius, steps=12):
    points = []
    for cx, cy, start in ((right - radius, top + radius, -90),
                          (right - radius, bottom - radius, 0),
                          (left + radius, bottom - radius, 90),
                          (left + radius, top + radius, 180)):
        for step in range(steps + 1):
            angle = math.radians(start + 90 * step / steps)
            points.append((cx + radius * math.cos(angle), cy + radius * math.sin(angle)))
    return points


def coverage(polygon, center, scale, rows=8):
    """How much of each pixel `polygon` (in mark units, centred on `center`
    at `scale` pixels per unit) covers. Each pixel row is sampled at `rows`
    heights; along each, the filled span's exact overlap with a pixel counts."""
    points = [(center[0] + (x - 50) * scale, center[1] + (y - 50) * scale)
              for x, y in polygon]
    edges = list(zip(points, points[1:] + points[:1]))
    cover = {}
    top = int(min(y for _, y in points))
    bottom = int(max(y for _, y in points)) + 1
    for py in range(top, bottom):
        for row in range(rows):
            y = py + (row + 0.5) / rows
            xs = sorted(xa + (y - ya) * (xb - xa) / (yb - ya)
                        for (xa, ya), (xb, yb) in edges if (ya <= y) != (yb <= y))
            for left, right in zip(xs[::2], xs[1::2]):
                for px in range(int(left), int(right) + 1):
                    overlap = min(right, px + 1) - max(left, px)
                    if overlap > 0:
                        cover[px, py] = cover.get((px, py), 0.0) + overlap / rows
    return cover


def mark_layers(center, scale):
    """The mark's shapes as (coverage, colour) pairs; they never overlap."""
    layers = [(coverage(path_polygon(side), center, scale), MARK_SIDE_COLOR)
              for side in MARK_SIDES]
    layers.append((coverage(rounded_rect_polygon(*MARK_BLOCK), center, scale),
                   MARK_BLOCK_COLOR))
    return layers


MARK = mark_layers(MARK_CENTER, MARK_SCALE)


def mix(a, b, amount):
    return tuple(round(x + (y - x) * amount) for x, y in zip(a, b))


def pixel(x, y):
    glow = max(0.0, 1.0 - math.hypot(x - WIDTH / 2, y - GLOW_CENTER_Y) / GLOW_RADIUS)
    color = mix(BACKGROUND, (25, 25, 44), glow * glow * 0.72)

    cx, cy = MARK_CENTER
    # One calm baseline under the mark prevents the frame reading as an
    # application icon floating in empty space.
    if abs(y - (cy + 92)) < 1.2 and abs(x - cx) < 54:
        color = mix(color, (103, 108, 129), 0.65)
    for cover, accent in MARK:
        amount = cover.get((x, y))
        if amount:
            color = mix(color, accent, min(1.0, amount))
    return color


def write_bmp(path):
    stride = (WIDTH * 3 + 3) & ~3
    image_size = stride * HEIGHT
    with open(path, "wb") as output:
        output.write(b"BM")
        output.write(struct.pack("<IHHI", 54 + image_size, 0, 0, 54))
        output.write(struct.pack("<IiiHHIIiiII", 40, WIDTH, HEIGHT, 1, 24, 0, image_size, 2835, 2835, 0, 0))
        padding = b"\0" * (stride - WIDTH * 3)
        for y in reversed(range(HEIGHT)):
            row = bytearray()
            for x in range(WIDTH):
                red, green, blue = pixel(x, y)
                row.extend((blue, green, red))
            output.write(row)
            output.write(padding)


if __name__ == "__main__":
    write_bmp(sys.argv[1])

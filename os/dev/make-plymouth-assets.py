#!/usr/bin/env python3
"""Generate dependency-free PNG assets for the Tilo Plymouth theme."""

import math
import os
import re
import struct
import subprocess
import sys
import zlib

from PIL import Image as PillowImage
from PIL import ImageDraw, ImageFont


def png(path, width, height, pixel):
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for x in range(width):
            rows.extend(pixel(x, y))

    def chunk(kind, data):
        checksum = zlib.crc32(kind + data)
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", checksum)

    payload = b"\x89PNG\r\n\x1a\n"
    payload += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
    payload += chunk(b"IDAT", zlib.compress(bytes(rows), 9))
    payload += chunk(b"IEND", b"")
    with open(path, "wb") as output:
        output.write(payload)


def smoothstep(edge0, edge1, value):
    amount = max(0.0, min(1.0, (value - edge0) / (edge1 - edge0)))
    return amount * amount * (3.0 - 2.0 * amount)


def blend(first, second, amount):
    return tuple(round(x + (y - x) * amount) for x, y in zip(first, second))


# Tilo's mark, path for path as the brand's mark.svg draws it in its 100 x 100
# view box, in the colours of mark-dark.svg, the variant for dark grounds. It
# is rasterised here so the theme needs no SVG tools.
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


def logo_pixel(size):
    layers = mark_layers((size / 2, size / 2), size / 100)

    def pixel(x, y):
        for cover, color in layers:
            amount = cover.get((x, y))
            if amount:
                return (*color, round(min(1.0, amount) * 255))
        return (0, 0, 0, 0)
    return pixel


def rounded_rect(width, height, radius, fill, stroke=None, stroke_width=1):
    def pixel(x, y):
        dx = max(radius - x - 0.5, 0, x + 0.5 - (width - radius))
        dy = max(radius - y - 0.5, 0, y + 0.5 - (height - radius))
        distance = math.hypot(dx, dy)
        outer = 1.0 - smoothstep(radius - 1, radius + 1, distance)
        if stroke is None:
            return (*fill, round(outer * 255))
        inner = 1.0 - smoothstep(radius - stroke_width - 1, radius - stroke_width + 1, distance)
        return (*blend(stroke, fill, inner), round(outer * 255))
    return pixel


def lock_pixel(x, y):
    center = 13.0
    shackle_distance = abs(math.hypot(x + 0.5 - center, y + 0.5 - 11.0) - 7.0)
    shackle = 1.0 - smoothstep(1.5, 2.5, shackle_distance) if y < 13 else 0.0
    body = 1.0 if 4 <= x < 22 and 11 <= y < 25 else 0.0
    return (159, 166, 224, round(max(shackle, body) * 255))


def circle_pixel(size, color):
    center = size / 2

    def pixel(x, y):
        distance = math.hypot(x + 0.5 - center, y + 0.5 - center)
        alpha = 1.0 - smoothstep(center - 1.5, center - 0.25, distance)
        return (*color, round(alpha * 255))
    return pixel


def progress_pixel(width, first, second):
    def pixel(x, y):
        color = blend(first, second, x / max(1, width - 1))
        alpha = round(255 * (1.0 - smoothstep(1.3, 2.1, abs(y + 0.5 - 2))))
        return (*color, alpha)
    return pixel


def text_png(path, text, size, color, weight="Regular"):
    """Rasterize text now so the tiny initramfs never needs fonts or Pango."""
    # The index also names the instance of a variable font (FreeType reads
    # its high 16 bits), so a variable Noto Sans is drawn at `weight` rather
    # than at its default instance.
    font_path, _, index = subprocess.run(
        ["fc-match", "-f", "%{file}\n%{index}", f"Noto Sans:style={weight}"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.partition("\n")
    if not font_path:
        raise RuntimeError("fontconfig could not resolve Noto Sans")
    font = ImageFont.truetype(font_path, size, index=int(index or 0))
    probe = PillowImage.new("RGBA", (1, 1))
    bounds = ImageDraw.Draw(probe).textbbox((0, 0), text, font=font)
    width = bounds[2] - bounds[0] + 4
    height = bounds[3] - bounds[1] + 4
    image = PillowImage.new("RGBA", (width, height), (0, 0, 0, 0))
    ImageDraw.Draw(image).text(
        (2 - bounds[0], 2 - bounds[1]), text, font=font, fill=(*color, 255)
    )
    image.save(path, format="PNG", optimize=True)


def main(directory):
    os.makedirs(directory, exist_ok=True)
    png(os.path.join(directory, "tilo-logo.png"), 160, 160, logo_pixel(160))
    png(os.path.join(directory, "entry.png"), 360, 56,
        rounded_rect(360, 56, 14, (31, 34, 51), (59, 67, 118), 2))
    png(os.path.join(directory, "lock.png"), 26, 26, lock_pixel)
    png(os.path.join(directory, "bullet.png"), 8, 8, circle_pixel(8, (226, 224, 255)))
    text_png(os.path.join(directory, "unlock-heading.png"), "Unlock Tilo", 30,
             (240, 240, 250), "SemiBold")
    text_png(os.path.join(directory, "unlock-help.png"),
             "Enter your disk passphrase or recovery key", 18, (158, 163, 186))
    text_png(os.path.join(directory, "unlock-retry.png"),
             "Unlock unsuccessful. Please try again.", 18, (244, 169, 154))
    text_png(os.path.join(directory, "starting-tilo.png"), "Starting Tilo", 26,
             (240, 240, 250), "SemiBold")
    text_png(os.path.join(directory, "shutting-down.png"), "Shutting down", 26,
             (240, 240, 250), "SemiBold")
    text_png(os.path.join(directory, "startup-help.png"),
             "Press Esc to view startup messages", 16, (158, 163, 186))
    png(os.path.join(directory, "progress-background.png"), 280, 4,
        progress_pixel(280, (54, 57, 75), (54, 57, 75)))
    png(os.path.join(directory, "progress-fill.png"), 280, 4,
        progress_pixel(280, (59, 67, 118), (111, 130, 255)))


if __name__ == "__main__":
    main(sys.argv[1])

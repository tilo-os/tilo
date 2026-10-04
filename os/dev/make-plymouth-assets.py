#!/usr/bin/env python3
"""Generate dependency-free PNG assets for the Tilo Plymouth theme."""

import math
import os
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


def logo_pixel(x, y):
    alpha = 0.0
    color = (130, 96, 255)
    for index, center_x in enumerate((56.0, 110.0, 164.0)):
        distance = math.hypot(x + 0.5 - center_x, y + 0.5 - 110.0)
        dot = 1.0 - smoothstep(15.0, 18.0, distance)
        if dot > alpha:
            alpha = dot
            color = blend((130, 96, 255), (49, 198, 181), index / 2)
    return (*color, round(alpha * 255))


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
    return (178, 174, 255, round(max(shackle, body) * 255))


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
    png(os.path.join(directory, "tilo-logo.png"), 220, 220, logo_pixel)
    png(os.path.join(directory, "entry.png"), 360, 56,
        rounded_rect(360, 56, 14, (31, 34, 51), (91, 79, 156), 2))
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
        progress_pixel(280, (130, 96, 255), (49, 198, 181)))


if __name__ == "__main__":
    main(sys.argv[1])

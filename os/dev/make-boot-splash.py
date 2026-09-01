#!/usr/bin/env python3
"""Generate qOS's dependency-free UKI splash as a 24-bit BMP."""

import math
import struct
import sys

WIDTH, HEIGHT = 1024, 768


def mix(a, b, amount):
    return tuple(round(x + (y - x) * amount) for x, y in zip(a, b))


def coverage(distance, half_width=3.0):
    return max(0.0, min(1.0, 0.5 - distance / (half_width * 2.0)))


def pixel(x, y):
    background = (14, 16, 23)
    glow = max(0.0, 1.0 - math.hypot(x - WIDTH / 2, y - HEIGHT * 0.44) / 430.0)
    color = mix(background, (25, 25, 44), glow * glow * 0.72)

    cx, cy, radius = WIDTH / 2, HEIGHT * 0.43, 72.0
    angle = (math.atan2(y - cy, x - cx) + math.pi) / (2.0 * math.pi)
    accent = mix((124, 92, 255), (49, 198, 181), angle)
    ring_distance = abs(math.hypot(x - cx, y - cy) - radius) - 8.0
    color = mix(color, accent, coverage(ring_distance, 1.7))

    # A short diagonal turns the ring into a quiet, recognisable q without
    # relying on a font in the initramfs/UKI build environment.
    ax, ay, bx, by = cx + 43, cy + 47, cx + 86, cy + 91
    dx, dy = bx - ax, by - ay
    t = max(0.0, min(1.0, ((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)))
    tail_distance = math.hypot(x - (ax + t * dx), y - (ay + t * dy)) - 8.0
    color = mix(color, (49, 198, 181), coverage(tail_distance, 1.7))

    # One calm baseline under the mark prevents the frame reading as an
    # application icon floating in empty space.
    if abs(y - (cy + 142)) < 1.2 and abs(x - cx) < 54:
        color = mix(color, (103, 108, 129), 0.65)
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

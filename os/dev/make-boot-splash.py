#!/usr/bin/env python3
"""Generate Tilo's dependency-free UKI splash as a 24-bit BMP."""

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

    cx, cy = WIDTH / 2, HEIGHT * 0.43
    # Tilo's three-dot mark matches the public site favicon and avoids carrying
    # the retired qOS “q” into the first frame of the renamed product. It is
    # geometric so the UKI build remains independent of fonts and SVG tools.
    for index, dot_x in enumerate((cx - 54, cx, cx + 54)):
        distance = math.hypot(x - dot_x, y - cy) - 16.0
        accent = mix((124, 92, 255), (49, 198, 181), index / 2)
        color = mix(color, accent, coverage(distance, 1.7))

    # One calm baseline under the mark prevents the frame reading as an
    # application icon floating in empty space.
    if abs(y - (cy + 92)) < 1.2 and abs(x - cx) < 54:
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

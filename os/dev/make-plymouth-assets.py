#!/usr/bin/env python3
"""Generate dependency-free PNG assets for the Tilo Plymouth theme."""

import ctypes
import math
import os
import re
import struct
import subprocess
import sys
import tempfile
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


# The login screen's face (LOOK's `family`, the variable file the greeter's
# font catalog installs), set by main() before anything is drawn.
FACE = None
CUT_DIR = tempfile.TemporaryDirectory(prefix="tilo-plymouth-faces-")


def find_face(family):
    """The upright file of `family`, refusing the substitute fontconfig
    offers for a family it lacks."""
    found = subprocess.run(
        ["fc-match", "-f", "%{family}\n%{file}", f"{family}:slant=0"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    families, _, path = found.partition("\n")
    if family not in families.split(",") or not path:
        raise RuntimeError(f"fontconfig has no {family} (it offers {families or 'nothing'})")
    return path


CUTS = {}


def face(size, weight=400):
    """FACE at `size` and `weight`. It is drawn from a static cut of that
    weight: FreeType's advances for a variable instance leave gaps between
    letters."""
    if weight not in CUTS:
        CUTS[weight] = os.path.join(CUT_DIR.name, f"{weight}.ttf")
        _HarfBuzz().instance(FACE, weight, CUTS[weight])
    return ImageFont.truetype(CUTS[weight], size)


class _HarfBuzz:
    """The few libharfbuzz-subset calls that cut a variable face to one
    static instance: the instancer the font catalog's own HarfBuzz carries,
    so the theme needs no font tooling beyond the image's."""

    def __init__(self):
        P = ctypes.c_void_p
        hb = ctypes.CDLL("libharfbuzz.so.0")
        subset = ctypes.CDLL("libharfbuzz-subset.so.0")
        for lib, name, restype, argtypes in (
            (hb, "hb_blob_create_from_file_or_fail", P, [ctypes.c_char_p]),
            (hb, "hb_face_create", P, [P, ctypes.c_uint]),
            (hb, "hb_face_reference_blob", P, [P]),
            (hb, "hb_blob_get_data", ctypes.POINTER(ctypes.c_char),
             [P, ctypes.POINTER(ctypes.c_uint)]),
            (hb, "hb_set_add_range", None, [P, ctypes.c_uint32, ctypes.c_uint32]),
            (subset, "hb_subset_input_create_or_fail", P, []),
            (subset, "hb_subset_input_unicode_set", P, [P]),
            (subset, "hb_subset_input_pin_all_axes_to_default", ctypes.c_int, [P, P]),
            (subset, "hb_subset_input_pin_axis_location", ctypes.c_int,
             [P, P, ctypes.c_uint32, ctypes.c_float]),
            (subset, "hb_subset_or_fail", P, [P, P]),
        ):
            function = getattr(lib, name)
            function.restype = restype
            function.argtypes = argtypes
            setattr(self, name.removeprefix("hb_"), function)

    def face(self, path):
        blob = self.blob_create_from_file_or_fail(path.encode())
        if not blob:
            raise RuntimeError(f"HarfBuzz could not read {path}")
        return self.face_create(blob, 0)

    def instance(self, source, weight, destination):
        """`source` at `weight` with every other axis at its default, every
        character kept, written to `destination`."""
        source_face = self.face(source)
        plan = self.subset_input_create_or_fail()
        self.set_add_range(self.subset_input_unicode_set(plan), 0, 0x10FFFF)
        # A static face has no axes to pin; the cut then only copies it.
        self.subset_input_pin_all_axes_to_default(plan, source_face)
        self.subset_input_pin_axis_location(plan, source_face,
                                            int.from_bytes(b"wght", "big"), weight)
        cut = self.subset_or_fail(source_face, plan)
        if not cut:
            raise RuntimeError(f"HarfBuzz could not cut {source} at weight {weight}")
        length = ctypes.c_uint()
        data = self.blob_get_data(self.face_reference_blob(cut), ctypes.byref(length))
        with open(destination, "wb") as out:
            out.write(ctypes.string_at(data, length.value))


def text_png(path, text, size, color, weight=400):
    """Rasterize text now so the tiny initramfs never needs fonts or Pango."""
    font = face(size, weight)
    probe = PillowImage.new("RGBA", (1, 1))
    bounds = ImageDraw.Draw(probe).textbbox((0, 0), text, font=font)
    width = bounds[2] - bounds[0] + 4
    height = bounds[3] - bounds[1] + 4
    image = PillowImage.new("RGBA", (width, height), (0, 0, 0, 0))
    ImageDraw.Draw(image).text(
        (2 - bounds[0], 2 - bounds[1]), text, font=font, fill=(*color, 255)
    )
    image.save(path, format="PNG", optimize=True)


def hint_png(path, parts, look):
    """A key hint: text with each named key drawn as a keycap (HIG §4), in
    the login screen's body text, dimmed, its keycaps in the field's colour."""
    color = look["dim"]
    font = face(look["body_size"], look["body_weight"])
    key_font = face(look["label_size"], look["label_weight"])
    keycap_fill = look["field"]
    keycap_edge = blend(look["field"], look["dim"], 0.3)
    probe = ImageDraw.Draw(PillowImage.new("RGBA", (1, 1)))
    ascent, descent = font.getmetrics()
    cap_height = ascent + descent + 8
    pieces = []
    for kind, text in parts:
        if kind == "key":
            bounds = probe.textbbox((0, 0), text, font=key_font)
            pieces.append((kind, text, bounds[2] - bounds[0] + 18))
        else:
            pieces.append((kind, text, probe.textlength(text, font=font)))
    width = round(sum(piece[2] for piece in pieces)) + 4
    height = cap_height + 4
    image = PillowImage.new("RGBA", (width, height), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    x = 2.0
    for kind, text, advance in pieces:
        if kind == "key":
            draw.rounded_rectangle((x, 2, x + advance - 1, 2 + cap_height - 1),
                                   radius=look["field_radius"] - 2,
                                   fill=(*keycap_fill, 255), outline=(*keycap_edge, 255),
                                   width=1)
            draw.text((x + advance / 2, 2 + cap_height / 2), text, font=key_font,
                      fill=(*color, 255), anchor="mm")
        else:
            draw.text((x, 2 + cap_height / 2), text, font=font, fill=(*color, 255),
                      anchor="lm")
        x += advance
    image.save(path, format="PNG", optimize=True)


def main(directory, face_path=None):
    """Everything the theme loads, into `directory`. The face is LOOK's
    family as fontconfig finds it, or `face_path` for a check of the theme's
    structure where the catalog's fonts are not installed."""
    global FACE
    os.makedirs(directory, exist_ok=True)
    look = LOOK
    FACE = face_path or find_face(look["family"])
    png(os.path.join(directory, "tilo-logo.png"), 160, 160, logo_pixel(160))
    # The disk's own prompt (tilo.script, "dialog") in the login screen's
    # look: its field, the dots typed into it, its refusal as a note, and the
    # way back to signing in.
    text_png(os.path.join(directory, "unlock-heading.png"), "Unlock Tilo", look["name_size"],
             look["text"], look["name_weight"])
    text_png(os.path.join(directory, "unlock-help.png"),
             "Enter your disk passphrase or recovery key", look["body_size"], look["dim"],
             look["body_weight"])
    # tilo-boot-signin unlock-prompt names what this volume's prompt takes.
    for kind, text in UNLOCK_ASKS.items():
        text_png(os.path.join(directory, f"unlock-help-{kind}.png"), text, look["body_size"],
                 look["dim"], look["body_weight"])
    note_png(os.path.join(directory, "unlock-retry.png"),
             "Unlock unsuccessful. Please try again.", look, look["danger"])
    width, height, radius = 360, look["field_height"], look["field_radius"]
    png(os.path.join(directory, "field.png"), width, height,
        rounded_rect(width, height, radius, look["field"]))
    png(os.path.join(directory, "field-bullet.png"), 7, 7, circle_pixel(7, look["text"]))
    hint_png(os.path.join(directory, "hint-go-back.png"),
             [("text", "To go back to signing in, press "), ("key", "Enter"),
              ("text", " with the field empty.")], look)
    text_png(os.path.join(directory, "starting-tilo.png"), "Starting Tilo", 26,
             (240, 240, 250), 600)
    text_png(os.path.join(directory, "shutting-down.png"), "Shutting down", 26,
             (240, 240, 250), 600)
    text_png(os.path.join(directory, "startup-help.png"),
             "Press Esc to view startup messages", 16, (158, 163, 186))
    png(os.path.join(directory, "progress-background.png"), 280, 4,
        progress_pixel(280, (54, 57, 75), (54, 57, 75)))
    png(os.path.join(directory, "progress-fill.png"), 280, 4,
        progress_pixel(280, (59, 67, 118), (111, 130, 255)))


# The login screen's colours and sizes the disk's prompt is drawn in: the
# greeter's dark theme (Theme::dark) for its text, field and danger colours,
# and the Body, Label and LargeTitle text roles.
LOOK = {
    "family": "Tilo Sans",
    "text": (0xEF, 0xF2, 0xF6),
    "dim": (0xAF, 0xB1, 0xB5),
    "danger": (0xE4, 0x7C, 0x75),
    "field": (0x1C, 0x1E, 0x21),
    "field_radius": 8,
    "field_height": 32,
    "name_size": 26,
    "name_weight": 600,
    "body_size": 14,
    "body_weight": 400,
    "label_size": 13,
    "label_weight": 500,
}
UNLOCK_ASKS = {
    "password-recovery": "Enter your password or recovery key",
    "passphrase-recovery": "Enter your disk passphrase or recovery key",
    "password": "Enter your password",
    "passphrase": "Enter your disk passphrase",
    "recovery": "Enter your recovery key",
}


def alert_pixel(size, color):
    """The circle-alert icon the login screen puts before an error."""
    center = size / 2
    ring = size / 2 - 1.25
    stroke = max(1.25, size / 12)

    def pixel(x, y):
        px, py = x + 0.5, y + 0.5
        circle = abs(math.hypot(px - center, py - center) - ring) - stroke / 2
        bar_x = abs(px - center) - stroke / 2
        bar = max(bar_x, abs(py - size * 0.42) - size * 0.14)
        dot = math.hypot(px - center, py - size * 0.68) - stroke * 0.6
        inside = 1.0 - smoothstep(-0.6, 0.6, min(circle, bar, dot))
        return (*color, round(inside * 255))
    return pixel


def note_png(path, text, look, color, icon=True):
    """A line under the field as the login screen sets it: the label size,
    led by the alert icon when it reports an error."""
    font = face(look["label_size"], look["label_weight"])
    probe = ImageDraw.Draw(PillowImage.new("RGBA", (1, 1)))
    ascent, descent = font.getmetrics()
    height = ascent + descent + 4
    icon_size = look["label_size"] + 1
    offset = icon_size + 6 if icon else 0
    width = round(probe.textlength(text, font=font)) + offset + 4
    image = PillowImage.new("RGBA", (width, height), (0, 0, 0, 0))
    if icon:
        glyph = PillowImage.new("RGBA", (icon_size, icon_size))
        pixel = alert_pixel(icon_size, color)
        glyph.putdata([pixel(x, y) for y in range(icon_size) for x in range(icon_size)])
        image.alpha_composite(glyph, (0, (height - icon_size) // 2))
    ImageDraw.Draw(image).text((offset, height / 2), text, font=font,
                               fill=(*color, 255), anchor="lm")
    image.save(path, format="PNG", optimize=True)


if __name__ == "__main__":
    arguments = sys.argv[1:]
    face_path = None
    if "--face" in arguments:
        at = arguments.index("--face")
        face_path = arguments[at + 1]
        del arguments[at:at + 2]
    main(arguments[0], face_path)

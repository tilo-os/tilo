"""The real-boot sign-in acceptance judges frames the way it claims to."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "greeter_acceptance", ROOT / "os" / "dev" / "greeter-acceptance.py"
)
acceptance = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(acceptance)


def painted(width: int, height: int) -> bytearray:
    """A frame with a gradient backdrop, never pure black."""
    rgb = bytearray()
    for y in range(height):
        for x in range(width):
            rgb += bytes((20 + x % 50, 22 + y % 40, 30 + (x + y) % 30))
    return rgb


def fill(rgb: bytearray, width: int, box: tuple[int, int, int, int], colour: bytes) -> None:
    x0, y0, x1, y1 = box
    for y in range(y0, y1):
        for x in range(x0, x1):
            i = (y * width + x) * 3
            rgb[i : i + 3] = colour


class GreeterAcceptanceTests(unittest.TestCase):
    def test_a_fully_painted_frame_is_whole(self):
        image = acceptance.Image(128, 96, bytes(painted(128, 96)))
        self.assertEqual(image.black_tiles(), [])
        acceptance.judge_whole(image, "painted")

    def test_an_unpainted_hole_is_found_where_it_is(self):
        rgb = painted(128, 96)
        fill(rgb, 128, (32, 48, 64, 80), b"\x00\x00\x00")
        image = acceptance.Image(128, 96, bytes(rgb))
        self.assertEqual(image.black_tiles(), [(32, 48), (48, 48), (32, 64), (48, 64)])
        with self.assertRaisesRegex(acceptance.Failed, "4 unpainted"):
            acceptance.judge_whole(image, "holed")

    def test_a_dark_theme_is_not_mistaken_for_a_hole(self):
        rgb = painted(64, 64)
        fill(rgb, 64, (0, 0, 64, 64), b"\x08\x09\x0b")
        fill(rgb, 64, (8, 8, 40, 40), b"\x40\x50\x60")
        image = acceptance.Image(64, 64, bytes(rgb))
        self.assertEqual(image.black_tiles(), [])

    def test_a_flat_display_is_not_the_sign_in_screen(self):
        image = acceptance.Image(64, 64, b"\x30\x30\x30" * 64 * 64)
        with self.assertRaisesRegex(acceptance.Failed, "flat colour"):
            acceptance.judge_whole(image, "flat")

    def test_changes_are_located_by_tile_and_noise_is_ignored(self):
        base = painted(96, 64)
        noisy = bytearray(base)
        noisy[0] = min(255, noisy[0] + 3)
        changed = bytearray(base)
        fill(changed, 96, (70, 20, 76, 26), b"\xff\x00\x00")
        a = acceptance.Image(96, 64, bytes(base))
        self.assertEqual(a.changed_tiles(acceptance.Image(96, 64, bytes(noisy))), [])
        self.assertEqual(a.changed_tiles(acceptance.Image(96, 64, bytes(changed))), [(64, 16)])

    def test_captures_round_trip_through_ppm_and_png(self):
        rgb = bytes(painted(40, 30))
        with tempfile.TemporaryDirectory() as tmp:
            ppm = Path(tmp) / "frame.ppm"
            ppm.write_bytes(b"P6\n# qemu\n40 30\n255\n" + rgb)
            image = acceptance.Image.read_ppm(ppm)
            self.assertEqual((image.width, image.height, image.rgb), (40, 30, rgb))
            png = Path(tmp) / "frame.png"
            image.write_png(png)
            data = png.read_bytes()
            self.assertTrue(data.startswith(b"\x89PNG\r\n\x1a\n"))
            idat = data.index(b"IDAT")
            length = int.from_bytes(data[idat - 4 : idat], "big")
            raw = zlib.decompress(data[idat + 4 : idat + 4 + length])
            self.assertEqual(len(raw), 30 * (1 + 40 * 3))

    def test_check_image_reports_a_hole_with_a_failing_status(self):
        rgb = painted(64, 64)
        fill(rgb, 64, (0, 0, 64, 16), b"\x00\x00\x00")
        with tempfile.TemporaryDirectory() as tmp:
            ppm = Path(tmp) / "frame.ppm"
            ppm.write_bytes(b"P6\n64 64\n255\n" + bytes(rgb))
            png = Path(tmp) / "frame.png"
            self.assertEqual(
                acceptance.main(["--check-image", str(ppm), "--png", str(png)]), 1
            )
            self.assertTrue(png.exists())


if __name__ == "__main__":
    unittest.main()

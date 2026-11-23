"""The on-screen keyboard's keys, read off a boot sign-in screenshot's OCR.

The keyboard is ui::passcode_keyboard's letters layout: five equally spaced rows, the
digits on top, each row centred. Tesseract reads the keys it can and misses others, it
reads single characters out of labels that are not keys ("?123" in the bottom row), and
the card above the keyboard has stray single letters of its own ("o" of an avatar). Where
a key is therefore comes from the layout, found by the keyboard's rows of keys.
"""
import importlib.util
from pathlib import Path
import unittest

TOOLS = Path(__file__).parents[1]
spec = importlib.util.spec_from_file_location("signin_e2e", TOOLS / "ci/signin_e2e.py")
signin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(signin)

HEADER = ("level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\t"
          "left\ttop\twidth\theight\tconf\ttext")
# The keyboard's key pitch and row pitch, and the row the digit row sits on.
PITCH, ROW_PITCH, DIGIT_ROW = 60, 70, 400
# Stray single letters and the bottom row's lone "1", as the card and the `?123` key read.
CARD = [("o", 120, 90), ("o", 1500, 300), ("c", 300, 200), ("a", 400, 205), ("n", 500, 198)]
BOTTOM_ROW_ONE = ("1", 600 - 4.5 * PITCH, DIGIT_ROW + 4 * ROW_PITCH)


def tsv(*tokens: tuple[str, float, float]) -> str:
    """Tesseract's TSV: a header and one recognised word per line, each a box centred on
    the position it was read at."""
    lines = [HEADER]
    for n, (text, x, y) in enumerate(tokens, start=1):
        lines.append(f"5\t1\t1\t1\t1\t{n}\t{x - 10:.0f}\t{y - 10:.0f}\t20\t20\t96\t{text}")
    return "\n".join(lines) + "\n"


def key(char: str, row: int, index: int, row_width: int) -> tuple[str, float, float]:
    """A key's label and the centre it was read at, per ui::passcode_keyboard's layout."""
    return char, 600 + (index - (row_width - 1) / 2) * PITCH, DIGIT_ROW + row * ROW_PITCH


def keyboard(*extra: tuple[str, float, float]) -> str:
    """A keyboard as OCR reads it: digits 2-8 and every letter, plus `extra`."""
    rows = signin.KEYBOARD_ROWS
    tokens = [key(k, 0, i, len(rows[0])) for i, k in enumerate(rows[0]) if k in "2345678"]
    tokens += [key(k, r, i, len(rows[r])) for r in (1, 2, 3) for i, k in enumerate(rows[r])]
    return tsv(*tokens, *extra)


class KeyPositions(unittest.TestCase):
    def test_the_digit_row_is_the_one_above_the_letters(self) -> None:
        # The card's stray single letters and the bottom row's "1" are below the digit row
        # or alone on their rows; neither may move the keys. The old matcher took the
        # lowest digit as the row and failed with "tesseract found one digit key only".
        keys = signin.key_positions(keyboard(*CARD, BOTTOM_ROW_ONE))
        self.assertEqual(keys["1"], (round(600 - 4.5 * PITCH), DIGIT_ROW))
        self.assertEqual(keys["0"][1], DIGIT_ROW)
        self.assertEqual(keys["q"][1], DIGIT_ROW + ROW_PITCH)
        self.assertEqual(keys["m"][1], DIGIT_ROW + 3 * ROW_PITCH)
        self.assertEqual(keys["\n"][1], DIGIT_ROW + 4 * ROW_PITCH)
        self.assertEqual(len(keys), 10 + 10 + 9 + 7 + 1)

    def test_every_digit_lands_on_the_digit_row(self) -> None:
        keys = signin.key_positions(keyboard(*CARD))
        for i, char in enumerate(signin.KEYBOARD_ROWS[0]):
            self.assertEqual(keys[char], (round(600 + (i - 4.5) * PITCH), DIGIT_ROW), char)

    def test_a_frame_without_the_keyboard_says_so(self) -> None:
        with self.assertRaises(signin.Failed):
            signin.key_positions(tsv(*CARD))
        with self.assertRaises(signin.Failed):
            signin.key_positions("")
        # Letters, but no digit row read above them: the message names what it did read.
        letters_only = [line for line in keyboard().splitlines()[1:]
                        if line.split("\t")[11] not in signin.KEYBOARD_ROWS[0]]
        with self.assertRaises(signin.Failed) as raised:
            signin.key_positions("\n".join([HEADER, *letters_only]) + "\n")
        self.assertIn("no row of digit keys", str(raised.exception))


if __name__ == "__main__":
    unittest.main()

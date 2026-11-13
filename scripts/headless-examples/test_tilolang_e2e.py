"""What the apps' end to end check reads off the desk: a window is read once the session says it
is drawn, never on a timer's guess; no compositor or application is launched.

`windows` reads a window's `rect` off the compositor, which maps a window when its client
commits its first buffer: the desk lists a tab whose window is not drawn yet with `rect: null`
and `frames: 0`. The check must wait for that row to become a drawn one, and fail naming the
window when the session never draws it, instead of reading across the gap.
"""
import importlib.util
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("tilolang_e2e", HERE / "tilolang-e2e.py")
e2e = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(e2e)


class Desk:
    """The desk a session running the check answers with: `undrawn` readings list the tab with no
    `rect` and no committed frame before it lists the drawn window, as the desk does while an
    app's first buffer is on its way."""

    def __init__(self, undrawn, rect, controls):
        self.undrawn = undrawn
        self.rect = rect
        self.controls = controls
        self.readings = 0

    def windows(self):
        self.readings += 1
        window = {"tab": 7, "active": True, "app_id": "so.tilo.lang.today"}
        if self.undrawn > 0:
            self.undrawn -= 1
            return [{**window, "rect": None, "frames": 0}]
        return [{**window, "rect": self.rect, "frames": 3}]

    def invoke(self, provider, ident, args=None):
        return {"ok": {}}

    def targets(self):
        return self.controls


IN, OUT = {"name": "Book the dentist", "rect": [120, 80, 200, 20]}, \
          {"name": "elsewhere", "rect": [900, 80, 200, 20]}


class WindowReads(unittest.TestCase):
    def test_a_window_is_read_once_the_session_reports_it_drawn(self):
        desk = Desk(undrawn=3, rect=[100, 50, 400, 300], controls=[IN, OUT])
        names = e2e.window_controls(desk, {"tab": 7})
        self.assertEqual(names, ["Book the dentist"])
        self.assertGreaterEqual(desk.readings, 4, "the window was read before the session drew it")

    def test_a_row_without_a_rect_or_a_frame_is_not_a_drawn_window(self):
        for row in ({"tab": 7, "rect": None, "frames": 0},
                    {"tab": 7, "rect": None, "frames": 2},
                    {"tab": 7, "rect": [0, 0, 10, 10], "frames": 0}):
            with self.subTest(row=row):
                self.assertIsNone(e2e.drawn_window(_Answering([row]), 7))
                self.assertIsNone(e2e.drawn_window(_Answering([]), 7))
        drawn = {"tab": 7, "rect": [0, 0, 10, 10], "frames": 1}
        self.assertEqual(e2e.drawn_window(_Answering([drawn]), 7), drawn)

    def test_a_window_the_session_never_draws_fails_naming_it(self):
        desk = Desk(undrawn=10 ** 9, rect=[100, 50, 400, 300], controls=[IN])
        wanted = e2e.until
        e2e.until = lambda what, check, deadline=60.0, step=0.25: wanted(what, check, 0.3, step)
        try:
            with self.assertRaises(e2e.Failed) as raised:
                e2e.window_controls(desk, {"tab": 7})
        finally:
            e2e.until = wanted
        self.assertIn("window 7 drawn on the desk to be read", str(raised.exception))


class _Answering:
    """A session that lists the same rows every time it is asked."""

    def __init__(self, rows):
        self.rows = rows

    def windows(self):
        return self.rows


if __name__ == "__main__":
    unittest.main()

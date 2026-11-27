"""What the harness's one client answers before it starts anything.

No compositor, profile or socket is made here: these are the decisions a
script makes from a path and an environment, which must be right before a
session is started at all."""
import os
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "headless-examples"))
import real_apps  # noqa: E402


class SocketPathLimit(unittest.TestCase):
    def test_path_at_the_limit_is_taken(self):
        # Linux holds a socket path in 108 bytes including its terminating NUL.
        path = "/tmp/" + "a" * 102
        self.assertEqual(len(path.encode()), 107)
        self.assertEqual(real_apps.socket_length_refusal(path), "")

    def test_path_one_byte_past_it_is_refused_before_anything_starts(self):
        path = "/tmp/" + "a" * 103
        refusal = real_apps.socket_length_refusal(path)
        self.assertIn("108 bytes", refusal)
        self.assertIn("at most 107", refusal)

    def test_the_limit_counts_encoded_bytes_not_characters(self):
        path = "/tmp/" + "é" * 54
        self.assertEqual(len(path), 59)
        self.assertNotEqual(real_apps.socket_length_refusal(path), "")


class ControlSocketFromArgv(unittest.TestCase):
    def setUp(self):
        self.saved = {name: os.environ.get(name)
                      for name in ("TILO_TEST_CONTROL", "XDG_RUNTIME_DIR")}

    def tearDown(self):
        for name, value in self.saved.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value

    def test_an_argument_names_the_session_first(self):
        os.environ["TILO_TEST_CONTROL"] = "/run/user/1000/tilo-session/from-env.sock"
        self.assertEqual(Path("/tmp/explicit.sock"),
                         real_apps.control_socket_from_argv(["script.py", "/tmp/explicit.sock"]))

    def test_the_environment_is_the_next_answer(self):
        os.environ["TILO_TEST_CONTROL"] = "/run/user/1000/tilo-session/from-env.sock"
        self.assertEqual(Path("/run/user/1000/tilo-session/from-env.sock"),
                         real_apps.control_socket_from_argv(["script.py"]))

    def test_without_either_it_is_the_private_default(self):
        os.environ.pop("TILO_TEST_CONTROL", None)
        os.environ["XDG_RUNTIME_DIR"] = "/run/user/1000"
        self.assertEqual(Path("/run/user/1000/tilo-session/test-control.sock"),
                         real_apps.control_socket_from_argv(["script.py"]))

    def test_no_private_directory_says_so_rather_than_guessing(self):
        os.environ.pop("TILO_TEST_CONTROL", None)
        os.environ.pop("XDG_RUNTIME_DIR", None)
        with self.assertRaises(real_apps.Failed):
            real_apps.control_socket_from_argv(["script.py"])


class SearchRowsAreTheAnswer(unittest.TestCase):
    """What a capture waits for: the query it typed, a palette that has stopped
    filling in, and the rows it asks of it. A shot taken on the keys alone shows
    "Searching…", which is what these decide against."""

    @staticmethod
    def answer(query="chromium", searching=False, titles=("Chromium Web Browser",)):
        return {"query": query, "searching": searching,
                "rows": [{"title": title} for title in titles]}

    def test_the_typed_querys_rows_are_the_answer(self):
        self.assertTrue(real_apps.search_answer_shown(
            self.answer(), "chromium", ("Chromium",)))

    def test_rows_of_the_query_before_it_are_not_an_answer(self):
        self.assertFalse(real_apps.search_answer_shown(
            self.answer(query=""), "chromium", ()))

    def test_a_palette_still_filling_in_is_not_an_answer(self):
        self.assertFalse(real_apps.search_answer_shown(
            self.answer(searching=True), "chromium", ()))

    def test_a_row_still_out_is_not_an_answer(self):
        self.assertFalse(real_apps.search_answer_shown(
            self.answer(titles=("Search the web for chromium",)), "chromium", ("Chromium",)))

    def test_a_name_is_found_inside_a_rows_title(self):
        self.assertTrue(real_apps.search_answer_shown(
            self.answer(titles=("Send to Assistant: chromium", "Chromium Web Browser")),
            "chromium", ("chromium",)))


class Desktop:
    """A session's answers, as `Session.launch` reads them: the windows the desktop
    shows, and the invokes the desktop is asked for."""

    def __init__(self, windows, opened=(), refusal=None):
        self.shown = list(windows)
        self.opened = list(opened)
        self.invokes = []
        self.refusal = refusal

    def windows(self, app=None):
        if app is None:
            return list(self.shown)
        return [window for window in self.shown if window.get("app_id") == app]

    def open(self, ident, deadline=120.0):
        self.invokes.append(ident)
        if self.refusal is not None:
            raise real_apps.Refused("invoke", self.refusal)
        if ident in self.opened:
            self.shown.append({"app_id": ident, "title": "Untitled"})
        return "Accepted"


class Launch(unittest.TestCase):
    """An app the desktop already shows is answered by its window at once, and
    one this device lacks is refused at once: a launch that waited for the
    app's listing entry waited out its deadline for every app already open."""

    def setUp(self):
        self.until = real_apps.until

        def brief(what, check, deadline=60.0, step=0.25):
            return self.until(what, check, min(deadline, 0.3), step)

        real_apps.until = brief

    def tearDown(self):
        real_apps.until = self.until

    def session(self, desktop: Desktop) -> real_apps.Session:
        return type("Launched", (real_apps.Session,), {
            "windows": lambda _self, app=None: desktop.windows(app),
            "open": lambda _, ident, deadline=120.0: desktop.open(ident, deadline),
        })(Path("/nonexistent-session"))

    def test_an_app_the_desktop_already_shows_is_answered_by_its_window(self):
        notes = real_apps.app_id("notes")
        desktop = Desktop([{"app_id": notes, "title": "Untitled"}])
        window = self.session(desktop).launch("notes")
        self.assertEqual(window["app_id"], notes)
        self.assertEqual(window["title"], "Untitled")
        self.assertEqual(desktop.invokes, [], "an app already shown was opened again")

    def test_an_app_the_desktop_does_not_show_is_opened(self):
        notes = real_apps.app_id("notes")
        desktop = Desktop([], opened=[notes])
        window = self.session(desktop).launch(notes)
        self.assertEqual(desktop.invokes, [notes])
        self.assertEqual(window["app_id"], notes)

    def test_an_app_this_device_lacks_is_refused_at_once(self):
        notes = real_apps.app_id("notes")
        desktop = Desktop([], refusal={"code": "unknown_action",
                                       "message": f"no application {notes}"})
        with self.assertRaises(real_apps.Refused):
            self.session(desktop).launch("notes")


class Invocation(unittest.TestCase):
    """The frame an action is invoked with: its resource and action apart, its arguments typed."""

    def test_arguments_go_as_the_typed_object(self):
        sent = real_apps.invocation("session", "desktop:chrome", "new-workspace", {"name": "Q"})
        request = sent["message"]["Invoke"]
        self.assertEqual(request["target"]["address"],
                         {"resource": "desktop:chrome", "action": "new-workspace"})
        self.assertEqual(request["argument"], {"Json": {"name": "Q"}})

    def test_a_resource_and_action_joined_in_one_id_is_refused_before_it_is_sent(self):
        with self.assertRaises(TypeError):
            real_apps.invocation("session", "desktop:chrome\x1fnew-workspace", {"name": "Q"})
        with self.assertRaises(TypeError):
            real_apps.invocation("session", "desktop:chrome\x1fnew-workspace", "new-workspace")

    def test_reading_a_status_as_the_old_receipt_fails_saying_so(self):
        answer = real_apps.frame({"Receipt": {"status": {"Completed": {"value": 1}}}})
        status = real_apps.receipt(answer)
        self.assertEqual(status["Completed"], {"value": 1})
        with self.assertRaises(KeyError):
            status.get("receipt")
        with self.assertRaises(KeyError):
            status["update"]


if __name__ == "__main__":
    unittest.main()

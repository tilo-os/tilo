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


if __name__ == "__main__":
    unittest.main()

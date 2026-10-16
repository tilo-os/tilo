"""Regression tests for headless proof failures that must not look successful."""
import builtins
import importlib.util
import pathlib
import subprocess
import sys
import unittest
from unittest import mock


EXAMPLES = pathlib.Path(__file__).resolve().parents[1] / "headless-examples"


def load_example(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), EXAMPLES / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    with mock.patch.object(sys, "path", [str(EXAMPLES), *sys.path]):
        spec.loader.exec_module(module)
    return module


CURSOR = load_example("peers-cross-both-ways")
LIFECYCLE = load_example("app-lifecycle-timing")


class CursorEvidenceTests(unittest.TestCase):
    def test_missing_pillow_is_an_error_not_a_pixel_count(self):
        original_import = builtins.__import__

        def without_pillow(name, *args, **kwargs):
            if name == "PIL":
                raise ImportError("Pillow unavailable")
            return original_import(name, *args, **kwargs)

        with mock.patch("builtins.__import__", side_effect=without_pillow):
            with self.assertRaisesRegex(RuntimeError, "requires Pillow") as raised:
                CURSOR.saturated_near("unused.png", (760, 470))
        self.assertIsInstance(raised.exception.__cause__, ImportError)


class ProcessObservationTests(unittest.TestCase):
    def test_unreadable_or_malformed_status_is_not_process_exit(self):
        for error in (PermissionError("denied"), OSError("I/O failure")):
            with self.subTest(error=error):
                with mock.patch.object(pathlib.Path, "read_text", side_effect=error):
                    with self.assertRaises(type(error)):
                        LIFECYCLE.proc_state(123)
        with mock.patch.object(pathlib.Path, "read_text", side_effect=["Name: app\n", "poll"]):
            with self.assertRaisesRegex(RuntimeError, "missing State"):
                LIFECYCLE.proc_state(123)

    def test_exited_and_zombie_processes_remain_supported(self):
        for error in (FileNotFoundError(), ProcessLookupError()):
            with mock.patch.object(pathlib.Path, "read_text", side_effect=error):
                self.assertIsNone(LIFECYCLE.proc_state(123))
        with mock.patch.object(pathlib.Path, "read_text", side_effect=["State: Z (zombie)", "0"]):
            self.assertIsNone(LIFECYCLE.proc_state(123))

    def test_live_process_is_still_reported(self):
        with mock.patch.object(pathlib.Path, "read_text", side_effect=[
            "State: S (sleeping)\nVmRSS: 2048 kB\nThreads: 2\n", "poll", "app\n"
        ]):
            self.assertIn("app state=S wchan=poll rss=2MB threads=2", LIFECYCLE.proc_state(123))


class DisplayPublicationTests(unittest.TestCase):
    def setUp(self):
        self.session = LIFECYCLE.Session.__new__(LIFECYCLE.Session)
        self.session.runtime = "/private/run"
        self.session.env = {"XDG_RUNTIME_DIR": self.session.runtime}
        self.session.proc = mock.Mock(pid=123)

    def test_failed_activation_update_is_propagated(self):
        def update(command, **kwargs):
            if kwargs.get("check"):
                raise subprocess.CalledProcessError(1, command)
            return subprocess.CompletedProcess(command, 1)

        with mock.patch.object(LIFECYCLE.os, "listdir", return_value=["wayland-1", "wayland-1.lock"]), \
                mock.patch.object(LIFECYCLE, "session_bus_address", return_value="unix:path=/private/bus"), \
                mock.patch.object(LIFECYCLE.subprocess, "run", side_effect=update):
            with self.assertRaises(subprocess.CalledProcessError):
                self.session.publish_display_to_bus()

    def test_missing_setup_inputs_fail_instead_of_skipping(self):
        for sockets, bus, message in (([], "bus", "no Wayland"), (["wayland-1"], None, "no session bus")):
            with self.subTest(message=message):
                with mock.patch.object(LIFECYCLE.os, "listdir", return_value=sockets), \
                        mock.patch.object(LIFECYCLE, "session_bus_address", return_value=bus):
                    with self.assertRaisesRegex(RuntimeError, message):
                        self.session.publish_display_to_bus()

    def test_success_publishes_only_to_private_bus(self):
        with mock.patch.object(LIFECYCLE.os, "listdir", return_value=["wayland-1"]), \
                mock.patch.object(LIFECYCLE, "session_bus_address", return_value="private-bus"), \
                mock.patch.object(LIFECYCLE.subprocess, "run") as run:
            self.session.publish_display_to_bus()
        self.assertEqual(run.call_args.kwargs["env"]["DBUS_SESSION_BUS_ADDRESS"], "private-bus")
        self.assertTrue(run.call_args.kwargs["check"])


if __name__ == "__main__":
    unittest.main()

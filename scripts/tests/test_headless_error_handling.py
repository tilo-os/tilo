"""Regression tests for headless proof failures that must not look successful."""
import builtins
import contextlib
import importlib.util
import io
import pathlib
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


EXAMPLES = pathlib.Path(__file__).resolve().parents[1] / "headless-examples"


def load_example(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), EXAMPLES / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    with mock.patch.object(sys, "path", [str(EXAMPLES), *sys.path]):
        spec.loader.exec_module(module)
    return module


CURSOR = load_example("desktop-peer-cursor-proof")
LIFECYCLE = load_example("app-lifecycle-timing")


class CursorEvidenceTests(unittest.TestCase):
    def setUp(self):
        CURSOR.failures.clear()

    def run_proof(self, capture_control=True, pixel_error=None):
        with tempfile.TemporaryDirectory() as root:
            binary = pathlib.Path(root) / "shell"
            binary.touch()
            session = mock.Mock(name="session")
            session.name = "test"
            session.socket = str(pathlib.Path(root) / "control.sock")
            session.log_path = str(pathlib.Path(root) / "shell.log")
            session.wait_up.return_value = True
            session.enumerate.return_value = [{"provider": "actions", "id": "peer.invite"}]
            session.pointers.return_value = [{"x": 760, "y": 470, "name": "quinten"}]

            def snapshot(path):
                if capture_control or not path.endswith("control-peer-moved-away.png"):
                    pathlib.Path(path).write_bytes(b"x" * 6000)
                return {"ok": {}}

            session.snapshot.side_effect = snapshot
            with contextlib.ExitStack() as stack:
                stack.enter_context(mock.patch.object(CURSOR, "SHELL", binary))
                stack.enter_context(mock.patch.object(CURSOR, "Session", return_value=session))
                stack.enter_context(mock.patch.object(CURSOR, "mint_invite", return_value="invite"))
                stack.enter_context(mock.patch.object(CURSOR, "join", return_value=True))
                stack.enter_context(mock.patch.object(CURSOR, "wait_for", side_effect=lambda pred, **kw: pred()))
                stack.enter_context(mock.patch.object(CURSOR.time, "sleep"))
                pixels = stack.enter_context(mock.patch.object(CURSOR, "saturated_near"))
                pixels.side_effect = pixel_error if pixel_error else [100, 0]
                stack.enter_context(mock.patch.object(sys, "argv", ["proof", "--out", root]))
                stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
                return CURSOR.main()

    def test_missing_control_capture_fails_proof(self):
        self.assertEqual(self.run_proof(capture_control=False), 1)
        self.assertIn("the moved-away control frame was captured", CURSOR.failures)

    def test_both_pixel_observations_allow_success(self):
        self.assertEqual(self.run_proof(), 0)

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
        with self.assertRaisesRegex(RuntimeError, "requires Pillow"):
            self.run_proof(pixel_error=raised.exception)


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

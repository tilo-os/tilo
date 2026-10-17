"""Regression tests for headless proof failures that must not look successful."""
import builtins
import importlib.util
import pathlib
import subprocess
import sys
import unittest
import tempfile
from unittest import mock


EXAMPLES = pathlib.Path(__file__).resolve().parents[1] / "headless-examples"


def load_example(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), EXAMPLES / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    with mock.patch.object(sys, "path", [str(EXAMPLES), *sys.path]), \
            mock.patch.dict(sys.modules, {spec.name: module}):
        spec.loader.exec_module(module)
    return module


CURSOR = load_example("peers-cross-both-ways")
LIFECYCLE = load_example("app-lifecycle-timing")
CLIENT = load_example("tilo_client")
CONTROL = load_example("_control")


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


class ClientFailureTests(unittest.TestCase):
    def test_failed_connect_closes_socket_in_both_clients(self):
        for module in [CLIENT, CONTROL]:
            with self.subTest(module=module.__name__):
                sock = mock.Mock()
                sock.connect.side_effect = ConnectionRefusedError('not ready')
                with mock.patch.object(module.socket, 'socket', return_value=sock):
                    with self.assertRaises(ConnectionRefusedError):
                        if module is CLIENT:
                            module.TiloClient('/private/control').raw({'op': 'state_dump'})
                        else:
                            module.call('/private/control', {'op': 'state_dump'})
                sock.close.assert_called_once()

    def test_state_dump_propagates_structured_error(self):
        client = CLIENT.TiloClient('/private/control')
        with mock.patch.object(client, '_call', return_value={
                'err': {'code': 'forbidden', 'message': 'not authorized'}}):
            with self.assertRaises(CLIENT.TiloError) as raised:
                client.state_dump()
        self.assertEqual(raised.exception.code, 'forbidden')
        state = {'workspace_tree': {}, 'chrome': {}}
        for reply in [state, {'state': state}]:
            with mock.patch.object(client, '_call', return_value=reply):
                self.assertEqual(client.state_dump(), state)

    def test_context_exit_reaps_after_kill(self):
        proc = mock.Mock()
        proc.wait.side_effect = [subprocess.TimeoutExpired('shell', 5), 0]
        with mock.patch.object(CLIENT.os, 'unlink'):
            CLIENT._SpawnedTilo(proc, None, '/private/control').__exit__()
        proc.kill.assert_called_once()
        self.assertEqual(proc.wait.call_count, 2)

    def test_spawn_failure_removes_disposable_profile(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            binary = root / 'shell'
            binary.touch()
            profile = root / 'profile'
            profile.mkdir()
            with mock.patch.object(CLIENT, 'scratch_session_env', return_value=({}, profile)), \
                    mock.patch.object(CLIENT.subprocess, 'Popen', side_effect=OSError('spawn denied')):
                with self.assertRaisesRegex(OSError, 'spawn denied'):
                    CLIENT.TiloClient.spawn_headless(socket_path=root / 'control', binary=binary)
            self.assertFalse(profile.exists())

    def test_existing_socket_is_not_sufficient_for_readiness(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            binary = root / 'shell'
            binary.touch()
            profile = root / 'profile'
            profile.mkdir()
            proc = mock.Mock(returncode=None)
            proc.poll.return_value = None
            with mock.patch.object(CLIENT, 'scratch_session_env', return_value=({}, profile)), \
                    mock.patch.object(CLIENT.subprocess, 'Popen', return_value=proc), \
                    mock.patch.object(CLIENT.os.path, 'exists', return_value=True), \
                    mock.patch.object(CLIENT.TiloClient, 'raw', side_effect=ConnectionRefusedError('refused')), \
                    mock.patch.object(CLIENT.time, 'sleep'):
                with self.assertRaises(TimeoutError):
                    CLIENT.TiloClient.spawn_headless(socket_path=root / 'control', binary=binary,
                        wait_seconds=.01)
            proc.terminate.assert_called_once()
            proc.wait.assert_called_once_with(timeout=5)
            self.assertFalse(profile.exists())

    def test_answering_socket_and_live_child_remain_supported(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            binary = root / 'shell'
            binary.touch()
            profile = root / 'profile'
            profile.mkdir()
            proc = mock.Mock()
            proc.poll.return_value = None
            with mock.patch.object(CLIENT, 'scratch_session_env', return_value=({}, profile)), \
                    mock.patch.object(CLIENT.subprocess, 'Popen', return_value=proc), \
                    mock.patch.object(CLIENT.os.path, 'exists', return_value=True), \
                    mock.patch.object(CLIENT.TiloClient, 'raw', return_value={'targets': []}):
                session = CLIENT.TiloClient.spawn_headless(socket_path=root / 'control', binary=binary,
                    wait_seconds=1)
                with session as client:
                    self.assertEqual(client.socket_path, str(root / 'control'))
                    self.assertEqual(client.timeout, 5.0)
            proc.wait.assert_called_once_with(timeout=5)
            self.assertFalse(profile.exists())


if __name__ == "__main__":
    unittest.main()

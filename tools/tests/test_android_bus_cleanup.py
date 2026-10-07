"""Exercise private bus cleanup against actual Linux process identities."""
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'android'))
from run_integration_journey import stop_bus_helpers


class AndroidBusCleanup(unittest.TestCase):
    def helper(self, runtime, hidden=False):
        code = ('import ctypes,signal;libc=ctypes.CDLL(None);'
                'assert libc.prctl(15,b"dbus-daemon",0,0,0)==0;'
                'assert libc.prctl(4,' + str(int(not hidden)) + ',0,0,0)==0;'
                'print("ready",flush=True);signal.pause()')
        process = subprocess.Popen([sys.executable, '-c', code],
            env={'XDG_RUNTIME_DIR': runtime}, stdout=subprocess.PIPE)
        self.addCleanup(self.reap, process)
        self.assertTrue(select.select([process.stdout], [], [], 5)[0])
        self.assertEqual(process.stdout.readline(), b'ready\n')
        return process

    def reap(self, process):
        if process.poll() is None: process.kill()
        process.wait(timeout=5)
        process.stdout.close()

    def cleanup_one(self, process, runtime):
        with patch.object(Path, 'iterdir', return_value=iter([Path('/proc') / str(process.pid)])):
            stop_bus_helpers(runtime)

    def test_owned_live_helper_is_stopped(self):
        with tempfile.TemporaryDirectory() as runtime:
            process = self.helper(runtime)
            self.cleanup_one(process, runtime)
            self.assertEqual(process.wait(timeout=5), -signal.SIGTERM)

    def test_unrelated_live_helper_is_preserved(self):
        with tempfile.TemporaryDirectory() as runtime:
            process = self.helper(runtime + '/unrelated')
            self.cleanup_one(process, runtime)
            self.assertIsNone(process.poll())

    def test_unreadable_live_helper_fails(self):
        with tempfile.TemporaryDirectory() as runtime:
            process = self.helper(runtime, hidden=True)
            read_bytes = Path.read_bytes
            def unavailable_environment(path):
                if path == Path('/proc') / str(process.pid) / 'environ':
                    # Privileged CI may read nondumpable processes. Control only
                    # that read failure; retain real process identity/liveness.
                    raise PermissionError('live helper environment is unavailable')
                return read_bytes(path)
            with patch.object(Path, 'read_bytes', unavailable_environment), self.assertRaises(PermissionError):
                self.cleanup_one(process, runtime)
            self.assertIsNone(process.poll())

    def test_proven_exited_helper_needs_no_environment_read(self):
        with tempfile.TemporaryDirectory() as runtime:
            process = self.helper(runtime, hidden=True)
            descriptor = os.pidfd_open(process.pid)
            try:
                process.kill()
                self.assertTrue(select.select([descriptor], [], [], 5)[0])
                self.cleanup_one(process, runtime)
            finally:
                os.close(descriptor)

    def test_exit_during_environment_read_is_proven_by_pidfd(self):
        with tempfile.TemporaryDirectory() as runtime:
            process = self.helper(runtime, hidden=True)
            descriptor = os.pidfd_open(process.pid)
            read_bytes = Path.read_bytes
            def exit_before_read(path):
                if path == Path('/proc') / str(process.pid) / 'environ':
                    process.kill()
                    self.assertTrue(select.select([descriptor], [], [], 5)[0])
                    raise PermissionError('process exited during environment observation')
                return read_bytes(path)
            try:
                with patch.object(Path, 'read_bytes', exit_before_read):
                    self.cleanup_one(process, runtime)
                self.assertEqual(process.wait(timeout=5), -signal.SIGKILL)
            finally:
                os.close(descriptor)

    def test_environment_loss_before_pidfd_readiness_waits_for_actual_exit(self):
        with tempfile.TemporaryDirectory() as runtime:
            process = self.helper(runtime, hidden=True)
            read_bytes = Path.read_bytes
            timer = threading.Timer(0.05, process.kill)
            def lost_environment_before_exit(path):
                if path == Path('/proc') / str(process.pid) / 'environ':
                    # The kernel revokes environ access during exit before the
                    # pidfd is ready. Keep both the delay and exit real here.
                    self.assertIsNone(process.poll())
                    timer.start()
                    raise PermissionError('environment unavailable before pidfd readiness')
                return read_bytes(path)
            try:
                with patch.object(Path, 'read_bytes', lost_environment_before_exit):
                    self.cleanup_one(process, runtime)
                self.assertEqual(process.wait(timeout=5), -signal.SIGKILL)
            finally:
                if timer.ident is not None: timer.join(timeout=5)

"""Actual lower process/file boundaries; never creates a source grant or capsule."""
import importlib.util
import ast
import base64
import http.server
import os
from pathlib import Path
import signal
import tempfile
import time
import threading
import types
import unittest
import urllib.request
from unittest.mock import patch

import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[3] / 'tools/tests'))
from hang import HANG_BOUND  # noqa: E402

spec = importlib.util.spec_from_file_location('owned_source_stage', Path(__file__).with_name('source.py'))
source = importlib.util.module_from_spec(spec)
spec.loader.exec_module(source)


class RetentionObserved(Exception):
    pass


class SourceBoundaries(unittest.TestCase):
    def test_real_production_dav_head_cannot_certify_get(self):
        # Execute the actual production HTTP handler with an owned loopback
        # server. TLS/signing is outside this lower HTTP-method regression.
        parsed = ast.parse(Path(source.__file__).read_text())
        dav = next(node for node in parsed.body if isinstance(node, ast.ClassDef) and node.name == 'Dav')
        init = next(node for node in dav.body if isinstance(node, ast.FunctionDef) and node.name == '__init__')
        handler = next(node for node in init.body if isinstance(node, ast.ClassDef) and node.name == 'Handler')
        owner = types.SimpleNamespace(lock=threading.Lock(), gets=0, delivered_ranges=[], uploads={})
        namespace = dict(source.__dict__, owner=owner)
        exec(compile(ast.Module(body=[handler], type_ignores=[]), source.__file__, 'exec'), namespace)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), namespace['Handler'])
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            url = f'http://127.0.0.1:{server.server_port}/dav/Documents/selected-document.txt'
            headers = {'Authorization': 'Basic ' + base64.b64encode(b'ada:correct horse').decode(),
                       'Connection': 'close'}
            with urllib.request.urlopen(urllib.request.Request(url, headers=headers, method='HEAD'), timeout=HANG_BOUND) as response:
                self.assertEqual(response.read(), b'')
            with owner.lock:
                self.assertEqual(owner.gets, 0)
                self.assertEqual(owner.delivered_ranges, [])
            with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=HANG_BOUND) as response:
                self.assertEqual(response.read(), source.DAV_BYTES)
            # The handler records a GET only once its body is written, which
            # the client can finish reading first: wait for the record,
            # bounded by the shared hang bound.
            deadline = time.monotonic() + HANG_BOUND
            while time.monotonic() < deadline:
                with owner.lock:
                    if owner.gets:
                        break
                time.sleep(0.01)
            with owner.lock:
                self.assertEqual(owner.gets, 1)
                self.assertEqual(owner.delivered_ranges, [(0, len(source.DAV_BYTES))])
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=HANG_BOUND)
            self.assertFalse(thread.is_alive())

    def test_actual_source_owner_is_observed_then_reaped(self):
        pid = os.fork()
        if pid == 0:
            os._exit(0)
        reaped = False
        try:
            observed = source.wait_source_namespace(pid, time.monotonic() + HANG_BOUND)
            reaped = True
            self.assertEqual(observed.si_pid, pid)
            self.assertEqual(observed.si_code, os.CLD_EXITED)
            self.assertEqual(observed.si_status, 0)
            with self.assertRaises(ChildProcessError):
                os.waitpid(pid, os.WNOHANG)
        finally:
            try:
                if reaped:
                    raise ChildProcessError
                os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
            except ProcessLookupError:
                pass
            except ChildProcessError:
                pass

    def test_deadline_retains_live_owner_without_numeric_signal_or_reap(self):
        pid = os.fork()
        if pid == 0:
            time.sleep(3600)
            os._exit(0)
        try:
            with patch.object(source, 'retain_unknown_namespace', side_effect=RetentionObserved):
                with self.assertRaises(RetentionObserved):
                    source.wait_source_namespace(pid, time.monotonic() - 1)
            self.assertIsNone(os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT))
        finally:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)

    def test_actual_package_held_reader_refuses_fifo_and_link(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            regular = directory / 'ciphertext'
            regular.write_bytes(b'ciphertext transport only')
            self.assertEqual(source.held_bytes(regular, 64), regular.read_bytes())
            fifo = directory / 'fifo'
            os.mkfifo(fifo)
            with self.assertRaises(RuntimeError):
                source.held_bytes(fifo, 64)
            alias = directory / 'alias'
            alias.symlink_to(regular)
            with self.assertRaises(OSError):
                source.held_bytes(alias, 64)


if __name__ == '__main__':
    unittest.main()

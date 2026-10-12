"""Probe real Unix socket readiness with an owned synthetic daemon, without Java."""
import importlib.util
import json
from pathlib import Path
import socket
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('signal_fixture', ROOT / 'tools/apps/signal_runtime_fixture.py')
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)

DAEMON = '''
import json, pathlib, socket, sys, time
if '--version' in sys.argv:
    print('signal-cli 0.14.8')
    sys.exit(0)
gate, result, exit_after_bind = sys.argv[1:4]
endpoint = sys.argv[sys.argv.index('--socket') + 1]
with socket.socket(socket.AF_UNIX) as server:
    server.bind(endpoint)
    while not pathlib.Path(gate).exists():
        time.sleep(0.001)
    if exit_after_bind == 'yes':
        print('synthetic daemon exited after bind', file=sys.stderr, flush=True)
        sys.exit(2)
    server.listen(1)
    with server.accept()[0] as stream, stream.makefile('rb') as reader:
        request = json.loads(reader.readline())
        assert request['method'] == 'listAccounts' and request['params'] == {}
        stream.sendall((json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': json.loads(result)}) + '\\n').encode())
    while True:
        time.sleep(1)
'''


class SignalRuntimeFixture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)
        self.script = self.work / 'daemon.py'
        self.script.write_text(DAEMON)
        self.gate = self.work / 'listen'
        self.refused = 0
        self.elapsed = 0
        original = socket.socket
        case = self

        class ObservedSocket(original):
            def connect(self, address):
                try:
                    return super().connect(address)
                except ConnectionRefusedError:
                    # The daemon cannot listen until a real OS connection has
                    # observed its bound-but-not-listening socket.
                    case.refused += 1
                    if case.elapsed == 0:
                        case.gate.touch()
                    else:
                        case.elapsed = 21
                    raise

        self.socket = ObservedSocket

    def run_probe(self, result=(), exit_after_bind=False, timeout=False):
        command = [sys.executable, str(self.script), str(self.gate),
                   json.dumps(list(result)), 'yes' if exit_after_bind else 'no']
        with patch.object(fixture, 'command', return_value=command), \
                patch.object(fixture.socket, 'socket', self.socket):
            if timeout:
                self.elapsed = 1
                with patch.object(fixture.time, 'monotonic', side_effect=lambda: self.elapsed):
                    fixture.probe(self.work)
            else:
                fixture.probe(self.work)

    def test_bound_socket_is_not_ready_until_it_accepts_the_actual_rpc(self):
        self.run_probe()
        self.assertGreater(self.refused, 0)

    def test_nonempty_account_response_remains_refused(self):
        with self.assertRaisesRegex(RuntimeError, 'fixture must contain no authenticated accounts'):
            self.run_probe(result=({'number': '+12025550123'},))
        self.assertGreater(self.refused, 0)

    def test_exit_after_bind_fails_with_the_daemon_diagnostic(self):
        with self.assertRaisesRegex(RuntimeError, 'exited before accepting RPC.*synthetic daemon exited after bind'):
            self.run_probe(exit_after_bind=True)
        self.assertGreater(self.refused, 0)

    def test_bound_nonlistener_cannot_extend_the_original_twenty_second_deadline(self):
        with self.assertRaisesRegex(RuntimeError, 'daemon did not accept RPC'):
            self.run_probe(timeout=True)
        self.assertGreater(self.refused, 0)


if __name__ == '__main__':
    unittest.main()

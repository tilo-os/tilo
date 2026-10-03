"""Bounded real transport + fail-closed VM/physical gates; no VM or disk mutation."""
import base64
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import socket
import struct
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

protocol = load('refresh_protocol_test', ROOT / 'usr/libexec/tilo-installer-code-protocol.py')
host = load('refresh_host_test', ROOT / 'dev/refresh-installer-code.py')


class Transport(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.source = Path(self.temporary.name)
        self.file = self.source / 'os/usr/libexec/tilo-install'
        self.file.parent.mkdir(parents=True)
        self.file.write_bytes(b'#!/bin/bash\necho development\n')
        self.raw = protocol.bundle(self.source, 'a' * 40, True)

    def test_fragmented_transport_and_source_content_identity(self):
        class Fragmented(io.BytesIO):
            def read(self, size=-1):
                return super().read(min(size, 3))
        incoming = Fragmented(struct.pack('!I', len(self.raw)) + self.raw)
        files, receipt = protocol.receive(incoming)
        self.assertEqual(files['usr/libexec/tilo-install'], self.file.read_bytes())
        self.assertEqual(receipt['source'], {'revision': 'a' * 40, 'dirty': True})
        class Partial(io.BytesIO):
            def write(self, data):
                return super().write(data[:7])
        output = Partial()
        protocol.send(output, self.raw)
        self.assertEqual(output.getvalue(), struct.pack('!I', len(self.raw)) + self.raw)

    def test_bad_hash_path_duplicates_and_packet_size_refused(self):
        packet = json.loads(self.raw)
        for variant in ('hash', 'path', 'duplicate'):
            document = json.loads(self.raw)
            if variant == 'hash': document['files'][0]['sha256'] = 'b' * 64
            elif variant == 'path': document['files'][0]['path'] = '../../etc/shadow'
            else: document['files'] *= 2
            with self.subTest(variant=variant), self.assertRaises(ValueError):
                protocol.validate(protocol.canonical(document))
        with self.assertRaises(ValueError):
            protocol.receive(io.BytesIO(struct.pack('!I', protocol.LIMIT + 1)))
        with self.assertRaises(ValueError):
            protocol.validate(b'{"schema":1,"schema":2}')

    def test_real_loopback_refresh_ack_binds_exact_manifest(self):
        listener = socket.socket()
        self.addCleanup(listener.close)
        listener.bind(('127.0.0.1', 0)); listener.listen(1)
        port = listener.getsockname()[1]
        results = []
        def server():
            client, _ = listener.accept()
            with client:
                stream = client.makefile('rwb', buffering=0)
                files, receipt = protocol.receive(stream)
                results.append(files)
                protocol.write_all(stream, protocol.canonical({'ok': True, 'receipt': receipt}) + b'\n')
        worker = threading.Thread(target=server, daemon=True)
        worker.start()
        def git(argv, **kwargs):
            class Result:
                stdout = 'a' * 40 + '\n' if 'rev-parse' in argv else b' M os/usr/libexec/tilo-install'
            return Result()
        (self.source / 'installer-code-phase.json').write_text(json.dumps({'phase':'startup'}))
        (self.source / 'installer-code-channel.json').write_text(json.dumps({'schema':'tilo.installer-code-runtime.v1', 'enabled':True, 'port':port, 'source':str(self.source)}))
        with patch.object(host.subprocess, 'run', side_effect=git):
            receipt = host.refresh(self.source, port, self.source / 'receipt.json', timeout=3)
        worker.join(3)
        self.assertFalse(worker.is_alive())
        self.assertEqual(results[0]['usr/libexec/tilo-install'], self.file.read_bytes())
        self.assertEqual(json.loads((self.source / 'receipt.json').read_text()), receipt)

    def test_physical_admission_rejects_refreshed_guest_before_other_fields(self):
        admission = load('refresh_admission_test', ROOT / 'dev/windows-direct-dev-admission.py')
        proof = self.source / 'proof.json'
        proof.write_text(json.dumps({'installer_code_runtime': {'schema':'tilo.installer-code-runtime.v1', 'channel':{'schema':'tilo.installer-code-runtime.v1', 'enabled':False, 'port':None, 'source':None}, 'history':[]}, 'guest_proof': {'installer_code_override_sha256': 'a' * 64},
                                     'source_check': {}}))
        with self.assertRaisesRegex(ValueError, 'cannot authorize a physical install'):
            admission.checked_proof(proof)

    def test_guest_omission_does_not_hide_host_override_or_missing_host_evidence(self):
        admission = load('refresh_host_admission_test', ROOT / 'dev/windows-direct-dev-admission.py')
        proof = self.source / 'host-proof.json'
        for runtime in (None, {'schema': protocol.SCHEMA},
                        {'schema':'tilo.installer-code-runtime.v1', 'channel':{'schema':'tilo.installer-code-runtime.v1',
                         'enabled':True, 'port':2222, 'source':'worktree'}, 'history':[]}):
            proof.write_text(json.dumps({'guest_proof':{},'source_check':{},'installer_code_runtime':runtime}))
            with self.subTest(runtime=runtime), self.assertRaisesRegex(ValueError, 'fresh host evidence'):
                admission.checked_proof(proof)

    def test_crlf_and_forbidden_trust_scripts_are_not_refreshable(self):
        self.file.write_bytes(b'#!/bin/bash\r\necho broken\r\n')
        with self.assertRaisesRegex(ValueError, 'LF line endings'):
            protocol.bundle(self.source, 'a' * 40, False)
        for name in ('tilo-vm-install-proof.py', 'tilo-installer-code-refresh.py',
                     'tilo-installer-code-protocol.py', 'tilo-verified-boot-install-preflight'):
            self.assertNotIn('usr/libexec/' + name, protocol.ALLOWLIST)

    def test_host_closed_window_and_resume_marker_refuse_before_network(self):
        receipt = self.source / 'receipt.json'
        phase = self.source / 'installer-code-phase.json'
        resume = self.source / 'continue-install'
        for state in ({'phase':'closed'}, {'phase':'paused','resume':str(resume)}):
            phase.write_text(json.dumps(state))
            resume.touch()
            with patch.object(host.socket, 'create_connection') as connect:
                with self.assertRaisesRegex(ValueError, 'window is closed'):
                    host.refresh(self.source, 2222, receipt)
                connect.assert_not_called()
            self.assertFalse((self.source / 'installer-code-window.lock').exists())

    def test_host_exclusive_window_blocks_concurrent_refresh(self):
        (self.source / 'installer-code-window.lock').touch()
        with patch.object(host.socket, 'create_connection') as connect:
            with self.assertRaisesRegex(ValueError, 'window is busy'):
                host.refresh(self.source, 2222, self.source / 'receipt.json')
            connect.assert_not_called()


class Guest(unittest.TestCase):
    def setUp(self):
        self.refresh = load('refresh_guest_test', ROOT / 'usr/libexec/tilo-installer-code-refresh.py')

    def test_missing_explicit_vm_devices_refuses_without_privileged_commands(self):
        with patch.object(self.refresh.os, 'geteuid', return_value=0), patch.object(self.refresh, 'command') as command:
            with self.assertRaisesRegex(ValueError, 'opted-in development scenario VM'):
                self.refresh.gate()
            command.assert_not_called()

    def test_binding_preserves_destination_mode_label_and_records_applied_state(self):
        import os
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            destination = root / 'usr/libexec/tilo-install'
            destination.parent.mkdir(parents=True)
            destination.write_bytes(b'original helper')
            destination.chmod(0o755)
            state = root / 'state'
            state.mkdir(mode=0o700)
            descriptor = os.open(state / 'lock', os.O_CREAT | os.O_RDWR, 0o600)
            receipt = {'schema': protocol.SCHEMA, 'sha256': 'a' * 64, 'files': [], 'source': {}}
            calls, labels = [], []
            def command(*argv):
                calls.append(argv)
                if argv[1] == '--bind':
                    source = Path(argv[2])
                    self.assertEqual(source.read_bytes(), b'updated helper')
                    self.assertEqual(source.stat().st_mode & 0o777, 0o755)
                return ''
            with patch.object(self.refresh, 'gate'), patch.object(self.refresh, 'lock', return_value=descriptor), \
                    patch.object(self.refresh, 'BASE', state), patch.object(self.refresh, 'ROOT', root), \
                    patch.object(self.refresh, 'command', side_effect=command), patch.object(self.refresh, 'idle'), \
                    patch.object(self.refresh.os, 'listxattr', return_value=['security.selinux']), \
                    patch.object(self.refresh.os, 'getxattr', return_value=b'original_executable_label'), \
                    patch.object(self.refresh.os, 'setxattr', side_effect=lambda *args: labels.append(args)):
                self.assertEqual(self.refresh.apply({'usr/libexec/tilo-install': b'updated helper'}, receipt), receipt)
            self.assertEqual(labels[0][1:], ('security.selinux', b'original_executable_label'))
            self.assertEqual(calls[-1], ('mount', '-o', 'remount,bind,ro', str(destination)))
            self.assertEqual(json.loads((state / 'active.json').read_text())['state'], 'applied')

    def test_real_backend_flock_refuses_refresh_before_write(self):
        import fcntl
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / 'lock'
            with path.open('wb') as held, path.open('rb') as contender:
                fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
                # apply closes its owned descriptor, so duplicate the test FD.
                import os
                descriptor = os.dup(contender.fileno())
                with patch.object(self.refresh, 'gate'), patch.object(self.refresh, 'lock', return_value=descriptor), \
                        patch.object(self.refresh, 'BASE', root), patch.object(self.refresh, 'command') as command:
                    with self.assertRaisesRegex(ValueError, 'backend is running'):
                        self.refresh.apply({}, {'sha256': 'a' * 64})
                    command.assert_not_called()
                    self.assertFalse((root / 'active.json').exists())

    def test_transient_backend_owns_lock_after_launcher_reexec(self):
        import subprocess
        text = (ROOT / 'usr/libexec/tilo-install').read_text(encoding='utf-8')
        start = text.index('# Only the transient backend owns this lock.')
        end = text.index('# --- parse args', start)
        self.assertGreater(start, text.index('if [ -z "${TILO_INSTALL_TRANSIENT:-}" ]; then'))
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory) / 'code-refresh'
            state.mkdir(mode=0o700)
            block = text[start:end].replace('/run/tilo-installer/code-refresh', str(state))
            script = Path(directory) / 'launcher.sh'
            script.write_text('#!/bin/bash\nset -euo pipefail\n'
                              'if [ -z "${TILO_INSTALL_TRANSIENT:-}" ]; then\n'
                              ' TILO_INSTALL_TRANSIENT=1 bash "$0"; exit $?\nfi\n'
                              + block + 'echo backend-ran\n')
            result = subprocess.run(['bash', str(script)], capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, 'backend-ran\n')
            (state / 'active.json').write_text('{"state":"applying"}')
            result = subprocess.run(['bash', str(script)], capture_output=True, text=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn('backend-ran', result.stdout)

    def test_old_backend_independent_unit_and_process_checks(self):
        with patch.object(self.refresh, 'command', return_value='tilo-install-123.service loaded activating start task'):
            with self.assertRaisesRegex(ValueError, 'transient unit is running'):
                self.refresh.idle()
        with tempfile.TemporaryDirectory() as directory:
            process = Path(directory) / '123'
            process.mkdir()
            (process / 'cmdline').write_bytes(b'/bin/bash\0/usr/libexec/tilo-install\0--disk\0/dev/test')
            original = self.refresh.Path
            def rooted(path):
                return Path(directory) if path == '/proc' else original(path)
            with patch.object(self.refresh, 'command', return_value=''), patch.object(self.refresh, 'Path', side_effect=rooted):
                with self.assertRaisesRegex(ValueError, 'helper process is running'):
                    self.refresh.idle()

    def test_failed_second_mount_rolls_back_new_binds_and_keeps_old_receipt(self):
        import os
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); state = root / 'state'; state.mkdir(mode=0o700)
            files = {'usr/libexec/tilo-install': b'first', 'usr/libexec/tilo-install-dev-ssh.py': b'second'}
            for relative in files:
                path = root / relative; path.parent.mkdir(parents=True, exist_ok=True); path.write_bytes(b'old')
            previous = {'sha256':'b'*64, 'state':'applied'}
            (state / 'active.json').write_text(json.dumps(previous))
            calls = []
            def command(*argv):
                calls.append(argv)
                if argv[:2] == ('mount', '--bind') and argv[-1].endswith('tilo-install-dev-ssh.py'):
                    raise ValueError('simulated mount failure')
                return ''
            descriptor = os.open(state / 'lock', os.O_CREAT | os.O_RDWR, 0o600)
            with patch.object(self.refresh, 'gate'), patch.object(self.refresh, 'idle'), \
                    patch.object(self.refresh, 'lock', return_value=descriptor), \
                    patch.object(self.refresh, 'BASE', state), patch.object(self.refresh, 'ROOT', root), \
                    patch.object(self.refresh, 'command', side_effect=command):
                with self.assertRaisesRegex(ValueError, 'simulated mount failure'):
                    self.refresh.apply(files, {'sha256':'a'*64})
            self.assertEqual(calls[-1], ('umount', str(root / 'usr/libexec/tilo-install')))
            self.assertEqual(json.loads((state / 'active.json').read_text()), previous)

    def test_failed_rollback_stops_greeter_and_keeps_incomplete_marker(self):
        import os
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); state = root / 'state'; state.mkdir(mode=0o700)
            target = root / 'usr/libexec/tilo-install'; target.parent.mkdir(parents=True); target.write_bytes(b'old')
            descriptor = os.open(state / 'lock', os.O_CREAT | os.O_RDWR, 0o600)
            calls = []
            def command(*argv):
                calls.append(argv)
                if argv[0] == 'umount' or argv[:2] == ('mount', '-o'):
                    raise ValueError('simulated transaction failure')
                return ''
            with patch.object(self.refresh, 'gate'), patch.object(self.refresh, 'idle'), \
                    patch.object(self.refresh, 'lock', return_value=descriptor), \
                    patch.object(self.refresh, 'BASE', state), patch.object(self.refresh, 'ROOT', root), \
                    patch.object(self.refresh, 'command', side_effect=command):
                with self.assertRaisesRegex(ValueError, 'transaction failure'):
                    self.refresh.apply({'usr/libexec/tilo-install': b'new'}, {'sha256':'a'*64})
            self.assertEqual(calls[-1], ('systemctl', 'stop', 'greetd.service'))
            self.assertEqual(json.loads((state / 'active.json').read_text())['state'], 'applying')

    def test_readiness_only_after_successful_first_application(self):
        from contextlib import nullcontext
        with patch.object(self.refresh, 'gate'), patch.object(self.refresh, 'PORT') as port, \
                patch.object(self.refresh.protocol, 'receive', return_value=({}, {})), \
                patch.object(self.refresh.protocol, 'write_all'), \
                patch.object(self.refresh, 'apply', side_effect=[ValueError('busy'), {}]), \
                patch.object(self.refresh, 'ready') as ready, \
                patch.object(self.refresh.time, 'sleep', side_effect=[None, RuntimeError('end test')]):
            port.open.side_effect = lambda *args, **kwargs: nullcontext(io.BytesIO())
            with self.assertRaisesRegex(RuntimeError, 'end test'):
                self.refresh.main()
            ready.assert_called_once()


if __name__ == '__main__':
    unittest.main()

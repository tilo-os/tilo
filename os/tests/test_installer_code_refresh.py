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
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

SEED = ROOT / 'dev/installer-code-bootstrap'
protocol = load('refresh_protocol_test', SEED / 'tilo-installer-code-protocol.py')
host = load('refresh_host_test', ROOT / 'dev/refresh-installer-code.py')


console = load('installer_console_test', ROOT / 'dev/installer-console.py')


class ConsoleTransport(unittest.TestCase):
    def test_nonobject_frames_do_not_hide_the_matching_reply(self):
        client, server = socket.socketpair()
        self.addCleanup(client.close)
        self.addCleanup(server.close)
        def serve():
            with server.makefile('rb') as stream:
                seq = json.loads(stream.readline())['seq']
            server.sendall(b'null\n[]\n42\n'
                + json.dumps({'seq': 'old', 'state': {}}).encode() + b'\n'
                + json.dumps({'seq': seq, 'state': {'app': 'installer'}}).encode() + b'\n')
        worker = threading.Thread(target=serve)
        worker.start()
        try:
            with patch.object(console.socket, 'create_connection', return_value=client):
                self.assertEqual(console.state('tcp://127.0.0.1:12345'), {'app': 'installer'})
        finally:
            client.close()
            worker.join(timeout=2)
        self.assertFalse(worker.is_alive())

    def test_malformed_state_is_retryable_not_an_attribute_error(self):
        for state in (None, [], 'booting'):
            with self.subTest(state=state), patch.object(console, 'request', return_value={'state': state}):
                with self.assertRaises(console.Unreachable):
                    console.state('fixture')

    def test_failed_connect_closes_the_socket(self):
        client = Mock()
        client.connect.side_effect = OSError('not ready')
        with patch.object(console.socket, 'socket', return_value=client):
            with self.assertRaises(console.Unreachable):
                console.request('fixture', {'op': 'state_dump'})
        client.close.assert_called_once()

    def test_unterminated_response_is_bounded(self):
        client = Mock()
        client.__enter__ = Mock(return_value=client)
        client.__exit__ = Mock(return_value=False)
        client.recv.side_effect = [b'x' * 65536] * 33 + [b'']
        with patch.object(console.socket, 'socket', return_value=client):
            with self.assertRaisesRegex(console.Unreachable, 'too large'):
                console.request('fixture', {'op': 'state_dump'})
        self.assertLess(client.recv.call_count, 33)

    def test_read_timeout_tracks_the_remaining_request_budget(self):
        now = [0.0]
        client = Mock()
        client.__enter__ = Mock(return_value=client)
        client.__exit__ = Mock(return_value=False)
        budget = [1.0]
        client.settimeout.side_effect = lambda seconds: budget.__setitem__(0, seconds)
        def receive(_):
            now[0] += min(0.6, budget[0])
            if budget[0] < 0.6:
                raise socket.timeout('read timed out')
            return b'old boot output\n'
        client.recv.side_effect = receive
        with patch.object(console.socket, 'socket', return_value=client), \
                patch.object(console.time, 'monotonic', side_effect=lambda: now[0]):
            with self.assertRaises(console.Unreachable):
                console.request('fixture', {'op': 'state_dump'}, timeout=1.0)
        self.assertLessEqual(now[0], 1.01)

    def test_poll_budget_includes_requests_and_retry_delay(self):
        now = [0.0]
        def unavailable(path, payload, timeout=15):
            now[0] += timeout
            raise console.Unreachable('booting')
        def sleep(seconds):
            now[0] += seconds
        with patch.object(console, 'request', side_effect=unavailable), \
                patch.object(console.time, 'monotonic', side_effect=lambda: now[0]), \
                patch.object(console.time, 'sleep', side_effect=sleep):
            with self.assertRaises(TimeoutError):
                console.poll('fixture', lambda s: True, timeout=0.25)
        self.assertLessEqual(now[0], 0.251)


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

    def test_canonical_profile_source_maps_only_to_existing_runtime_module(self):
        module = self.source/'os/dev/hardware_profile.py'
        module.parent.mkdir(parents=True)
        module.write_bytes(b'canonical profile implementation\n')
        files, _ = protocol.validate(protocol.bundle(self.source, 'a'*40, True))
        self.assertEqual(files['usr/lib/tilo/hardware_profile.py'], module.read_bytes())
        self.assertNotIn('usr/lib/tilo/uki_hardware_profile.py', protocol.ALLOWLIST)

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
                runtime={'schema':'tilo.installer-code-runtime-probe.v1','backend':{'cached_target':{'mount':{
                    'uki_fixture':{'sha256':'b'*64,'bytes':1024*1024,'physical_admission':False}}}}}
                protocol.write_all(stream, protocol.canonical({'ok': True, 'receipt': receipt, 'runtime':runtime}) + b'\n')
        worker = threading.Thread(target=server, daemon=True)
        worker.start()
        def git(argv, **kwargs):
            class Result:
                stdout = 'a' * 40 + '\n' if 'rev-parse' in argv else b' M os/usr/libexec/tilo-install'
            return Result()
        (self.source / 'installer-code-phase.json').write_text(json.dumps({'phase':'startup'}))
        (self.source / 'installer-code-channel.json').write_text(json.dumps({'schema':'tilo.installer-code-runtime.v1', 'enabled':True, 'port':port, 'source':str(self.source)}))
        (self.source/'installer-code-uki-fixture.json').write_text(json.dumps({
            'schema':'tilo.installer-code-uki-fixture.v1','physical_admission':False,'readonly':True,
            'sha256':'b'*64,'bytes':1024*1024}))
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
                     'tilo-installer-code-protocol.py', 'tilo-installer-code-target-probe.py',
                     'tilo-verified-boot-install-preflight'):
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


class SeedDaemon(unittest.TestCase):
    """The Python daemon only old media run, from the bootstrap seed."""

    def setUp(self):
        self.refresh = load('refresh_guest_test', SEED / 'tilo-installer-code-refresh.py')

    def test_bootstrap_transport_resynchronizes_bounded_early_and_repeated_hellos(self):
        raw = protocol.canonical({'schema':protocol.SCHEMA, 'source':{'revision':'a'*40,'dirty':True},
            'files':[{'path':'usr/libexec/tilo-install','sha256':hashlib.sha256(b'helper\n').hexdigest(),
                      'data':base64.b64encode(b'helper\n').decode()}]})
        class Duplex:
            def __init__(self, data): self.reader=io.BytesIO(data); self.writer=io.BytesIO()
            def readline(self, size): return self.reader.readline(size)
            def read(self, size): return self.reader.read(min(size,3))
            def write(self, data): return self.writer.write(data)
            def flush(self): pass
        stream = Duplex(b'LO-1\n'+b'TILO-CODE-HELLO-1\n'*20+struct.pack('!I',len(raw))+raw)
        files, _ = self.refresh.bootstrap_receive(stream)
        self.assertEqual(files['usr/libexec/tilo-install'], b'helper\n')
        self.assertEqual(stream.writer.getvalue().count(b'installer-code-ready'),20)
        with self.assertRaisesRegex(ValueError, 'too many repeated'):
            self.refresh.bootstrap_receive(Duplex(b'TILO-CODE-HELLO-1\n'*66))


class Bootstrap(unittest.TestCase):
    def test_promotion_receipt_masks_only_cmdline_payload_and_virtual_size(self):
        probe=load('promotion_pe_identity',ROOT/'dev/installer-code-bootstrap/tilo-installer-code-target-probe.py')
        raw=bytearray(512); raw[:2]=b'MZ'; struct.pack_into('<I',raw,60,64); raw[64:68]=b'PE\0\0'
        struct.pack_into('<H',raw,70,1); raw[88:96]=b'.cmdline'; struct.pack_into('<I',raw,96,4)
        struct.pack_into('<II',raw,104,64,256); raw[256:260]=b'old\0'
        changed=bytearray(raw); changed[256:260]=b'new\0'; struct.pack_into('<I',changed,96,3)
        self.assertEqual(probe.non_cmdline_bytes(raw),probe.non_cmdline_bytes(changed))
        changed[200]=1
        self.assertNotEqual(probe.non_cmdline_bytes(raw),probe.non_cmdline_bytes(changed))
        struct.pack_into('<I',changed,108,500)
        with self.assertRaisesRegex(ValueError,'bounds'): probe.non_cmdline_bytes(changed)

    def test_promotion_metadata_is_bounded_before_archive_or_scratch_writes(self):
        builder=load('bounded_promotion_builder',ROOT/'dev/make-installer-probe-fixture.py')
        probe=load('bounded_promotion_probe',ROOT/'dev/installer-code-bootstrap/tilo-installer-code-target-probe.py')
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary); (root/'input').write_bytes(b'kernel')
            for metadata in ([], {'composefs_digest':'a'*128,'origin':'x'*16385,
                                  'loader':'entry','update_origin':'registry/test'}):
                with self.assertRaisesRegex(ValueError,'invalid promotion fixture'):
                    builder.create(root,root/'invalid.tar','sha256:'+'a'*64,metadata)
                self.assertFalse((root/'invalid.tar').exists())
            with self.assertRaisesRegex(ValueError,'invalid scratch promotion fixture'):
                probe.promotion_probe(None,root,{'promotion':[]},'7')
            self.assertFalse((root/'promotion-target').exists())

    def test_original_ci_boot_fixture_validates_real_archive_and_refuses_tampering(self):
        from types import SimpleNamespace
        probe=load('original_boot_fixture',ROOT/'dev/installer-code-bootstrap/tilo-installer-code-target-probe.py')
        builder=load('boot_fixture_builder',ROOT/'dev/make-installer-probe-fixture.py')
        with tempfile.TemporaryDirectory() as temporary:
            base=Path(temporary); root=base/'inputs'; root.mkdir()
            for name,data in {'kernel/7/vmlinuz':b'kernel','kernel/7/initramfs.img':b'initrd',
                              'usr/lib/tilo/hardware-profile.json':b'{"id":"lenovo-83ed"}',
                              'padding':b'x'*(1024*1024)}.items():
                path=root/name; path.parent.mkdir(parents=True,exist_ok=True); path.write_bytes(data)
            archive=base/'inputs.tar'; builder.create(root,archive,'sha256:'+'a'*64)
            sysblock=base/'sys'; (sysblock/archive.name).mkdir(parents=True)
            ro=sysblock/archive.name/'ro'; ro.write_text('1')
            def validate(projected):
                self.assertEqual((projected/'kernel/7/vmlinuz').read_bytes(),b'kernel')
                return '7',projected/'kernel/7/vmlinuz',projected/'kernel/7/initramfs.img',None
            helper=SimpleNamespace(image_inputs=validate)
            with patch.object(probe,'FIXTURE_DEVICE',archive), patch.object(probe,'SYS_BLOCK',sysblock), patch.object(probe,'SCRATCH_ROOT',base), \
                    patch.object(Path,'is_block_device',return_value=True), \
                    patch.object(probe,'run',return_value=str(archive.stat().st_size)), \
                    patch.object(probe,'HELPER',root/'kernel/7/vmlinuz'):
                result=probe.fixture_probe(helper)
                self.assertEqual(result['profile'],'lenovo-83ed')
                self.assertEqual(result['mount']['uki_fixture']['sha256'],hashlib.sha256(archive.read_bytes()).hexdigest())
                ro.write_text('0')
                with self.assertRaisesRegex(ValueError,'writable'): probe.fixture_probe(helper)
                ro.write_text('1')
                data=archive.read_bytes(); archive.write_bytes(data.replace(b'kernel',b'BROKEN',1))
                with self.assertRaisesRegex(ValueError,'differ'): probe.fixture_probe(helper)
            (root/'symlink').symlink_to(root/'kernel/7/vmlinuz')
            with self.assertRaisesRegex(ValueError,'symlinks'): builder.create(root,base/'bad.tar','sha256:'+'a'*64)

    def test_seed_validation_precedes_any_bind_and_exec_uses_frozen_daemon(self):
        from types import SimpleNamespace
        bootstrap = load('refresh_bootstrap_test', ROOT / 'dev/installer-code-bootstrap/tilo-installer-code-bootstrap.py')
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            files = ('tilo-installer-code-bootstrap.py', 'tilo-installer-code-refresh.py',
                     'tilo-installer-code-protocol.py', 'tilo-installer-code-target-probe.py', 'tilo-vm-install-proof.py')
            for name in files: (root / name).write_bytes(name.encode())
            receipt = {'schema':'tilo.installer-code-bootstrap.v1',
                       'files':{name:hashlib.sha256((root/name).read_bytes()).hexdigest() for name in files}}
            (root/'receipt.json').write_text(json.dumps(receipt))
            destination = root/'original-proof'; destination.write_bytes(b'original')
            daemon = SimpleNamespace(gate=lambda:None, BASE=root/'runtime')
            original_stat = Path.stat
            def root_owned(path, *args, **kwargs):
                fields = list(original_stat(path, *args, **kwargs)); fields[4] = 0
                return os.stat_result(fields)
            with patch.object(bootstrap, 'ROOT', root), patch.object(bootstrap, 'DESTINATION', destination), \
                    patch.dict(os.environ, {}, clear=True), \
                    patch.object(Path, 'stat', root_owned), \
                    patch.object(bootstrap, 'load_daemon', return_value=daemon), \
                    patch.object(bootstrap.os, 'listxattr', return_value=['security.selinux']), \
                    patch.object(bootstrap.os, 'getxattr', return_value=b'system_u:object_r:bin_t:s0'), \
                    patch.object(bootstrap.os, 'setxattr'), patch.object(bootstrap, 'run') as run, \
                    patch.object(bootstrap.os, 'execv', side_effect=RuntimeError('exec observed')) as execute:
                (root/files[0]).write_bytes(b'tampered')
                with self.assertRaisesRegex(ValueError, 'differs from host manifest'): bootstrap.start()
                run.assert_not_called()
                (root/files[0]).write_bytes(files[0].encode())
                with self.assertRaisesRegex(RuntimeError, 'exec observed'): bootstrap.start()
                execute.assert_called_once_with('/usr/bin/python3',
                    ['/usr/bin/python3', str(root/'tilo-installer-code-refresh.py')])
                self.assertEqual(run.call_args_list[0].args, ('mount', '--bind', str(root/files[-1]), str(destination)))
                self.assertEqual(json.loads((daemon.BASE/'bootstrap.json').read_text()), receipt)


if __name__ == '__main__':
    unittest.main()

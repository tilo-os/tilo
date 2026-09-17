"""Small archive streams prove hidden-layer scanning without image builds."""
import gzip
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import time
import unittest
from unittest.mock import MagicMock, patch

spec = importlib.util.spec_from_file_location('audit', Path(__file__).resolve().parents[1] / 'dev/audit-rootfs-layers.py')
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)
SECRET = b'private-sentinel-that-must-never-be-logged'


def tar(entries, format=tarfile.PAX_FORMAT):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode='w', format=format) as archive:
        for name, value in entries:
            member = tarfile.TarInfo(name)
            member.size = len(value)
            archive.addfile(member, io.BytesIO(value))
    return output.getvalue()


def docker(layers, extra=None, config_change=None):
    config = {'architecture': 'amd64', 'os': 'linux',
              'rootfs': {'type': 'layers', 'diff_ids': ['sha256:' + hashlib.sha256(layer).hexdigest() for layer in layers]}}
    config.update(config_change or {})
    raw = json.dumps(config).encode()
    digest = hashlib.sha256(raw).hexdigest()
    names = [f'{index:064x}/layer.tar' for index in range(len(layers))]
    manifest = [{'Config': digest + '.json', 'RepoTags': [], 'Layers': names}]
    entries = list(zip(names, layers)) + [(digest + '.json', raw), ('manifest.json', json.dumps(manifest).encode())]
    entries.extend(extra or [])
    return tar(entries), 'sha256:' + digest


class LayerAuditTests(unittest.TestCase):
    def test_create_then_delete_secret_is_detected_in_earlier_layer(self):
        stream, image = docker([tar([('private.key', SECRET)]), tar([('.wh.private.key', b'')])])
        result = audit.audit_stream(io.BytesIO(stream), image, [SECRET])
        self.assertEqual(result['status'], 'rejected-private-material')
        self.assertEqual(result['layers'][0]['findings'], ['provided-private-material'])
        self.assertEqual(result['layers'][1]['findings'], [])
        self.assertNotIn(SECRET.decode(), json.dumps(result))

    def test_boundary_match_and_private_envelope_public_key_allowed(self):
        for value, findings in ((b'-----BEGIN PUBLIC KEY-----\npublic\n-----END PUBLIC KEY-----', []),
                                (b'-----BEGIN ENCRYPTED PRIVATE KEY-----\nopaque', ['private-pem-envelope']),
                                (b'x' * 499 + SECRET, ['provided-private-material'])):
            stream, image = docker([tar([('file', value)])])
            with patch.object(audit, 'CHUNK', 512):
                result = audit.audit_stream(io.BytesIO(stream), image, [SECRET])
            self.assertEqual(result['layers'][0]['findings'], findings)

    def test_secrets_in_configuration_are_not_missed(self):
        stream, image = docker([tar([('public', b'ok')])], config_change={'config': {'Env': ['VALUE=' + SECRET.decode()]}})
        result = audit.audit_stream(io.BytesIO(stream), image, [SECRET])
        self.assertEqual(result['metadata_findings'], ['provided-private-material'])
        self.assertEqual(result['status'], 'rejected-private-material')

    def test_pax_long_names_are_streamed_and_scanned(self):
        stream, image = docker([tar([('a/' * 100 + 'file', SECRET)])])
        result = audit.audit_stream(io.BytesIO(stream), image, [SECRET])
        self.assertEqual(result['status'], 'rejected-private-material')

    def test_compressed_truncated_misbound_or_oversized_archives_rejected(self):
        layer = tar([('file', b'ok')])
        good, image = docker([layer])
        bad, compressed_id = docker([gzip.compress(layer)])
        for stream, identity, budget in ((bad, compressed_id, audit.MAX_BYTES),
                                        (good[:100], image, audit.MAX_BYTES),
                                        (good, 'sha256:' + 'f' * 64, audit.MAX_BYTES),
                                        (good, image, 512),
                                        (good + b'hidden', image, audit.MAX_BYTES)):
            with self.subTest(length=len(stream), budget=budget), self.assertRaises(audit.AuditError):
                audit.audit_stream(io.BytesIO(stream), identity, max_bytes=budget)

    def test_manifest_layer_and_duplicate_metadata_rejected(self):
        layer = tar([('file', b'ok')])
        for extra, change in (([('manifest.json', b'[]')], None),
                              (None, {'rootfs': {'type': 'layers', 'diff_ids': ['sha256:' + 'f' * 64]}})):
            stream, image = docker([layer], extra, change)
            with self.assertRaises(audit.AuditError):
                audit.audit_stream(io.BytesIO(stream), image)

    def test_oversized_metadata_is_rejected_before_reading_payload(self):
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode='w') as archive:
            member = tarfile.TarInfo('large-pax')
            member.type = b'x'
            member.size = 65537
            archive.addfile(member, io.BytesIO(b'x' * member.size))
        stream, image = docker([output.getvalue()])
        with self.assertRaisesRegex(audit.AuditError, 'oversized tar metadata'):
            audit.audit_stream(io.BytesIO(stream), image)

    def test_export_error_kills_only_owned_process_group_and_hides_stderr(self):
        child = MagicMock(pid=12345, stdout=io.BytesIO(b'invalid'))
        child.poll.return_value = None
        child.wait.return_value = -9
        with patch.object(audit.subprocess, 'Popen', return_value=child) as launch, \
             patch.object(audit.os, 'killpg', create=True) as kill:
            with self.assertRaises(audit.AuditError):
                audit.audit_image('sha256:' + 'a' * 64)
        self.assertTrue(launch.call_args.kwargs['start_new_session'])
        self.assertEqual(launch.call_args.kwargs['stderr'], audit.subprocess.DEVNULL)
        self.assertNotIn('--output', launch.call_args.args[0])
        kill.assert_called_once_with(12345, audit.signal.SIGKILL)
        child.wait.assert_called_once_with(timeout=10)

    @unittest.skipUnless(hasattr(os, 'killpg'), 'process-group deadline requires POSIX')
    def test_real_stalled_export_is_terminated_at_deadline(self):
        original = subprocess.Popen
        def stalled(*args, **kwargs):
            return original([sys.executable, '-c', 'import time; time.sleep(60)'], **kwargs)
        started = time.monotonic()
        with patch.object(audit.subprocess, 'Popen', side_effect=stalled):
            with self.assertRaises(audit.AuditError):
                audit.audit_image('sha256:' + 'a' * 64, timeout=1)
        self.assertLess(time.monotonic() - started, 5)


if __name__ == '__main__':
    unittest.main()

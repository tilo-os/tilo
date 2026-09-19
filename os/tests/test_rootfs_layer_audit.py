"""Small archive streams prove hidden-layer scanning without image builds."""
import gzip
from contextlib import redirect_stderr, redirect_stdout
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
            member = name if isinstance(name, tarfile.TarInfo) else tarfile.TarInfo(name)
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
    def podman_archive(self, target=None, alias_name=None, duplicate=False, manifest_alias=False, wrong_digest=False):
        layer = tar([('private', SECRET)])
        original, image = docker([layer])
        canonical = hashlib.sha256(layer).hexdigest() + '.tar'
        if wrong_digest:
            canonical = 'f' * 64 + '.tar'
        with tarfile.open(fileobj=io.BytesIO(original)) as archive:
            config = archive.extractfile(image[7:] + '.json').read()
        alias = tarfile.TarInfo(alias_name or 'a' * 64 + '/layer.tar')
        alias.type = tarfile.SYMTYPE
        alias.linkname = target if target is not None else '../' + canonical
        entries = [(canonical, layer), (alias, b'')]
        if duplicate:
            other = tarfile.TarInfo('b' * 64 + '/layer.tar')
            other.type, other.linkname = alias.type, alias.linkname
            entries.append((other, b''))
        manifest = [{'Config': image[7:] + '.json', 'Layers': [alias.name if manifest_alias else canonical]}]
        entries.extend([(image[7:] + '.json', config), ('manifest.json', json.dumps(manifest).encode())])
        return tar(entries), image

    def test_podman_canonical_layers_and_legacy_aliases_scan_actual_bytes(self):
        for manifest_alias in (False, True):
            stream, image = self.podman_archive(manifest_alias=manifest_alias)
            result = audit.audit_stream(io.BytesIO(stream), image, [SECRET])
            self.assertEqual(result['status'], 'rejected-private-material')
            self.assertEqual(len(result['layers']), 1)
            self.assertEqual(result['layers'][0]['findings'], ['provided-private-material'])

    def test_podman_aliases_reject_escape_unknown_and_digest_mismatch(self):
        for options in ({'target': '../../' + 'a' * 64 + '.tar'},
                        {'target': '../' + 'a' * 64 + '.tar'},
                        {'target': '/absolute.tar'},
                        {'target': '../x/layer.tar'},
                        {'alias_name': 'unexpected-link'}, {'wrong_digest': True}):
            with self.subTest(options=options):
                stream, image = self.podman_archive(**options)
                with self.assertRaises(audit.AuditError):
                    audit.audit_stream(io.BytesIO(stream), image, [SECRET])

    def repeated_archive(self, aliases=False, order=None, unreferenced=False):
        # The completed ARM image repeats this exact 1024-byte empty delta.
        layers = [tar([('public', b'ok')]), bytes(1024), tar([('private', SECRET)])]
        names = [hashlib.sha256(layer).hexdigest() + '.tar' for layer in layers]
        sequence = [0, 1, 2, 1, 1]
        config = json.dumps({'rootfs': {'type': 'layers',
            'diff_ids': ['sha256:' + names[index][:-4] for index in sequence]}}).encode()
        image = 'sha256:' + hashlib.sha256(config).hexdigest()
        entries = list(zip(names, layers))
        alias_names = []
        for position, index in enumerate(sequence):
            alias = tarfile.TarInfo(f'{position:064x}/layer.tar')
            alias.type, alias.linkname = tarfile.SYMTYPE, '../' + names[index]
            entries.append((alias, b'')); alias_names.append(alias.name)
        if unreferenced:
            extra = tar([('unreferenced', b'payload')])
            entries.append((hashlib.sha256(extra).hexdigest() + '.tar', extra))
        selected = [alias_names[position] if aliases else names[index]
                    for position, index in enumerate(sequence)]
        if order is not None:
            selected = [selected[position] for position in order]
        entries.extend([(image[7:] + '.json', config),
                        ('manifest.json', json.dumps([{'Config': image[7:] + '.json',
                                                       'Layers': selected}]).encode())])
        return tar(entries), image, ['sha256:' + names[index][:-4] for index in sequence]

    def test_repeated_layer_occurrences_keep_order_and_scan_unique_payloads_once(self):
        for aliases in (False, True):
            with self.subTest(aliases=aliases):
                stream, image, ordered = self.repeated_archive(aliases=aliases)
                with patch.object(audit, 'walk_tar', wraps=audit.walk_tar) as walker:
                    result = audit.audit_stream(io.BytesIO(stream), image, [SECRET])
                self.assertEqual(walker.call_count, 4)  # Outer archive + three payloads.
                self.assertEqual(result['unique_layers_scanned'], 3)
                self.assertEqual([row['diff_id'] for row in result['layers']], ordered)
                self.assertEqual(result['status'], 'rejected-private-material')
                self.assertEqual(result['layers'][2]['findings'], ['provided-private-material'])
                self.assertTrue(all(not result['layers'][index]['findings'] for index in (0, 1, 3, 4)))
                self.assertNotIn(SECRET.decode(), json.dumps(result))

    def test_repeated_layers_still_require_exact_order_multiplicity_and_coverage(self):
        for options in ({'order': [1, 0, 2, 3, 4]}, {'order': [0, 1, 2, 3]},
                        {'order': [0, 1, 2, 3, 4, 4]}, {'unreferenced': True}):
            with self.subTest(options=options):
                stream, image, _ = self.repeated_archive(**options)
                with self.assertRaisesRegex(audit.AuditError, 'identity mismatch'):
                    audit.audit_stream(io.BytesIO(stream), image)

    def test_repeated_occurrences_still_obey_layer_count_bound(self):
        stream, image, _ = self.repeated_archive()
        with patch.object(audit, 'MAX_LAYERS', 4):
            with self.assertRaisesRegex(audit.AuditError, 'too many layer occurrences'):
                audit.audit_stream(io.BytesIO(stream), image)

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

    def test_scope_excludes_outer_archive_names_not_part_of_image_payload(self):
        stream, image = docker([tar([('public', b'ok')])], extra=[(SECRET.decode(), b'')])
        result = audit.audit_stream(io.BytesIO(stream), image, [SECRET])
        self.assertEqual(result['status'], 'passed-scoped-scan')
        self.assertEqual(result['scope'], 'all uncompressed layer bytes and archive metadata file contents: '
                         'provided exact private bytes and private PEM envelopes only')
        self.assertFalse(result['universal_secret_absence_proven'])

    def test_credential_file_newlines_do_not_hide_shell_exported_value(self):
        for content in (SECRET, SECRET + b'\r\n'):
            stream, image = docker([tar([('environment', content)])])
            result = audit.audit_stream(io.BytesIO(stream), image, [SECRET + b'\r\n'])
            self.assertEqual(result['layers'][0]['findings'], ['provided-private-material'])

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

    def test_cli_parser_failure_emits_safe_public_receipt_and_cleans_owned_export(self):
        image = 'sha256:' + 'a' * 64
        child = MagicMock(pid=12345, stdout=io.BytesIO(b'invalid'))
        child.poll.return_value = None
        child.wait.return_value = -9
        output, diagnostics = io.StringIO(), io.StringIO()
        with patch.object(sys, 'argv', ['audit', '--image-id', image]), \
             patch.object(audit.subprocess, 'Popen', return_value=child), \
             patch.object(audit.os, 'killpg', create=True) as kill, \
             redirect_stdout(output), redirect_stderr(diagnostics):
            code = audit.main()
        result = json.loads(output.getvalue())
        self.assertEqual(code, 1)
        self.assertEqual(result['image_id'], image)
        self.assertEqual(result['status'], 'failed-scoped-scan')
        self.assertEqual(result['failure_reason'], 'truncated-archive')
        self.assertEqual(result['failed_stage'], 'layer-export-and-scan')
        self.assertFalse(result['installable'])
        self.assertFalse(result['universal_secret_absence_proven'])
        self.assertEqual(diagnostics.getvalue(), '')
        kill.assert_called_once_with(12345, audit.signal.SIGKILL)

    def test_cli_failure_receipts_never_copy_private_paths_or_exception_diagnostics(self):
        image = 'sha256:' + 'a' * 64
        for error, reason in ((OSError(SECRET.decode()), 'io-error'),
                              (audit.AuditError(SECRET.decode()), 'unclassified-audit-error')):
            with self.subTest(reason=reason):
                output, diagnostics = io.StringIO(), io.StringIO()
                with patch.object(sys, 'argv', ['audit', '--image-id', image,
                                               '--private-file', SECRET.decode()]), \
                     patch.object(audit, 'load_private', side_effect=error), \
                     patch.object(audit, 'audit_image') as scan, \
                     redirect_stdout(output), redirect_stderr(diagnostics):
                    self.assertEqual(audit.main(), 1)
                result = json.loads(output.getvalue())
                self.assertEqual(result['failure_reason'], reason)
                self.assertEqual(result['failed_stage'], 'private-comparison-input')
                self.assertEqual(result['status'], 'failed-scoped-scan')
                scan.assert_not_called()
                self.assertNotIn(SECRET.decode(), output.getvalue() + diagnostics.getvalue())

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

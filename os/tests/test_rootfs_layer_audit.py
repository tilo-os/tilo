"""Small archive streams prove hidden-layer scanning without image builds."""
import base64
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
# A complete PKCS#8 Ed25519 test key, with a public, deterministic test seed.
PRIVATE_DER = bytes.fromhex('302e020100300506032b657004220420') + bytes(range(32))
# Exact public Ed25519 vector from the signed GnuTLS RPM pinned by the scanner.
PUBLIC_VECTOR_DER = bytes.fromhex(
    '302e020100300506032b6570042204207e167aa999199425498ffc1fce59662995776ea1e88ebc80537206fca1dd5732')

# Public secp192r1/secp224r1 self-test vectors from the signed GnuTLS source RPM.
LEGACY_PUBLIC_EC_DER = (
    bytes.fromhex(
        '3068020101041ce29624575676b47f0a056b51e4de6b03ab27d1a8cc4f4a2b1d9af0f9a00706'
        '052b81040021a13c033a00040a423dd8a5e9d64fb945478d529c1be68fff818fe920cd09da17'
        'ae6a580b4f449b195c594b4eedff21436a491bbd0ea3ec93cc411c007ba4'
    ),
    bytes.fromhex(
        '305f0201010418b8decc571b80331e02956b76d647beefe4d5af3fb55678d4a00a06082a8648'
        'ce3d030101a13403320004ed6572229d8317911fc93ae2fc5c695d4e6082bd6a456b5b4d83c3'
        '6d4c5e144b3f4974c330b0e8b192af9ad2e8068b16'
    ),
)

# Exact public encrypted example from the signed perl-Net-SSLeay RPM.
ENCRYPTED_PUBLIC_HEADERS = b'Proc-Type: 4,ENCRYPTED\nDEK-Info: DES-EDE3-CBC,932256E9ACCDC996\n\n'
ENCRYPTED_PUBLIC_CIPHERTEXT = base64.b64decode(
    'wav+t55gSrsCnvgeikRqcQKXuUeXEqEcL6zwNsTMIis6rpGuXkBYH5Ezntm6h/5h2vZDu2vTOeq7'
    'QbNmTbKWbU6Rw0e7hlnWcPFhg3ssLNrh0XW3ynZ56gLCj/TRHj9V4c7UnozeDTb+XYEAp+1Yj21B'
    'pepectak6j4dZfhhJteEGWMDj9faNTbA08LX87WnJiNhlhIWl7ncS/13riwLqsemHcTFlNCaoANa'
    'VCUKEGoIFZmz1OoYJvBWLj7HnxDU+Z/lvf3o1glE1EP3XcCACGhapfZcALlYhz1qGTA6ProZ/sIJ'
    'GLAYAi8uobJlA1E0SwGXNm4rtMocR7utO649/g92SFvGZmG715t2P8jBa4MqgV5H3oIIVJK1YiL6'
    'Zmk5jZMPmTDctXD61dLnTFXa+m0vLJMZAMD8UkhtbuCMYzTy0dIHBaxTYNxggSbfbbKJ6QijHZHn'
    '+iygyudCxYWXtZQ1uyBFzU4wpEGqugngf3T0W2TWpiM3PQjh+vymxKRZUVY5LGejMuz3HpgFcJ+H'
    's6DCJYAECUWWjSPhTXvTifNIZq5XdaeCveyLkMnCHQ7yLW64XcZ9jWzI+FOqRmLMYcUoHh8eZyB6'
    'X6ITC3wy2g1ovc5KrD+OuC+HOyLvvb+8avnSoc1+maBC7su7htE/EPP6X/zvNzLM4Ky4JvmNtFR5'
    '+CzD7xnwoS3jY97u6gMzMHjb7t2pA7rFnVImwivvi/y3sEAL9P5N+mL+q82q9qsmcb58tlkAteUp'
    'crOvlmRk3kzlM0tz9/Rm4bYttEDBt67Fg4/hZbZ+m+5BMzVWWdtZIdHUIGJjew=='
)


def pem(label=b'PRIVATE KEY', payload=PRIVATE_DER, headers=b'', newline=b'\n'):
    encoded = base64.b64encode(payload)
    lines = [encoded[i:i + 64] for i in range(0, len(encoded), 64)]
    return (b'-----BEGIN ' + label + b'-----' + newline + headers
            + newline.join(lines) + newline + b'-----END ' + label + b'-----')


def tar(entries, format=tarfile.PAX_FORMAT):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode='w', format=format) as archive:
        for name, value in entries:
            member = name if isinstance(name, tarfile.TarInfo) else tarfile.TarInfo(name)
            member.size = len(value)
            archive.addfile(member, io.BytesIO(value))
    return output.getvalue()


def docker(layers, extra=None, config_change=None, config_encoder=json.dumps):
    config = {'architecture': 'amd64', 'os': 'linux',
              'rootfs': {'type': 'layers', 'diff_ids': ['sha256:' + hashlib.sha256(layer).hexdigest() for layer in layers]}}
    config.update(config_change or {})
    raw = config_encoder(config).encode()
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

    def test_podman_layer_mapping(self):
        with self.subTest(phase="podman_canonical_layers_and_legacy_aliases_scan_actual_bytes"):
            for manifest_alias in (False, True):
                stream, image = self.podman_archive(manifest_alias=manifest_alias)
                result = audit.audit_stream(io.BytesIO(stream), image, [SECRET])
                self.assertEqual(result['status'], 'rejected-private-material')
                self.assertEqual(len(result['layers']), 1)
                self.assertEqual(result['layers'][0]['findings'], ['provided-private-material'])
        with self.subTest(phase="podman_aliases_reject_escape_unknown_and_digest_mismatch"):
            for options in ({'target': '../../' + 'a' * 64 + '.tar'},
                            {'target': '../' + 'a' * 64 + '.tar'},
                            {'target': '/absolute.tar'},
                            {'target': '../x/layer.tar'},
                            {'alias_name': 'unexpected-link'}, {'wrong_digest': True}):
                with self.subTest(options=options):
                    stream, image = self.podman_archive(**options)
                    with self.assertRaises(audit.AuditError):
                        audit.audit_stream(io.BytesIO(stream), image, [SECRET])
        with self.subTest(phase="repeated_layer_occurrences_keep_order_and_scan_unique_payloads_once"):
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
        with self.subTest(phase="repeated_layers_still_require_exact_order_multiplicity_and_coverage"):
            for options in ({'order': [1, 0, 2, 3, 4]}, {'order': [0, 1, 2, 3]},
                            {'order': [0, 1, 2, 3, 4, 4]}, {'unreferenced': True}):
                with self.subTest(options=options):
                    stream, image, _ = self.repeated_archive(**options)
                    with self.assertRaisesRegex(audit.AuditError, 'identity mismatch'):
                        audit.audit_stream(io.BytesIO(stream), image)
        with self.subTest(phase="repeated_occurrences_still_obey_layer_count_bound"):
            stream, image, _ = self.repeated_archive()
            with patch.object(audit, 'MAX_LAYERS', 4):
                with self.assertRaisesRegex(audit.AuditError, 'too many layer occurrences'):
                    audit.audit_stream(io.BytesIO(stream), image)
        with self.subTest(phase="create_then_delete_secret_is_detected_in_earlier_layer"):
            stream, image = docker([tar([('private.key', SECRET)]), tar([('.wh.private.key', b'')])])
            result = audit.audit_stream(io.BytesIO(stream), image, [SECRET])
            self.assertEqual(result['status'], 'rejected-private-material')
            self.assertEqual(result['layers'][0]['findings'], ['provided-private-material'])
            self.assertEqual(result['layers'][1]['findings'], [])
            self.assertNotIn(SECRET.decode(), json.dumps(result))

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

    def test_private_pem_detection(self):
        with self.subTest(phase="boundary_match_and_private_envelope_public_key_allowed"):
            for value, findings in ((b'-----BEGIN PUBLIC KEY-----\npublic\n-----END PUBLIC KEY-----', []),
                                    (pem(b'ENCRYPTED PRIVATE KEY'), ['private-pem-envelope']),
                                    (b'x' * 499 + SECRET, ['provided-private-material'])):
                stream, image = docker([tar([('file', value)])])
                with patch.object(audit, 'CHUNK', 512):
                    result = audit.audit_stream(io.BytesIO(stream), image, [SECRET])
                self.assertEqual(result['layers'][0]['findings'], findings)
        with self.subTest(phase="private_key_parser_literals_and_malformed_envelopes_are_not_keys"):
            values = (b'-----BEGIN PRIVATE KEY-----',
                      b'-----BEGIN ENCRYPTED PRIVATE KEY-----\nopaque',
                      b'-----BEGIN PRIVATE KEY-----\0-----END PRIVATE KEY-----',
                      b'-----BEGIN PRIVATE KEY-----\n-----END PRIVATE KEY-----',
                      b'-----BEGIN PRIVATE KEY-----\nsynthetic-not-a-key\n-----END PRIVATE KEY-----',
                      pem().replace(b'-----END PRIVATE KEY-----', b'-----END RSA PRIVATE KEY-----'),
                      pem().replace(base64.b64encode(PRIVATE_DER), b'not=base64'),
                      b'-----BEGIN PRIVATE KEY-----"\n"-----END PRIVATE KEY-----')
            for value in values:
                with self.subTest(value=value):
                    stream, image = docker([tar([('crypto-parser', value)])])
                    result = audit.audit_stream(io.BytesIO(stream), image)
                    self.assertEqual(result['status'], 'passed-scoped-scan')
        with self.subTest(phase="complete_private_pem_variants_are_rejected"):
            for label in (b'PRIVATE KEY', b'RSA PRIVATE KEY', b'EC PRIVATE KEY', b'DSA PRIVATE KEY',
                          b'ENCRYPTED PRIVATE KEY', b'OPENSSH PRIVATE KEY', b'ENCRYPTED COSIGN PRIVATE KEY'):
                for newline in (b'\n', b'\r\n'):
                    with self.subTest(label=label, newline=newline):
                        stream, image = docker([tar([('unfamiliar-name', pem(label, newline=newline))])])
                        result = audit.audit_stream(io.BytesIO(stream), image)
                        self.assertEqual(result['layers'][0]['findings'], ['private-pem-envelope'])
                        self.assertNotIn(base64.b64encode(PRIVATE_DER).decode(), json.dumps(result))
            encrypted = pem(b'RSA PRIVATE KEY', headers=(
                b'Proc-Type: 4,ENCRYPTED\nDEK-Info: AES-256-CBC,0123456789ABCDEF\n\n'))
            stream, image = docker([tar([('encrypted', encrypted)])])
            self.assertEqual(audit.audit_stream(io.BytesIO(stream), image)['status'], 'rejected-private-material')
        with self.subTest(phase="complete_pem_survives_every_single_byte_read_boundary"):
            values = (pem(), pem().replace(b'\n', b''), pem(b'RSA PRIVATE KEY', headers=(
                b'Proc-Type: 4,ENCRYPTED\nDEK-Info: AES-256-CBC,0123456789ABCDEF\n\n')),
                pem().replace(b'\n', b'\\r\\n'))
            for value in values:
                for cut in range(1, len(value)):
                    with self.subTest(length=len(value), cut=cut):
                        reader = audit.Reader(io.BytesIO(value), len(value), hashing=True)
                        reader.read(cut)
                        reader.read(len(value) - cut)
                        self.assertEqual(reader.findings, {'private-pem-envelope'})
        with self.subTest(phase="large_envelopes_and_later_keys_survive_chunk_and_candidate_boundaries"):
            # Larger than both the old overlap and the test read chunk.
            value = b'x' * 499 + pem(payload=PRIVATE_DER * 64)
            for prefix in (b'', b'-----BEGIN PRIVATE KEY-----\0invalid\0'):
                stream, image = docker([tar([('key', prefix + value)])])
                with patch.object(audit, 'CHUNK', 512):
                    result = audit.audit_stream(io.BytesIO(stream), image)
                self.assertEqual(result['status'], 'rejected-private-material')
            for value in (pem(payload=PRIVATE_DER * 64), b'-----BEGIN PRIVATE KEY-----\n' + b'A' * 4096):
                reader = audit.Reader(io.BytesIO(value), len(value), hashing=True)
                with patch.object(audit, 'MAX_PEM', 1024):
                    with self.assertRaisesRegex(audit.AuditError, 'private PEM envelope exceeds size limit'):
                        reader.read(len(value))
        with self.subTest(phase="complete_pem_in_json_values_rejected_without_joining_parser_literals"):
            for values, findings in ((['KEY=' + pem().decode()], ['private-pem-envelope']),
                                     (['-----BEGIN PRIVATE KEY-----', base64.b64encode(PRIVATE_DER).decode(),
                                       '-----END PRIVATE KEY-----'], [])):
                stream, image = docker([tar([('public', b'ok')])], config_change={'config': {'Env': values}})
                result = audit.audit_stream(io.BytesIO(stream), image)
                self.assertEqual(result['metadata_findings'], findings)
        with self.subTest(phase="exact_private_match_is_independent_of_pem_structure"):
            value = b'-----BEGIN PRIVATE KEY-----\0not-an-envelope'
            stream, image = docker([tar([('literal', value)])])
            result = audit.audit_stream(io.BytesIO(stream), image, [value])
            self.assertEqual(result['layers'][0]['findings'], ['provided-private-material'])

    def test_public_vector_exemptions(self):
        with self.subTest(phase="exact_public_vector_is_allowed_with_normalized_wrapping"):
            self.assertIn(hashlib.sha256(PUBLIC_VECTOR_DER).hexdigest(), audit.PUBLIC_PEM_DER_SHA256)
            for value in (pem(payload=PUBLIC_VECTOR_DER),
                          pem(payload=PUBLIC_VECTOR_DER).replace(b'\n', b''),
                          pem(payload=PUBLIC_VECTOR_DER, newline=b'\r\n'),
                          pem(payload=PUBLIC_VECTOR_DER).replace(b'\n', b'\\n')):
                for cut in range(1, len(value)):
                    reader = audit.Reader(io.BytesIO(value), len(value), hashing=True)
                    reader.read(cut)
                    reader.read(len(value) - cut)
                    self.assertEqual(reader.findings, set())
                stream, image = docker([tar([('any-path', value)])])
                self.assertEqual(audit.audit_stream(io.BytesIO(stream), image)['status'], 'passed-scoped-scan')
        with self.subTest(phase="legacy_public_ec_vectors_only_exempt_exact_authenticated_payloads"):
            for der in LEGACY_PUBLIC_EC_DER:
                self.assertIn(hashlib.sha256(der).hexdigest(), audit.PUBLIC_PEM_DER_SHA256)
                public = pem(b'EC PRIVATE KEY', payload=der)
                stream, image = docker([tar([('legacy-library', public)])])
                self.assertEqual(audit.audit_stream(io.BytesIO(stream), image)['status'], 'passed-scoped-scan')
                self.assertEqual(audit.audit_stream(io.BytesIO(stream), image, [public])['layers'][0]['findings'],
                                 ['provided-private-material'])
                modified = pem(b'EC PRIVATE KEY', payload=der[:-1] + bytes([der[-1] ^ 1]))
                for value in (modified, public + b'\0' + pem()):
                    for cut in range(len(value)):
                        reader = audit.Reader(io.BytesIO(value), len(value), hashing=True)
                        reader.read(cut)
                        reader.read(len(value) - cut)
                        self.assertEqual(reader.findings, {'private-pem-envelope'})
        with self.subTest(phase="public_vector_does_not_exempt_mutated_or_later_unknown_key"):
            public = pem(payload=PUBLIC_VECTOR_DER)
            mutated = pem(payload=PUBLIC_VECTOR_DER[:-1] + bytes([PUBLIC_VECTOR_DER[-1] ^ 1]))
            for value in (mutated, public + b'\0' + pem(), public + b'\n' + mutated,
                          public * 3 + pem(b'RSA PRIVATE KEY')):
                # Exercise both whole-window and every split through Reader. This
                # must continue scanning after each allowed block in the same read.
                for cut in range(len(value)):
                    reader = audit.Reader(io.BytesIO(value), len(value), hashing=True)
                    reader.read(cut)
                    reader.read(len(value) - cut)
                    self.assertEqual(reader.findings, {'private-pem-envelope'})
                stream, image = docker([tar([('usr/lib64/libgnutls.so', b'\x7fELF' + value)])])
                with patch.object(audit, 'CHUNK', 512):
                    self.assertEqual(audit.audit_stream(io.BytesIO(stream), image)['status'], 'rejected-private-material')
        with self.subTest(phase="public_der_exception_does_not_apply_to_encrypted_envelopes"):
            value = pem(b'RSA PRIVATE KEY', payload=PUBLIC_VECTOR_DER, headers=(
                b'Proc-Type: 4,ENCRYPTED\nDEK-Info: AES-256-CBC,0123456789ABCDEF\n\n'))
            stream, image = docker([tar([('encrypted', value)])])
            self.assertEqual(audit.audit_stream(io.BytesIO(stream), image)['status'], 'rejected-private-material')
        with self.subTest(phase="encrypted_public_fixture_binds_label_headers_and_ciphertext"):
            public = pem(b'RSA PRIVATE KEY', payload=ENCRYPTED_PUBLIC_CIPHERTEXT,
                         headers=ENCRYPTED_PUBLIC_HEADERS)
            for value in (public, public.replace(b'\n', b'\r\n'), public.replace(b'\n', b'\\n')):
                for cut in range(1, len(value)):
                    reader = audit.Reader(io.BytesIO(value), len(value), hashing=True)
                    reader.read(cut)
                    reader.read(len(value) - cut)
                    self.assertEqual(reader.findings, set())
            prefix, iv = ENCRYPTED_PUBLIC_HEADERS.rsplit(b',', 1)
            changed_iv = prefix + b',' + (b'1' if iv[:1] == b'0' else b'0') + iv[1:]
            changed_cipher = ENCRYPTED_PUBLIC_HEADERS.replace(b'DEK-Info: ', b'DEK-Info: WRONG-')
            for label, headers, payload in (
                    (b'EC PRIVATE KEY', ENCRYPTED_PUBLIC_HEADERS, ENCRYPTED_PUBLIC_CIPHERTEXT),
                    (b'RSA PRIVATE KEY', changed_iv, ENCRYPTED_PUBLIC_CIPHERTEXT),
                    (b'RSA PRIVATE KEY', changed_cipher, ENCRYPTED_PUBLIC_CIPHERTEXT),
                    (b'RSA PRIVATE KEY', ENCRYPTED_PUBLIC_HEADERS,
                     ENCRYPTED_PUBLIC_CIPHERTEXT[:-1] + bytes([ENCRYPTED_PUBLIC_CIPHERTEXT[-1] ^ 1]))):
                value = pem(label, payload=payload, headers=headers)
                stream, image = docker([tar([('example.pem', value)])])
                self.assertEqual(audit.audit_stream(io.BytesIO(stream), image)['status'], 'rejected-private-material')
            stream, image = docker([tar([('example.pem', public)])])
            self.assertEqual(audit.audit_stream(io.BytesIO(stream), image, [public])['layers'][0]['findings'],
                             ['provided-private-material'])
            stream, image = docker([tar([('example.pem', public + b'\n' + pem())])])
            self.assertEqual(audit.audit_stream(io.BytesIO(stream), image)['status'], 'rejected-private-material')
        with self.subTest(phase="exact_private_input_always_overrides_public_vector_exception"):
            public = pem(payload=PUBLIC_VECTOR_DER)
            for value in (public, PUBLIC_VECTOR_DER):
                for cut in range(1, len(value)):
                    reader = audit.Reader(io.BytesIO(value), len(value), patterns=(value,), hashing=True)
                    reader.read(cut)
                    reader.read(len(value) - cut)
                    self.assertEqual(reader.findings, {'provided-private-material'})
                stream, image = docker([tar([('private', b'x' * 499 + value)])])
                with patch.object(audit, 'CHUNK', 512):
                    result = audit.audit_stream(io.BytesIO(stream), image, [value + b'\n'])
                self.assertEqual(result['layers'][0]['findings'], ['provided-private-material'])
                self.assertEqual(result['status'], 'rejected-private-material')
        with self.subTest(phase="public_vectors_in_metadata_do_not_hide_unknown_keys_or_exact_matches"):
            public = pem(payload=PUBLIC_VECTOR_DER)
            for value, private, findings in (
                    (public, [], []),
                    (public + b'\n' + pem(), [], ['private-pem-envelope']),
                    (public, [public.replace(b'\n', b'\\n')], ['provided-private-material'])):
                stream, image = docker([tar([('public', b'ok')])],
                                       config_change={'config': {'Env': ['KEY=' + value.decode()]}})
                result = audit.audit_stream(io.BytesIO(stream), image, private)
                self.assertEqual(result['metadata_findings'], findings)

    def test_metadata_and_configuration_secrets(self):
        with self.subTest(phase="decoded_json_metadata_keys_cannot_hide_behind_valid_escapes"):
            private = pem(payload=PRIVATE_DER + b'\xff' * 3)
            public = pem(payload=PUBLIC_VECTOR_DER)
            self.assertIn(b'/', private)
            def escaped_json(value):
                return json.dumps(value).replace('\\n', '\\u000a').replace('/', '\\/')
            for value, supplied, findings in (
                    (private, [], ['private-pem-envelope']),
                    (public, [public], ['provided-private-material']),
                    (public, [], [])):
                stream, image = docker([tar([('public', b'ok')])],
                    config_change={'config': {'Env': ['KEY=' + value.decode()]}}, config_encoder=escaped_json)
                result = audit.audit_stream(io.BytesIO(stream), image, supplied)
                self.assertEqual(result['metadata_findings'], findings)
            # Every distinct string, including duplicate JSON object values, gets
            # its own scanner. Neither decoding nor duplicates may conceal a key.
            duplicate = ('{"x":' + escaped_json(private.decode()) + ',"x":"public"}').encode()
            separate = escaped_json(['-----BEGIN PRIVATE KEY-----',
                base64.b64encode(PRIVATE_DER).decode(), '-----END PRIVATE KEY-----']).encode()
            for raw, findings in ((duplicate, ['private-pem-envelope']), (separate, [])):
                stream, image = docker([tar([('public', b'ok')])], extra=[('legacy.json', raw)])
                self.assertEqual(audit.audit_stream(io.BytesIO(stream), image)['metadata_findings'], findings)
        with self.subTest(phase="secrets_in_configuration_are_not_missed"):
            stream, image = docker([tar([('public', b'ok')])], config_change={'config': {'Env': ['VALUE=' + SECRET.decode()]}})
            result = audit.audit_stream(io.BytesIO(stream), image, [SECRET])
            self.assertEqual(result['metadata_findings'], ['provided-private-material'])
            self.assertEqual(result['status'], 'rejected-private-material')
        with self.subTest(phase="scope_excludes_outer_archive_names_not_part_of_image_payload"):
            stream, image = docker([tar([('public', b'ok')])], extra=[(SECRET.decode(), b'')])
            result = audit.audit_stream(io.BytesIO(stream), image, [SECRET])
            self.assertEqual(result['status'], 'passed-scoped-scan')
            self.assertEqual(result['scope'], 'all uncompressed layer bytes and archive metadata file contents: '
                             'provided exact private bytes and complete private PEM envelopes excluding pinned public fixtures')
            self.assertFalse(result['universal_secret_absence_proven'])
        with self.subTest(phase="credential_file_newlines_do_not_hide_shell_exported_value"):
            for content in (SECRET, SECRET + b'\r\n'):
                stream, image = docker([tar([('environment', content)])])
                result = audit.audit_stream(io.BytesIO(stream), image, [SECRET + b'\r\n'])
                self.assertEqual(result['layers'][0]['findings'], ['provided-private-material'])
        with self.subTest(phase="pax_long_names_are_streamed_and_scanned"):
            stream, image = docker([tar([('a/' * 100 + 'file', SECRET)])])
            result = audit.audit_stream(io.BytesIO(stream), image, [SECRET])
            self.assertEqual(result['status'], 'rejected-private-material')

    def test_archive_shape_rejections(self):
        with self.subTest(phase="compressed_truncated_misbound_or_oversized_archives_rejected"):
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
        with self.subTest(phase="manifest_layer_and_duplicate_metadata_rejected"):
            layer = tar([('file', b'ok')])
            for extra, change in (([('manifest.json', b'[]')], None),
                                  (None, {'rootfs': {'type': 'layers', 'diff_ids': ['sha256:' + 'f' * 64]}})):
                stream, image = docker([layer], extra, change)
                with self.assertRaises(audit.AuditError):
                    audit.audit_stream(io.BytesIO(stream), image)
        with self.subTest(phase="oversized_metadata_is_rejected_before_reading_payload"):
            output = io.BytesIO()
            with tarfile.open(fileobj=output, mode='w') as archive:
                member = tarfile.TarInfo('large-pax')
                member.type = b'x'
                member.size = 65537
                archive.addfile(member, io.BytesIO(b'x' * member.size))
            stream, image = docker([output.getvalue()])
            with self.assertRaisesRegex(audit.AuditError, 'oversized tar metadata'):
                audit.audit_stream(io.BytesIO(stream), image)

    def test_export_process_and_cli_receipts(self):
        with self.subTest(phase="export_error_kills_only_owned_process_group_and_hides_stderr"):
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
        with self.subTest(phase="cli_parser_failure_emits_safe_public_receipt_and_cleans_owned_export"):
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
        with self.subTest(phase="cli_failure_receipts_never_copy_private_paths_or_exception_diagnostics"):
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
        with self.subTest(phase="real_stalled_export_is_terminated_at_deadline"):
            if not (hasattr(os, 'killpg')):
                self.skipTest('process-group deadline requires POSIX')
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

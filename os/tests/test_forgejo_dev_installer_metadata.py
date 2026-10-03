#!/usr/bin/env python3
"""CI medium metadata must be bounded, intact and bound to its live run."""
import importlib.util
import json
from pathlib import Path
import struct
import unittest
from unittest.mock import patch
import zlib

SPEC = importlib.util.spec_from_file_location(
    "medium", Path(__file__).resolve().parents[1] / "dev/forgejo-dev-installer-metadata.py")
medium = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(medium)


def entry(name, data, descriptor=True):
    compressor = zlib.compressobj(wbits=-15)
    packed = compressor.compress(data) + compressor.flush()
    crc = zlib.crc32(data)
    header = struct.pack('<I5H3I2H', 0x04034b50, 20, 8 if descriptor else 0, 8, 0, 0,
                         0 if descriptor else crc, 0 if descriptor else len(packed),
                         0 if descriptor else len(data), len(name), 0)
    tail = struct.pack('<4sIII', b'PK\x07\x08', crc, len(packed), len(data)) if descriptor else b''
    return header + name.encode() + packed + tail


def fixture(descriptor=True):
    revision = 'a' * 40
    files = {'source.txt': ('build_source=' + revision + '\n').encode(),
             'tilo-dev-installer-aarch64.qcow2.sha256':
                 ('b' * 64 + '  tilo-dev-installer-aarch64.qcow2\n').encode(),
             'tilo-dev-installer-aarch64.qcow2.zst.sha256':
                 ('c' * 64 + '  tilo-dev-installer-aarch64.qcow2.zst\n').encode()}
    return revision, files, b''.join(entry(n, d, descriptor) for n, d in files.items())


class MetadataTests(unittest.TestCase):
    def test_metadata_without_payload_or_central_directory(self):
        for descriptor in (False, True):
            with self.subTest(descriptor=descriptor):
                _, files, prefix = fixture(descriptor)
                self.assertEqual(medium.metadata(prefix), files)
                self.assertEqual(medium.metadata(prefix, with_offset=True), (files, len(prefix)))

    def test_corrupt_duplicate_oversized_and_truncated_entries_are_refused(self):
        _, _, prefix = fixture()
        bad_crc = bytearray(prefix)
        descriptor = bad_crc.index(b'PK\x07\x08')
        bad_crc[descriptor + 4] ^= 1
        cases = [bytes(bad_crc), prefix[:40],
                 entry('source.txt', b'x') * 3,
                 entry('source.txt', bytes(65537))]
        for candidate in cases:
            with self.subTest(candidate_length=len(candidate)):
                with self.assertRaises((ValueError, struct.error)):
                    medium.metadata(candidate)

    def test_live_artifact_source_binding_and_bounded_download(self):
        revision, _, prefix = fixture()
        run = {'id': 17, 'workflow_id': 'os-dev-installer.yml', 'status': 'success',
               'is_fork_pull_request': False, 'commit_sha': revision}
        artifact = {'id': 44, 'name': 'tilo-dev-installer-aarch64-17',
                    'run_id': 17, 'expired': False}

        class Response:
            status = 200

            def __enter__(self):
                return self

            def __exit__(self, *_):
                return False

            def read(self, maximum):
                self.assert_bounded(maximum)
                return prefix

            assert_bounded = staticmethod(lambda maximum: self.assertEqual(maximum, 65536))

        responses = [json.dumps(run).encode(), json.dumps([artifact]).encode(), b'test-token']
        with patch.object(medium, 'fgj', side_effect=responses), \
                patch.object(medium.urllib.request.OpenerDirector, 'open', return_value=Response()):
            result = medium.verify_medium(17)
            self.assertEqual(result['artifact_id'], 44)
            self.assertEqual(result['assembly_source'], revision)
            self.assertNotIn('test-token', json.dumps(result))
        run['commit_sha'] = 'd' * 40
        responses = [json.dumps(run).encode(), json.dumps([artifact]).encode(), b'test-token']
        with patch.object(medium, 'fgj', side_effect=responses), \
                patch.object(medium.urllib.request.OpenerDirector, 'open', return_value=Response()), \
                self.assertRaisesRegex(SystemExit, 'assembly source'):
            medium.verify_medium(17)


if __name__ == '__main__':
    unittest.main()

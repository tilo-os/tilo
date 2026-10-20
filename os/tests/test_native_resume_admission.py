#!/usr/bin/env python3
"""Exercise checkpoint admission on real files; only the image engine is replaced."""
import contextlib
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
from types import SimpleNamespace
import unittest
from restart_fixture import restart_fixture

ROOT = Path(__file__).parents[2]
spec = importlib.util.spec_from_file_location('resume_admission', ROOT / 'os/dev/native_resume.py')
resume = importlib.util.module_from_spec(spec)
spec.loader.exec_module(resume)
native = resume.load_native(ROOT)


class Admission(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.payload = self.root / 'image'
        (self.payload / 'partial').mkdir(parents=True)
        self.destination = self.root / 'restored'
        self.destination.mkdir()
        self.union = {}
        self.expected = {'base': 'base', 'rpms': 'rpms', 'wpe_receipt_sha256': 'wpe'}
        tools = SimpleNamespace(**{name: getattr(native.tools, name) for name in
            ('read_bounded', 'sha', 'strict_json', 'canonical')})
        tools.registry_identity = lambda *args: 'sha256:' + 'b' * 64
        tools.command = lambda *args: b''
        tools.inspect_local = lambda *args: 'sha256:' + 'b' * 64
        tools.mounted = lambda *args: contextlib.nullcontext(self.payload)
        self.admission_native = SimpleNamespace(tools=tools, REQUIRED=['shell'], file_record=native.file_record)
        self.receipt = self.make_payload(b'original')

    def make_payload(self, suffix):
        header = bytearray(64)
        header[:7] = b'\x7fELF\x02\x01\x01'
        struct.pack_into('<HH', header, 16, 3, 183)
        binary = self.payload / 'partial/shell'
        binary.write_bytes(header + suffix)
        binary.chmod(0o755)
        inventory = {'shell': native.file_record(binary)[0]}
        metadata = {'schema': 1, 'purpose': resume.PARTIAL, 'source_revision': resume.SOURCE,
            'installable': False, 'complete_native_builder': False, 'build_success': False,
            'architecture': 'aarch64', 'run_id': 20866, 'run_number': 7489, 'inventory': inventory,
            'inputs': {'revision': resume.SOURCE, 'architecture': 'aarch64', 'builder': 'builder',
                'pins': {'BASE_REF': 'base', 'RPMS_REF': 'rpms'}, 'wpe_adoption_sha256': 'wpe'}}
        raw = native.tools.canonical(metadata)
        (self.payload / 'partial-checkpoint.json').write_bytes(raw)
        return {'purpose': resume.PARTIAL, 'source_revision': resume.SOURCE,
            'reference': 'registry.tilo.so/tilo/tilo-development-partial-native-aarch64@sha256:' + 'a' * 64,
            'config_digest': 'sha256:' + 'b' * 64, 'metadata_sha256': native.tools.sha(raw),
            'inventory': inventory, 'installable': False, 'complete_native_builder': False, 'build_success': False}

    def admit(self, receipt=None):
        resume.validate_partial(self.admission_native, receipt or self.receipt, self.expected,
                                'builder', self.destination, self.union)

    def test_restore_admission(self):
        with self.subTest(phase="verified_bytes_restored_and_identical_overlap_is_allowed"):
            self.admit()
            self.admit()
            self.assertEqual(self.union, self.receipt['inventory'])
            self.assertEqual(native.file_record(self.destination / 'shell')[0], self.union['shell'])
        restart_fixture(self)
        with self.subTest(phase="changed_bytes_and_unlisted_files_rejected_before_copy"):
            with (self.payload / 'partial/shell').open('ab') as output:
                output.write(b'changed')
            with self.assertRaises(ValueError):
                self.admit()
            self.assertEqual(list(self.destination.iterdir()), [])
            self.receipt = self.make_payload(b'original')
            (self.payload / 'partial/unlisted').write_bytes(b'not admitted')
            with self.assertRaises(ValueError):
                self.admit()
        restart_fixture(self)
        with self.subTest(phase="conflicting_checkpoint_cannot_replace_restored_binary"):
            self.admit()
            before = (self.destination / 'shell').read_bytes()
            conflicting = self.make_payload(b'different valid binary')
            with self.assertRaisesRegex(ValueError, 'overlapping checkpoint'):
                self.admit(conflicting)
            self.assertEqual((self.destination / 'shell').read_bytes(), before)

    def test_metadata_claims_rejected(self):
        with self.subTest(phase="metadata_substitution_and_complete_claim_rejected"):
            path = self.payload / 'partial-checkpoint.json'
            metadata = json.loads(path.read_bytes())
            metadata['inputs']['pins']['BASE_REF'] = 'other'
            raw = native.tools.canonical(metadata)
            path.write_bytes(raw)
            self.receipt['metadata_sha256'] = native.tools.sha(raw)
            with self.assertRaisesRegex(ValueError, 'source/toolchain inputs'):
                self.admit()
            self.receipt['complete_native_builder'] = True
            with self.assertRaisesRegex(ValueError, 'honest partial'):
                self.admit()
        restart_fixture(self)
        with self.subTest(phase="checkpoint_metadata_cannot_claim_completion"):
            path = self.payload / 'partial-checkpoint.json'
            metadata = json.loads(path.read_bytes())
            metadata['complete_native_builder'] = True
            raw = native.tools.canonical(metadata)
            path.write_bytes(raw)
            self.receipt['metadata_sha256'] = native.tools.sha(raw)
            with self.assertRaisesRegex(ValueError, 'metadata identity'):
                self.admit()


if __name__ == '__main__':
    unittest.main()

#!/usr/bin/env python3
"""Real compiler-artifact IO and closed inventory checks; no Rust compilation."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).with_name('export-sdk-compiler.py')
SPEC = importlib.util.spec_from_file_location('compiler_export', SOURCE)
EXPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EXPORT)


class CompilerInputs(unittest.TestCase):
    def make_archive(self, root):
        member = root / 'lib.rmeta'
        member.write_bytes(b'actual archived metadata member')
        archive = root / 'libfixture.rlib'
        subprocess.run(['ar', 'crD', str(archive), str(member)], check=True)
        return archive, member

    def test_metadata_only_archive_preserves_exact_member_and_actual_input_hash(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            source, member = self.make_archive(root)
            expected = hashlib.sha256(source.read_bytes()).hexdigest()
            output = root / 'metadata.rlib'
            scratch = root / 'scratch'
            scratch.mkdir()
            digest = EXPORT.metadata_archive(source, output, scratch / 'lib.rmeta')
            self.assertEqual(digest, expected)
            result = subprocess.run(['ar', 'p', str(output), 'lib.rmeta'],
                                    check=True, capture_output=True)
            self.assertEqual(result.stdout, member.read_bytes())
            self.assertEqual(subprocess.check_output(['ar', 't', str(output)]).splitlines(),
                             [b'lib.rmeta'])

    def test_pending_archive_read_rejects_path_replacement_even_with_pinned_fd(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            source, _ = self.make_archive(root)
            scratch = root / 'scratch'
            scratch.mkdir()
            actual_run = subprocess.run
            def replace_before_ar(argv, **kwargs):
                self.assertEqual(argv[2].split('/')[:4], ['', 'proc', 'self', 'fd'])
                source.rename(root / 'original.rlib')
                source.write_bytes(b'foreign replacement')
                return actual_run(argv, **kwargs)
            with patch.object(EXPORT.subprocess, 'run', replace_before_ar):
                with self.assertRaisesRegex(ValueError, 'changed while'):
                    EXPORT.metadata_archive(source, root / 'out.rlib', scratch / 'lib.rmeta')
            self.assertFalse((root / 'out.rlib').exists())

    def test_closed_capture_refuses_mutated_undeclared_and_aliased_payloads(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            artifacts = root / 'artifacts'
            artifacts.mkdir()
            payload = artifacts / 'libui.rlib'
            payload.write_bytes(b'captured exact metadata')
            descriptor = dict(schema=1, profile='thin-app', resources=['tilo.text'],
                              ui=payload.name, units=[dict(artifacts=[dict(
                                  name=payload.name, size=payload.stat().st_size,
                                  sha256=EXPORT.digest(payload))])])
            (root / 'manifest.json').write_text(json.dumps(descriptor))
            self.assertEqual(EXPORT.checked_capture(root), descriptor)
            (artifacts / 'foreign').write_bytes(b'not in unit inventory')
            with self.assertRaisesRegex(ValueError, 'incomplete'):
                EXPORT.checked_capture(root)
            (artifacts / 'foreign').unlink()
            payload.write_bytes(b'mutated metadata')
            with self.assertRaisesRegex(ValueError, 'differs'):
                EXPORT.checked_capture(root)
            payload.unlink()
            os.symlink(root / 'manifest.json', payload)
            with self.assertRaisesRegex(ValueError, 'differs'):
                EXPORT.checked_capture(root)

    def test_native_exports_preserve_selected_runtime_payload_identities(self):
        runtime = dict(files=[dict(path='lib/libstd-real.so', size=17, hash='actual'),
                              dict(path='lib/libruntime_dylib.so', size=23, hash='toolkit')])
        compiler = dict(files=[dict(path='lib/libstd-real.so', size=17, hash='actual')])
        EXPORT.verify_runtime_exports(runtime, compiler)
        for replacement in [[], [dict(path='lib/libstd-real.so', size=17, hash='foreign')],
                             [dict(path='lib/libstd-real.so', size=18, hash='actual')]]:
            with self.assertRaisesRegex(ValueError, 'native closure differs'):
                EXPORT.verify_runtime_exports(runtime, dict(files=replacement))



if __name__ == '__main__':
    unittest.main()

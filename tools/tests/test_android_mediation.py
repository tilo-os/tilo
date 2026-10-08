"""Mediation input admission tests; Android API behavior requires the real APK journey."""
import argparse
import importlib.util
import json
from pathlib import Path
import struct
import sys
import tempfile
import unittest

ANDROID = Path(__file__).resolve().parents[1] / 'android'
sys.path.insert(0, str(ANDROID))
from qualify_apk import digest, runtime_files, verify_runtime


def module(name):
    spec = importlib.util.spec_from_file_location('android_mediation_' + name, ANDROID / 'mediation' / (name + '.py'))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


stage_runtime = module('stage_runtime')
patch_framework = module('patch_framework')


class MediationAdmission(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        elf = bytearray(64)
        elf[:6] = b'\x7fELF\x02\x01'
        struct.pack_into('<H', elf, 18, 62)
        self.elf = bytes(elf)
        base = self.root / 'base'
        (base / 'bin').mkdir(parents=True)
        (base / 'licenses').mkdir()
        (base / 'lib').mkdir()
        (base / 'usr/lib/java/dex/android_translation_layer').mkdir(parents=True)
        (base / 'bin/android-translation-layer').write_bytes(self.elf)
        (base / 'bin/android-translation-layer').chmod(0o755)
        (base / 'licenses/acquisition.txt').write_text('Test fixture only')
        (base / 'usr/lib/java/dex/android_translation_layer/api-impl.jar').write_bytes(b'base framework fixture')
        receipt = self.root / 'base-receipt.json'
        receipt.write_text(json.dumps({'schema': 1, 'architecture': 'x86_64', 'files': runtime_files(base)}))
        inputs = {}
        for name, data in [('entry', self.elf + b'new'), ('bridge', self.elf), ('framework', b'new framework fixture')]:
            path = self.root / name
            path.write_bytes(data)
            inputs[name] = path
            inputs[name + '_sha256'] = digest(path)
        self.args = argparse.Namespace(**inputs, base=base, base_receipt=receipt,
            base_receipt_sha256=digest(receipt), architecture='x86_64',
            output=self.root / 'new-runtime', receipt=self.root / 'new-receipt.json')

    def test_new_receipt_covers_replacements_and_base_stays_unchanged(self):
        old = runtime_files(self.args.base)
        stage_runtime.stage(self.args)
        verify_runtime(self.args.output, self.args.receipt, digest(self.args.receipt), 'x86_64')
        self.assertEqual(runtime_files(self.args.base), old)
        self.assertEqual((self.args.output / 'lib/libtilo-android-chooser.so').read_bytes(), self.elf)

    def test_changed_base_or_patch_input_refuses_publication(self):
        for path in [self.args.entry, self.args.base / 'licenses/acquisition.txt']:
            saved = path.read_bytes()
            path.write_bytes(saved + b'tampered')
            with self.assertRaises(ValueError): stage_runtime.stage(self.args)
            self.assertFalse(self.args.output.exists())
            self.assertFalse(self.args.receipt.exists())
            path.write_bytes(saved)

    def test_wrong_architecture_and_receipt_inside_runtime_are_refused(self):
        self.args.architecture = 'aarch64'
        with self.assertRaises(ValueError): stage_runtime.stage(self.args)
        self.args.architecture = 'x86_64'
        self.args.receipt = self.args.output / 'receipt.json'
        with self.assertRaises(ValueError): stage_runtime.stage(self.args)
        self.assertFalse(self.args.output.exists())

    def test_framework_patch_refuses_wrong_source_before_any_mutation(self):
        for name in patch_framework.PINS:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('unqualified upstream source')
        before = {name: (self.root / name).read_bytes() for name in patch_framework.PINS}
        with self.assertRaises(ValueError): patch_framework.patch(self.root)
        self.assertEqual(before, {name: (self.root / name).read_bytes() for name in patch_framework.PINS})
        self.assertFalse((self.root / 'src/api-impl/android/app/TiloChooser.java').exists())

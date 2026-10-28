#!/usr/bin/env python3
"""Immutable base selection must agree with the actual generated prerequisite recipe."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location('prerequisite_image', HERE / 'prerequisite-image.py')
IMAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(IMAGE)


class ImmutableBaseTests(unittest.TestCase):
    def test_recipe_and_installed_provenance_use_selected_base(self):
        for base in ('sha256:' + 'a' * 64,
                     'registry.fedoraproject.org/fedora@sha256:' + 'b' * 64):
            with self.subTest(base=base), tempfile.TemporaryDirectory(
                prefix='prerequisite-image-test-'
            ) as scratch:
                context = Path(scratch) / 'context'
                manifest = IMAGE.assemble(context, base)
                self.assertEqual((context / 'Containerfile').read_text().splitlines()[0],
                                 'FROM ' + base)
                installed = context / 'rootfs/usr/share/tilo-native-prerequisites/source-inputs.json'
                self.assertEqual(json.loads(installed.read_text())['base'], base)
                self.assertEqual(json.loads((context / 'source-inputs.json').read_text()), manifest)
                self.assertFalse(manifest['qualification'])
                self.assertFalse(manifest['candidate_executables_included'])

    def test_mutable_or_injected_base_is_rejected_before_context_creation(self):
        with tempfile.TemporaryDirectory(prefix='prerequisite-image-test-') as scratch:
            context = Path(scratch) / 'context'
            for base in ('fedora:44', 'sha256:' + 'a' * 63,
                         'sha256:' + 'A' * 64, 'sha256:' + 'a' * 64 + '\nRUN true',
                         'registry.fedoraproject.org/fedora@sha256:' + 'b' * 64 + '\n'):
                with self.subTest(base=base), self.assertRaises(ValueError):
                    IMAGE.assemble(context, base)
                self.assertFalse(context.exists())


if __name__ == '__main__':
    unittest.main()

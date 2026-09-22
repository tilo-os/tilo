import argparse
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('candidate', Path(__file__).resolve().parents[1] / 'dev/wpe-candidate-receipt.py')
candidate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(candidate)


class CandidateReceiptTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        self.args = argparse.Namespace(architecture='x86_64', revision='b' * 40,
                                       run_id='123', attempt='2',
                                       reference=candidate.REPOSITORY + '@sha256:' + 'a' * 64,
                                       base='code.podesta.ai/qos/fedora-bootc@sha256:' + 'c' * 64,
                                       inspection=root / 'inspect.json', packages=root / 'packages',
                                       recipe=root / 'Containerfile')
        self.args.packages.write_bytes(b'aaa-0:1-1.x86_64\nbbb-0:2-1.noarch\n')
        self.args.recipe.write_bytes(b'fixture recipe\n')
        self.image = {'Architecture': 'amd64', 'Os': 'linux', 'RepoDigests': [self.args.reference],
                      'Config': {'Labels': {'org.opencontainers.image.revision': self.args.revision,
                                            'ai.tilo.wpe.base': self.args.base,
                                            'ai.tilo.wpe.purpose': 'development-candidate'}}}
        self.prepare()

    def prepare(self):
        self.args.tag = f'{candidate.REPOSITORY}:candidate-{self.args.revision}-123-2-{self.args.architecture}'
        self.args.inspection.write_text(json.dumps([self.image]))

    def test_both_native_architectures_record_candidate_not_production_promotion(self):
        for arch, oci in candidate.ARCHITECTURES.items():
            self.args.architecture = arch
            self.image['Architecture'] = oci
            self.prepare()
            result = candidate.receipt(self.args)
            self.assertEqual(result['architecture'], arch)
            self.assertEqual(result['packages'], 2)
            self.assertFalse(result['production_signed'])
            self.assertEqual(result['reference'], self.args.reference)

    def test_wrong_architecture_digest_and_revision_fail(self):
        for field, value in [('Architecture', 'arm64'), ('Os', 'windows'), ('RepoDigests', []), ('Config', {})]:
            original = self.image[field]
            self.image[field] = value
            self.prepare()
            with self.subTest(field=field), self.assertRaises(ValueError):
                candidate.receipt(self.args)
            self.image[field] = original

    def test_mutable_or_wrong_namespace_reference_and_reused_tag_fail(self):
        for field, value in [('reference', candidate.REPOSITORY + ':latest'),
                             ('reference', self.args.reference.replace('/tilo/', '/tilo/')),
                             ('tag', candidate.REPOSITORY + ':latest'), ('attempt', '0'),
                             ('revision', 'main'), ('base', 'fedora:latest')]:
            original = getattr(self.args, field)
            setattr(self.args, field, value)
            with self.subTest(field=field), self.assertRaises(ValueError):
                candidate.receipt(self.args)
            setattr(self.args, field, original)

    def test_empty_or_unsorted_package_inventory_fails(self):
        for value in (b'', b'z\na\n', b'a\n\nb\n'):
            self.args.packages.write_bytes(value)
            with self.assertRaises(ValueError):
                candidate.receipt(self.args)


if __name__ == '__main__':
    unittest.main()

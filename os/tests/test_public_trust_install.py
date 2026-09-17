#!/usr/bin/env python3
"""Test production preservation and validated development installation."""
import importlib.util
import json
from pathlib import Path
import shutil
import unittest
import test_development_trust as fixtures

spec = importlib.util.spec_from_file_location('install_public_trust', Path(__file__).resolve().parents[1] / 'dev/install-public-trust.py')
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)

class InstallTests(fixtures.PublicBundleTests):
    def setUp(self):
        super().setUp()
        self.official = Path(self.temporary.name) / 'official'
        self.official.mkdir()
        for name in installer.PRODUCTION_FILES:
            shutil.copyfile(self.output / name, self.official / name)
        self.destination = Path(self.temporary.name) / 'installed'
        self.digest = installer.trust.validate(self.output)['manifest_sha256']

    def run_install(self, source, mode, digest='', owner=''):
        installer.install(source, self.official, self.destination, mode, digest, owner)

    def test_production_preserves_exact_keys_and_no_marker(self):
        self.run_install(self.official, 'production')
        self.assertEqual(set(p.name for p in self.destination.iterdir()), set(installer.PRODUCTION_FILES))
        for name in installer.PRODUCTION_FILES:
            self.assertEqual((self.destination / name).read_bytes(), (self.official / name).read_bytes())

    def test_production_rejects_development_bundle(self):
        with self.assertRaises(ValueError):
            self.run_install(self.output, 'production')
        self.assertFalse(self.destination.exists())

    def test_production_rejects_substituted_keys(self):
        source = Path(self.temporary.name) / 'substitute'
        shutil.copytree(self.official, source)
        (source / 'cosign.pub').write_text('substituted')
        with self.assertRaisesRegex(ValueError, 'differ'):
            self.run_install(source, 'production')

    def test_development_installs_pinned_keys_and_public_provenance(self):
        self.run_install(self.output, 'development', self.digest, 'ab' * 32)
        self.assertEqual((self.destination / 'tpm2-pcr-signing.public.pem').read_bytes(), self.sources[1].read_bytes())
        provenance = json.loads((self.destination / 'development-trust/provenance.json').read_text())
        self.assertEqual(provenance['manifest_sha256'], self.digest)
        self.assertEqual(provenance['purpose'], 'tilo-development')

    def test_development_requires_pinned_digest_and_matching_owner(self):
        for digest, owner in (('', 'ab' * 32), ('0' * 64, 'ab' * 32), (self.digest, 'cd' * 32)):
            with self.subTest(digest=digest, owner=owner), self.assertRaises(ValueError):
                self.run_install(self.output, 'development', digest, owner)
        self.assertFalse(self.destination.exists())

    def test_image_trust_is_installed_before_dracut(self):
        containerfile = Path(__file__).resolve().parents[1] / 'Containerfile'
        source = containerfile.read_text()
        self.assertLess(source.index('RUN python3 /run/tilo-trust-tools/install-public-trust.py'), source.index('    dracut --force'))

if __name__ == '__main__':
    unittest.main()

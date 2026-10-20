#!/usr/bin/env python3
"""Development RPM trust boundaries; no package execution or compilation."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from restart_fixture import restart_fixture

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('wine_repository', ROOT / 'os/dev/development-wine-repository.py')
wine = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(wine)
REF = 'registry.tilo.so/tilo/wine-paired@sha256:' + '1' * 64


class DevelopmentWineTrust(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.pair = self.root / 'pair'
        (self.pair / 'rpms').mkdir(parents=True)
        self.pin = {'version': '11.0', 'build_release': '3.fc44.tilo_traditional1'}
        pin_file = self.root / 'wine-source.json'
        pin_file.write_text(json.dumps(self.pin))
        pin_patch = patch.object(wine, 'PIN', pin_file)
        pin_patch.start()
        self.addCleanup(pin_patch.stop)
        inventory = {}
        for arch in ('x86_64', 'i686'):
            name = f'wine-core-11.0-3.fc44.tilo_traditional1.{arch}.rpm'
            path = self.pair / 'rpms' / name
            path.write_bytes(b'unsigned fixture ' + arch.encode())
            inventory[name] = {'sha256': wine.digest(path), 'size': path.stat().st_size,
                'identity': ['wine-core', '11.0', self.pin['build_release'], arch, f"wine-11.0-{self.pin['build_release']}.src.rpm", 'LGPL']}
        self.record = {'schema': 1, 'source': self.pin, 'execution_validated': False,
                       'input_refs': {'x86_64': REF, 'i686': REF}, 'rpms': inventory}
        (self.pair / 'paired-contract.json').write_text(json.dumps(self.record))
        self.output = self.root / 'signed'

    def fake_command(self, *args, env=None):
        if args[0] in ('gpg', 'rpmsign'):
            home = Path(env['GNUPGHOME'])
            self.assertEqual(home.stat().st_mode & 0o777, 0o700)
            self.assertNotIn(self.output, home.parents)
            self.private_home = home
        if '--list-secret-keys' in args:
            return 'fpr:::::::::' + 'A' * 40 + ':\n'
        if '--export' in args:
            return 'public key fixture\n'
        if args[0] == 'rpmsign':
            path = Path(args[-1])
            path.write_bytes(path.read_bytes() + b' signed')
        if '--checksig' in args:
            return '    Header RSA Signature, key ID aaaaaaaa: OK\n'
        if args[0] == 'rpm':
            if '%{SOURCERPM}' in args[3]:
                return '\t'.join(self.record['rpms'][Path(args[-1]).name]['identity'])
            return 'bash'
        return ''

    def signed(self):
        with patch.object(wine, 'command', side_effect=self.fake_command), \
             patch.object(wine.subprocess, 'run'):
            return wine.sign(self.pair, REF, self.pin, self.output)

    def test_signing_lifecycle(self):
        with self.subTest(phase="signs_only_copies_and_removes_private_key_directory"):
            before = {p.name: p.read_bytes() for p in (self.pair / 'rpms').iterdir()}
            receipt = self.signed()
            self.assertFalse(self.private_home.exists())
            self.assertTrue(receipt['development_only'])
            self.assertEqual(receipt['public_key_sha256'], wine.digest(self.output / 'public.asc'))
            self.assertEqual({p.name for p in self.output.iterdir()},
                             {'rpms', 'public.asc', 'paired-contract.json', 'development-wine.json'})
            self.assertEqual(before, {p.name: p.read_bytes() for p in (self.pair / 'rpms').iterdir()})
        restart_fixture(self)
        with self.subTest(phase="signing_failure_removes_partial_export_and_private_keys"):
            def failing(*args, **kwargs):
                if args[0] == 'rpmsign':
                    raise ValueError('signing failure')
                return self.fake_command(*args, **kwargs)
            with patch.object(wine, 'command', side_effect=failing), patch.object(wine.subprocess, 'run'):
                with self.assertRaisesRegex(ValueError, 'signing failure'):
                    wine.sign(self.pair, REF, self.pin, self.output)
            self.assertFalse(self.output.exists())
            self.assertFalse(self.private_home.exists())
        restart_fixture(self)
        with self.subTest(phase="pair_changes_rejected_before_signing"):
            first = next((self.pair / 'rpms').iterdir())
            first.write_bytes(b'corruption')
            with patch.object(wine, 'command') as command, self.assertRaises(ValueError):
                wine.sign(self.pair, REF, self.pin, self.output)
            command.assert_not_called()
            self.assertFalse(self.output.exists())
        restart_fixture(self)
        with self.subTest(phase="independent_key_pin_and_rpm_hash_cannot_be_self_authorized"):
            receipt = self.signed()
            with patch.object(wine, 'command') as command, self.assertRaisesRegex(ValueError, 'independent pin'):
                wine.verify(self.output, '0' * 64)
            command.assert_not_called()
            first = next((self.output / 'rpms').iterdir())
            first.write_bytes(b'corrupted signed package')
            with patch.object(wine, 'command', side_effect=self.fake_command), self.assertRaisesRegex(ValueError, 'bytes differ'):
                wine.verify(self.output, receipt['public_key_sha256'])
        restart_fixture(self)
        with self.subTest(phase="non_wine_identity_is_never_signed"):
            first = next(iter(self.record['rpms'].values()))
            first['identity'][0] = 'bash'
            (self.pair / 'paired-contract.json').write_text(json.dumps(self.record))
            with patch.object(wine, 'command') as command, self.assertRaisesRegex(ValueError, 'traditional Wine'):
                wine.sign(self.pair, REF, self.pin, self.output)
            command.assert_not_called()

    def test_trust_database_separation(self):
        with self.subTest(phase="fedora_and_development_packages_use_separate_trust_databases"):
            receipt = self.signed()
            packages = self.output / 'rpms'
            (packages / 'bash-1-1.x86_64.rpm').write_bytes(b'Fedora RPM fixture')
            fedora_key = self.root / 'fedora.asc'
            fedora_key.write_text('Fedora key fixture')
            checked = []
            with patch.object(wine, 'command', side_effect=self.fake_command), \
                 patch.object(wine, 'verify_signature', side_effect=lambda p, db: checked.append((p.name, db.name))):
                wine.verify(self.output, receipt['public_key_sha256'], packages, fedora_key)
            self.assertEqual([role for name, role in checked if name.startswith('wine-core')], ['development'] * 2)
            self.assertEqual([role for name, role in checked if name.startswith('bash')], ['fedora'])
        restart_fixture(self)
        with self.subTest(phase="unsigned_digest_only_rpm_is_rejected"):
            with patch.object(wine, 'command', return_value='digests OK\n'), self.assertRaisesRegex(ValueError, 'no verified'):
                wine.verify_signature(Path('package.rpm'), Path('database'))
        restart_fixture(self)
        with self.subTest(phase="missing_core_or_fedora_substitution_rejected"):
            receipt = self.signed()
            packages = self.output / 'rpms'
            next(packages.glob('*.i686.rpm')).unlink()
            with patch.object(wine, 'command', side_effect=self.fake_command), self.assertRaisesRegex(ValueError, 'complete signed'):
                wine.verify(self.output, receipt['public_key_sha256'], packages)
            # Restore the pair before testing an additional forbidden replacement.
            original = next((self.pair / 'rpms').glob('*.i686.rpm'))
            (packages / original.name).write_bytes(original.read_bytes() + b' signed')
            (packages / 'wine-core-11.0-3.fc44.i686.rpm').write_bytes(b'stock package')
            key = self.root / 'fedora.asc'
            key.write_text('key fixture')
            def query(*args, **kwargs):
                return 'wine-core' if args[0] == 'rpm' else self.fake_command(*args, **kwargs)
            with patch.object(wine, 'command', side_effect=query), self.assertRaisesRegex(ValueError, 'replaced'):
                wine.verify(self.output, receipt['public_key_sha256'], packages, key)
        restart_fixture(self)
        with self.subTest(phase="pinned_wine_ntsync_subpackage_is_allowed"):
            identity = ['ntsync-autoload', '11.0', self.pin['build_release'], 'noarch',
                        f"wine-11.0-{self.pin['build_release']}.src.rpm", 'LGPL']
            wine.wine_identity(identity, self.pin)
            for index, value in ((0, 'ntsync-unrelated'), (4, 'unrelated.src.rpm'),
                                 (2, 'unreviewed-release')):
                changed = list(identity)
                changed[index] = value
                with self.subTest(index=index), self.assertRaisesRegex(ValueError, 'pinned traditional'):
                    wine.wine_identity(changed, self.pin)

    def test_consumer_rejections(self):
        with self.subTest(phase="parent_symlink_is_rejected"):
            link = self.root / 'linked-pair'
            link.symlink_to(self.pair, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, 'symlink'):
                wine.pair_inventory(link, self.pin)
        restart_fixture(self)
        with self.subTest(phase="changed_source_pin_is_rejected_by_consumer"):
            receipt = self.signed()
            wine.PIN.write_text(json.dumps(self.pin | {'build_release': 'different'}))
            with patch.object(wine, 'command') as command, self.assertRaisesRegex(ValueError, 'reviewed source'):
                wine.verify(self.output, receipt['public_key_sha256'])
            command.assert_not_called()


if __name__ == '__main__':
    unittest.main()

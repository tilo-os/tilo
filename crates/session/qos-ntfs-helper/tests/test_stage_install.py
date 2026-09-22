"""Real archive extraction and offline standalone dependency/build regression."""
import hashlib
import os
import pathlib
import subprocess
import tarfile
import tempfile
import tomllib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


class StandaloneBundle(unittest.TestCase):
    def test_source_bundle_contains_resolvable_pinned_parser_free_dependency(self):
        binary = os.environ['QOS_NTFS_STAGING_BINARY']
        with tempfile.TemporaryDirectory(prefix='qos-ntfs-stage2-fix1-bundle-') as directory:
            directory = pathlib.Path(directory)
            archives = [directory / name for name in ['first.tar', 'second.tar']]
            for archive in archives:
                subprocess.run(['python3', str(ROOT / 'stage_install.py'), '--binary', binary,
                                '--output', str(archive)], check=True)
            self.assertEqual(hashlib.sha256(archives[0].read_bytes()).digest(),
                             hashlib.sha256(archives[1].read_bytes()).digest())
            with tarfile.open(archives[0]) as archive:
                for member in archive.getmembers():
                    self.assertEqual((member.uid, member.gid, member.mtime), (0, 0, 0))
                    self.assertEqual(member.mode, 0o755 if member.name.endswith('/qos-ntfs-probe') else 0o644)
                archive.extractall(directory / 'extracted', filter='data')
            base = directory / 'extracted/usr/share/qos-ntfs-helper'
            helper = base / 'source'
            protocol = base / 'qos-ntfs-protocol'
            self.assertTrue((protocol / 'Cargo.toml').is_file(), 'parser-free dependency missing from standalone bundle')
            for crate in [helper, protocol]:
                manifest = tomllib.loads((crate / 'Cargo.toml').read_text())
                self.assertIn('workspace', manifest)
                self.assertNotIn('lints', manifest)
                for name, dependency in manifest['dependencies'].items():
                    if name == 'qos-ntfs-protocol':
                        self.assertEqual((crate / dependency['path']).resolve(), protocol.resolve())
                    else:
                        version = dependency if isinstance(dependency, str) else dependency['version']
                        self.assertTrue(version.startswith('='), f'{name} is not pinned')
                subprocess.run(['cargo', '+1.97.1', 'metadata', '--offline', '--format-version', '1',
                                '--manifest-path', str(crate / 'Cargo.toml')], check=True,
                               stdout=subprocess.DEVNULL)
            self.assertFalse((protocol / 'build.rs').exists())
            self.assertEqual((helper / 'vendor/ntfs-3g-source.tar.gz').read_bytes(),
                             (ROOT / 'vendor/ntfs-3g-source.tar.gz').read_bytes())
            subprocess.run(['cargo', '+1.97.1', 'build', '--offline', '--manifest-path',
                            str(helper / 'Cargo.toml'), '--bin', 'qos-ntfs-probe'], check=True)


if __name__ == '__main__':
    unittest.main()

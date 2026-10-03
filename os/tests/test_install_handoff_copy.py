"""Regression for private staging /etc replacing installed directory metadata."""
import importlib.util
import os
from pathlib import Path
import stat
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    'handoff_copy', ROOT / 'os/usr/libexec/tilo-install-handoff-copy.py')
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


@unittest.skipUnless(os.name == 'posix', 'installed Linux permission semantics')
class HandoffCopy(unittest.TestCase):
    def test_existing_directory_metadata_and_public_configuration_remain_readable(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, target = root / 'staging', root / 'installed'
            source.mkdir(mode=0o700)
            target.mkdir(mode=0o755)
            target.chmod(0o755)
            (source / 'NetworkManager').mkdir(mode=0o700)
            (target / 'NetworkManager').mkdir(mode=0o755)
            (target / 'profile').write_text('existing shell setup')
            (target / 'profile').chmod(0o644)
            (source / 'NetworkManager/profile').write_text('private network fixture')
            before = target.stat()
            os.setxattr(source, b'user.test_context', b'staging_var_run')
            os.setxattr(target, b'user.test_context', b'installed_etc')
            module.copy_tree(source, target)
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o755)
            self.assertEqual((target.stat().st_uid, target.stat().st_gid),
                             (before.st_uid, before.st_gid))
            self.assertEqual(os.getxattr(target, b'user.test_context'), b'installed_etc')
            self.assertEqual(stat.S_IMODE((target / 'NetworkManager').stat().st_mode), 0o755)
            self.assertEqual(stat.S_IMODE((target / 'profile').stat().st_mode), 0o644)
            self.assertEqual((target / 'profile').read_text(), 'existing shell setup')
            self.assertEqual(stat.S_IMODE((target / 'NetworkManager/profile').stat().st_mode), 0o600)
            backend = (ROOT / 'os/usr/libexec/tilo-install').read_text()
            self.assertIn('tilo-install-handoff-copy.py "$HANDOFF_SRC/etc" "$DEPLOY_ETC"', backend)
            self.assertIn('tilo-install-handoff-copy.py "$HANDOFF_SRC/var" "$HANDOFF_VAR"', backend)

    def test_new_structural_directories_are_traversable_and_files_private(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, target = root / 'source', root / 'target'
            (source / 'tilo').mkdir(parents=True, mode=0o700)
            target.mkdir()
            (source / 'tilo/request').write_text('fixture')
            previous = os.umask(0o077)
            try:
                module.copy_tree(source, target)
            finally:
                os.umask(previous)
            self.assertEqual(stat.S_IMODE((target / 'tilo').stat().st_mode), 0o755)
            self.assertEqual(stat.S_IMODE((target / 'tilo/request').stat().st_mode), 0o600)

    def test_linked_source_or_destination_cannot_escape_the_installed_tree(self):
        for linked_source in (True, False):
            with self.subTest(source=linked_source), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                source, target, outside = root / 'source', root / 'target', root / 'outside'
                for path in (source, target, outside):
                    path.mkdir()
                (outside / 'private').write_text('unchanged')
                if linked_source:
                    (source / 'private').symlink_to(outside / 'private')
                else:
                    (source / 'private').write_text('new')
                    (target / 'private').symlink_to(outside / 'private')
                with self.assertRaisesRegex(ValueError, 'symlinks'):
                    module.copy_tree(source, target)
                self.assertEqual((outside / 'private').read_text(), 'unchanged')


if __name__ == '__main__':
    unittest.main()

#!/usr/bin/env python3
"""sysext-image.py check over a tiny image, read as debugfs prints it: a
root-owned, labelled payload passes; one that would leave /usr owned by a build
user, unlabelled, writable or with another mode than the base's is refused,
naming the entry."""
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

SOURCE = Path(__file__).with_name('sysext-image.py')
SPEC = importlib.util.spec_from_file_location('sysext_image', SOURCE)
IMAGE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = IMAGE
SPEC.loader.exec_module(IMAGE)

USR = 'system_u:object_r:usr_t:s0'
LIB = 'system_u:object_r:lib_t:s0'


def image(owner='0/0', runner_mode='100755', lib_mode='040755', labels=None):
    """What debugfs answers for a small image: its directories and their labels."""
    labels = {2: 'system_u:object_r:root_t:s0', 11: 'system_u:object_r:lost_found_t:s0',
              12: USR, 13: LIB, 14: LIB, 15: LIB, 16: LIB} if labels is None else labels
    listings = {
        2: ['/2/040755/0/0/.//', '/2/040755/0/0/..//', '/11/040700/0/0/lost+found//',
            '/12/040755/0/0/usr//'],
        11: ['/11/040700/0/0/.//', '/2/040755/0/0/..//', '/0/000000/0/0///'],
        12: ['/12/040755/0/0/.//', '/2/040755/0/0/..//', f'/13/{lib_mode}/0/0/lib//'],
        13: [f'/13/{lib_mode}/0/0/.//', '/12/040755/0/0/..//', '/14/040755/0/0/tilo//'],
        14: ['/14/040755/0/0/.//', '/13/040755/0/0/..//',
             f'/15/{runner_mode}/{owner}/Program Files/12/', '/16/120777/0/0/current/1/'],
    }

    def debugfs(_image, commands, write=False):
        assert not write
        out = ''
        for command in commands:
            out += IMAGE.PROMPT + command + '\n'
            ino = int(command.split('<')[1].split('>')[0])
            if command.startswith('ls -p'):
                out += ''.join(line + '\n' for line in listings[ino]) + '\n'
            elif ino in labels:
                out += labels[ino] + '\n'
        return out
    return debugfs


class SysextImage(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory(prefix='sysext-image-')
        self.base = Path(self.dir.name)
        (self.base / 'usr/lib').mkdir(parents=True)
        (self.base / 'usr').chmod(0o755)
        (self.base / 'usr/lib').chmod(0o755)
        self.real = IMAGE.debugfs

    def tearDown(self):
        IMAGE.debugfs = self.real
        self.dir.cleanup()

    def found(self, **shape):
        IMAGE.debugfs = image(**shape)
        return IMAGE.check('tilo-windows-compat.raw', str(self.base))

    def test_a_root_owned_labelled_image_passes(self):
        self.assertEqual(self.found(), [])
        IMAGE.debugfs = image()
        self.assertEqual(IMAGE.main(['check', 'image.raw', '--base', str(self.base)]), 0)

    def test_every_entry_is_read_with_its_label(self):
        IMAGE.debugfs = image()
        read = {entry.path: entry for entry in IMAGE.entries('image.raw')}
        self.assertEqual(sorted(read), [
            '/', '/lost+found', '/usr', '/usr/lib', '/usr/lib/tilo',
            '/usr/lib/tilo/Program Files', '/usr/lib/tilo/current'])
        self.assertEqual(read['/usr'].label, USR)
        self.assertEqual(read['/usr/lib/tilo/Program Files'].mode, 0o100755)

    def test_an_entry_of_a_build_user_is_refused(self):
        found = self.found(owner='99/65534')
        self.assertEqual(found, ['/usr/lib/tilo/Program Files is owned by 99:65534, not root'])
        self.assertEqual(IMAGE.main(['check', 'image.raw', '--base', str(self.base)]), 1)

    def test_an_unlabelled_image_is_refused(self):
        self.assertIn('/usr has no SELinux label', self.found(labels={}))
        labels = {ino: LIB for ino in (2, 11, 13, 14, 15, 16)}
        labels[12] = 'system_u:object_r:unlabeled_t:s0'
        self.assertEqual(self.found(labels=labels), ['/usr is labelled system_u:object_r:unlabeled_t:s0'])

    def test_writable_or_set_id_entries_are_refused(self):
        self.assertEqual(self.found(runner_mode='104777'), [
            '/usr/lib/tilo/Program Files has mode 4777: writable by others or set-id'])

    def test_a_directory_of_the_base_keeps_its_mode(self):
        (self.base / 'usr/lib').chmod(0o555)
        self.assertEqual(self.found(), ["/usr/lib has mode 0755, the base's has 0555"])
        (self.base / 'usr/lib').chmod(0o755)

    def test_a_base_link_is_not_replaced_by_a_directory(self):
        (self.base / 'usr/lib/tilo').symlink_to('elsewhere')
        self.assertEqual(self.found(), ["/usr/lib/tilo would replace the base's lrwxrwxrwx entry"])

    def test_an_unreadable_listing_is_refused(self):
        with self.assertRaises(ValueError):
            list(IMAGE.listing(['/12/040755/usr//']))


if __name__ == '__main__':
    unittest.main()

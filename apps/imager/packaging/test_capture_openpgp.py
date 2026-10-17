import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('capture_openpgp', Path(__file__).with_name('capture_openpgp.py'))
subject = importlib.util.module_from_spec(spec)
spec.loader.exec_module(subject)


class ConfiguredEngine(unittest.TestCase):
    def sample(self):
        return b'openpgp\t/usr/bin/gpg2\nversion\t2.5.4\ngpgconf\t/usr/bin/gpgconf\nagent\t/usr/bin/gpg-agent\nlibexecdir\t/usr/libexec\n'

    def test_probe_fields_are_closed_not_guessed(self):
        result = subject.parse_probe(self.sample())
        self.assertEqual(result['openpgp'], '/usr/bin/gpg2')
        self.assertNotIn('keyboxd', result)
        for raw in [self.sample() + b'openpgp\t/usr/bin/gpg\n', self.sample() + b'override\t/tmp/helper\n', self.sample().replace(b'agent\t/usr/bin/gpg-agent\n', b''), self.sample() + b'x' * (32 * 1024)]:
            with self.assertRaises(ValueError):
                subject.parse_probe(raw)

    def test_packaging_resolves_alias_but_records_exact_configured_target(self):
        with tempfile.TemporaryDirectory() as root:
            binary = Path(root)/'gpg'
            binary.write_bytes(b'\x7fELF' + bytes(64))
            binary.chmod(0o755)
            alias = Path(root)/'gpg2'
            alias.symlink_to(binary.name)
            item = subject.helper('openpgp', str(alias))
            self.assertEqual(item['source'], str(binary.resolve()))
            self.assertEqual(item['target'], str(alias))
            self.assertEqual(item['bundle'], 'bin/gpg')
            binary.chmod(0o777)
            with self.assertRaises(ValueError):
                subject.helper('openpgp', str(alias))
            binary.chmod(0o755)
            binary.write_bytes(b'not ELF')
            with self.assertRaises(ValueError):
                subject.helper('openpgp', str(alias))


if __name__ == '__main__':
    unittest.main()

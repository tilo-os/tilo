"""Zero build admission on synthetic archives; no JVM, signer or account is run."""
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('signal_zero', ROOT / 'tools/apps/signal_zero_runtime.py')
zero = importlib.util.module_from_spec(spec)
spec.loader.exec_module(zero)


class SignalZeroRuntime(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.pin = copy.deepcopy(json.loads(zero.staging.PIN.read_text()))
        self.payload = self.root / 'payload'
        (self.payload / 'java/lib/server').mkdir(parents=True)
        self.upstream = ('JAVA_VERSION="' + self.pin['java_version'] + '"\n'
                         'SOURCE=".:git:' + self.pin['java_zero_source']['commit'][:12] + '"\n'
                         'IMPLEMENTOR="Eclipse Adoptium"\nJVM_VARIANT="Hotspot"\n')
        (self.payload / 'java/release').write_text(self.upstream)
        (self.payload / 'java/lib/server/libjvm.so').write_bytes(b'original VM')
        self.boot = self.root / 'boot'
        (self.boot / 'bin').mkdir(parents=True)
        (self.boot / 'bin/javac').write_bytes(b'fixture compiler')
        self.assets = self.root / 'assets'
        self.assets.mkdir()
        source = self.pin['java_zero_source']
        archive = self.assets / source['name']
        with tarfile.open(archive, 'w:gz') as output:
            entry = tarfile.TarInfo(source['root'] + '/configure')
            entry.size = 7
            output.addfile(entry, io.BytesIO(b'fixture'))
            contents = zero.ORIGINAL_RESERVATION.encode()
            entry = tarfile.TarInfo(source['root'] + '/' + zero.RESERVER)
            entry.size = len(contents)
            output.addfile(entry, io.BytesIO(contents))
        source['sha256'] = hashlib.sha256(archive.read_bytes()).hexdigest()

    def test_foreign_source_and_changed_archive_refuse_before_compilation(self):
        for alteration in ('source', 'archive'):
            with self.subTest(alteration=alteration), patch.object(zero.subprocess, 'run') as execute:
                if alteration == 'source':
                    (self.payload / 'java/release').write_text(self.upstream.replace('SOURCE=', 'FOREIGN='))
                else:
                    (self.payload / 'java/release').write_text(self.upstream)
                    (self.assets / self.pin['java_zero_source']['name']).write_bytes(b'changed source')
                with self.assertRaises(ValueError):
                    zero.build(self.root / 'build', self.payload, self.assets, self.pin)
                execute.assert_not_called()
                self.assertFalse((self.root / 'build').exists())

    def test_zero_build_preserves_upstream_and_binds_the_actual_vm_bytes(self):
        for name in ('classes.jsa', 'classes_nocoops.jsa'):
            (self.payload / 'java/lib/server' / name).write_bytes(b'HotSpot archive')
        vm = b'\x7fELF\x02\x01' + b'\0' * 12 + (62).to_bytes(2, 'little') + b'Zero VM'

        def compile_fixture(arguments, **kwargs):
            if arguments[0] == 'bash':
                self.assertIn('--with-jvm-variants=zero', arguments)
                modified = (kwargs['cwd'] / zero.RESERVER).read_text()
                self.assertIn('#ifdef ZERO', modified)
                self.assertIn('!ExecMem', modified)
                return
            self.assertEqual(arguments, ['make', 'hotspot', 'JOBS=2'])
            library = kwargs['cwd'] / 'build/linux-x86_64-zero-release/jdk/lib/zero/libjvm.so'
            library.parent.mkdir(parents=True)
            library.write_bytes(vm)

        with patch.object(zero, 'BOOT_JDK', self.boot), \
                patch.object(zero.subprocess, 'run', side_effect=compile_fixture):
            zero.build(self.root / 'build', self.payload, self.assets, self.pin)
        self.assertEqual((self.payload / 'java/upstream-release').read_text(), self.upstream)
        self.assertEqual((self.payload / 'java/lib/server/libjvm.so').read_bytes(), vm)
        self.assertFalse(list((self.payload / 'java/lib/server').glob('*.jsa')))
        zero.verify(self.payload, self.pin)
        (self.payload / 'java/lib/server/libjvm.so').write_bytes(b'changed VM')
        with self.assertRaisesRegex(ValueError, 'VM bytes changed'):
            zero.verify(self.payload, self.pin)
        (self.payload / 'java/lib/server/libjvm.so').write_bytes(vm)
        modified = self.payload / 'java-zero-source' / zero.RESERVER
        modified.write_text(modified.read_text().replace('!ExecMem', 'ExecMem'))
        with self.assertRaisesRegex(ValueError, 'data-only cache source differs'):
            zero.verify(self.payload, self.pin)


if __name__ == '__main__':
    unittest.main()

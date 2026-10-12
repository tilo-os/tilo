"""Verifier dependency regression; ELF fixture and isolated signature boundary, not native Tilo qualification."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
SCRIPT = Path(__file__).resolve().parents[1] / 'scripts/verify-account-consumer-capability.py'
spec = importlib.util.spec_from_file_location('account_capability', SCRIPT)
producer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(producer)
service_spec = importlib.util.spec_from_file_location('service_capability', SCRIPT.with_name('service-consumer-capability.py'))
service = importlib.util.module_from_spec(service_spec)
service_spec.loader.exec_module(service)

class OptionalServiceDependency(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        share = self.root / 'usr/share/tilo'
        share.mkdir(parents=True)
        (share / 'runtime-version').write_text('1+native.fixture\n')
        (share / 'runtime-bundle-publisher.hex').write_text('ab' * 32 + '\n')
        self.bundle = self.root / 'usr/lib/tilo-runtime/1+native.fixture'
        (self.bundle / 'lib').mkdir(parents=True)
        (self.bundle / 'bin').mkdir()
        self.library = self.bundle / 'lib/libruntime_dylib.so'
        self.probe = self.bundle / 'bin/tilo-runtime-account-capability'
        self.compile_library(1)
        source = self.root / 'probe.c'
        source.write_text('#include <stdio.h>\nextern int protocol(void);\n'
                          'int main(void) { printf("{\\"schema\\":1,\\"protected_grant_protocol\\":%d}\\n", protocol()); }\n')
        subprocess.run(['cc', str(source), '-o', str(self.probe), '-L'+str(self.library.parent),
                        '-lruntime_dylib'], check=True, capture_output=True)
        loader_name = producer.interpreter(self.probe)
        loader = self.bundle / loader_name
        loader.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(Path('/') / loader_name.removeprefix('lib/'), loader)
        report = subprocess.check_output(['ldd', str(self.probe)], text=True,
                                          env=dict(os.environ, LD_LIBRARY_PATH=str(self.library.parent)))
        import re
        for path in re.findall(r'(/[^\s()]+)', report):
            candidate = Path(path)
            if candidate.name != 'libruntime_dylib.so':
                shutil.copy2(candidate, self.library.parent / candidate.name)
        helper = self.root / 'usr/libexec/tilo-account-credential'
        helper.parent.mkdir(parents=True)
        helper.write_text('#!/bin/sh\n[ "$1" = capabilities ] || exit 64\nprintf \'{"schema":1,"protected_grant_protocol":1}\\n\'\n')
        helper.chmod(0o755)
        self.manifest = self.bundle / 'bundle.toml'
        self.manifest.write_text('version="1+native.fixture"\nroot_hash="'+'cd'*32+'"\n' +
                                 ''.join('[[files]]\npath="'+name+'"\n' for name in
                                         ('bin/tilo-runtime-account-capability', 'lib/libruntime_dylib.so', loader_name)))
        self.calls = []

    def compile_library(self, value):
        source = self.root / 'sdk.c'
        source.write_text('int protocol(void) { return '+str(value)+'; }\n')
        subprocess.run(['cc', '-shared', '-fPIC', '-Wl,-soname,libruntime_dylib.so',
                        str(source), '-o', str(self.library)], check=True, capture_output=True)

    def runner(self, command, env):
        self.calls.append(command)
        if command[0].endswith('/tilo-runtime-bundle'):
            # Crypto verification is exercised by runtime-bundle producer tests;
            # this seam isolates actual target-image linked execution.
            self.assertIn('--require-signature', command)
            self.assertIn('--require-publisher', command)
            return b''
        return producer.invoke(command, env)

    def test_cloud_consumer_checks_execute_without_optional_service_tools(self):
        receipt = producer.produce(self.root, self.runner, qualify_services=True)
        self.assertNotIn('service_sessions', receipt)
        self.assertEqual(receipt['protected_grant_protocol'], 1)
        self.assertEqual(receipt['runtime_probe_sha256'], hashlib.sha256(self.probe.read_bytes()).hexdigest())
        self.assertEqual(len(self.calls), 4)
        self.assertIn('--inhibit-cache', self.calls[1])
        self.assertEqual(self.calls[2][1:], ['capabilities'])

    def test_partial_optional_qualification_refuses_cloud_receipt(self):
        tools = ('tilo-service-session-receiver-qualification', 'tilo-service-session-qualification.py')
        for name in tools:
            path = self.root / 'usr/libexec' / name
            path.write_bytes(b'not qualification')
            path.chmod(0o755)
            with self.assertRaisesRegex(ValueError, 'incomplete optional'):
                producer.produce(self.root, self.runner, qualify_services=True)
            path.unlink()

    def test_saved_service_claim_cannot_downgrade_to_cloud_only(self):
        with self.assertRaisesRegex(ValueError, 'qualified sandbox'):
            service.produce(self.root, self.runner, (), {}, evidence={'schema': 1, 'protocol': 1})
        saved = {'receiving_shell_path': '/usr/bin/shell', 'receiving_shell_sha256': 'ab' * 32,
                 'receiving_route_qualification_sha256': 'ab' * 32}
        with self.assertRaisesRegex(ValueError, 'no longer installed'):
            service.produce(self.root, self.runner, (), {}, evidence=saved)

    def test_readonly_cloud_check_skips_optional_receivers(self):
        def forbidden(*args):
            self.fail('optional service qualification executed without request')
        self.assertIsNone(service.produce(self.root, forbidden, (), {}))

if __name__ == '__main__':
    unittest.main()

"""Production staging/mount contracts; no native execution is claimed here."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


BROWSER = load('capture_browser_pins', HERE / 'prepare-capture-browser.py')
IMAGE = load('capture_image_pins', HERE / 'capture-native-image.py')
QUALIFIER = load('capture_runtime_setup', HERE / 'qualify-capture-release.py')


class InstalledRuntimePins(unittest.TestCase):
    def test_native_setup_keeps_public_traversal_and_private_durable_logs(self):
        import shutil
        with tempfile.TemporaryDirectory() as scratch:
            base = Path(scratch)
            base = base.resolve()
            staged = base / 'staged'
            staged.mkdir()
            owned = base / 'owned'
            (base / 'evidence').mkdir()
            evidence = base / 'evidence/owned'
            paths = {'/owned': owned, '/evidence/owned': evidence}
            copytree = shutil.copytree
            prefix = QUALIFIER.CONTAINER_RUNNER.split('spec=importlib', 1)[0]
            previous = os.umask(0o077)
            try:
                with patch('pathlib.Path', side_effect=paths.__getitem__), \
                        patch('shutil.copytree', side_effect=lambda source, target: copytree(staged, target)), \
                        patch('os.getpid', return_value=1), patch('os.getuid', return_value=0), \
                        patch('os.geteuid', return_value=0):
                    exec(prefix, {})
            finally:
                os.umask(previous)
            self.assertEqual(owned.stat().st_mode & 0o777, 0o711)
            self.assertEqual((owned / 'candidate').stat().st_mode & 0o777, 0o755)
            self.assertEqual(evidence.stat().st_mode & 0o777, 0o700)
            self.assertIn("qualifier.execute(command,evidence/('command-'", QUALIFIER.CONTAINER_RUNNER)

    def test_exported_native_root_restores_readonly_trust_mounts(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch).resolve()
            share = root / 'usr/share/tilo'
            share.mkdir(parents=True)
            pins = [share / name for name in ('runtime-bundle-publisher.hex',
                    'browser-runtime-publisher.hex', 'registry-runtime-publisher.hex')]
            for pin in pins:
                pin.write_text('a' * 64 + '\n')
            retained = share / 'runtime-retained'
            retained.mkdir()
            with patch.object(IMAGE, 'bind') as bind:
                IMAGE.protect_runtime_trust(root)
            self.assertEqual(bind.call_args_list,
                             [unittest.mock.call(path, path, True) for path in (*pins, retained)])

    def test_actual_browser_stage_installs_with_readonly_component_pin(self):
        with tempfile.TemporaryDirectory() as scratch:
            base = Path(scratch)
            base = base.resolve()
            output = base / 'browser-release'
            output.mkdir()
            sdk = base / 'sidecar/runtime/sdk'
            (sdk / 'lib').mkdir(parents=True)
            (sdk / 'lib/libruntime_dylib.so').write_bytes(b'compiled runtime')
            (sdk / 'bin').mkdir()
            (sdk / 'bin/tilo-launcher').write_bytes(b'compiled launcher')
            (sdk / 'bundle.toml').write_text('version="sdk"\nroot_hash="root"\n')
            (output / 'cef').mkdir()
            (output / 'cef/bundle.toml').write_text('version="cef"\n')
            (base / 'pins.json').write_text(json.dumps(dict(runtime_version='sdk',
                runtime_publisher='a' * 64, runtime_root_hash='root')))
            manifest = b'[package]\nruntime="tilo-runtime/sdk"\noptional-runtimes=["tilo-runtime/cef"]\n'
            with tarfile.open(output / 'browser.tapp', 'w') as archive:
                item = tarfile.TarInfo('manifest.toml')
                item.size = len(manifest)
                archive.addfile(item, io.BytesIO(manifest))
            (output / 'cef.tapp').write_bytes(b'fixture package')
            digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
            surface = b'fixture surface'
            browser = b'compiled browser'
            (output / 'native-source-inputs.json').write_text(json.dumps({
                'apps/browser/surface.toml': {'sha256': hashlib.sha256(surface).hexdigest()}}))
            (output / 'receiving.json').write_text(json.dumps(dict(schema=1,
                browser_sha256=hashlib.sha256(browser).hexdigest(),
                packages={name: digest(output / name) for name in ('browser.tapp', 'cef.tapp')},
                cef_version='cef', cef_publisher='b' * 64, app_publisher='c' * 64)))
            host = base / 'imager'
            host.write_bytes(b'compiled host')
            cohort = dict(source_inputs={'apps/browser/surface.toml': hashlib.sha256(surface).hexdigest()},
                roles={'browser': {'sha256': hashlib.sha256(browser).hexdigest()},
                       'runtime': {'sha256': digest(sdk / 'lib/libruntime_dylib.so')},
                       'launcher': {'sha256': digest(sdk / 'bin/tilo-launcher')},
                       'host': {'path': str(host), 'sha256': digest(host)}})

            def run(command):
                if 'unpack' in command:
                    dest = Path(command[command.index('--dest') + 1])
                    (dest / 'bin').mkdir(parents=True)
                    (dest / 'bin/browser').write_bytes(browser)
                    (dest / 'surface.toml').write_bytes(surface)

            root = base / 'rootfs'
            (root / 'etc/tilo').mkdir(parents=True)
            (root / 'etc/tilo/runtime-bundle-trusted-publishers.txt').write_text('a' * 64 + '\n')
            previous = os.umask(0o077)
            try:
                with patch.object(BROWSER, 'run', side_effect=run):
                    recipe = BROWSER.stage(root, output, base / 'pins.json', cohort, qualification=True)
            finally:
                os.umask(previous)
            installs = [line for line in recipe.splitlines() if 'tilo-install install' in line]
            self.assertEqual(len(installs), 2)
            for line in installs:
                self.assertTrue(line.startswith('RUN --mount=type=bind,'
                    'source=rootfs/usr/share/tilo/browser-runtime-publisher.hex,'
                    'target=/usr/share/tilo/browser-runtime-publisher.hex,ro,relabel=shared '))
            self.assertTrue((root / 'usr/lib/tilo-runtime/sdk/lib/libruntime_dylib.so').exists())
            self.assertFalse((root / 'usr/lib/tilo-runtime/cef').exists())
            pin = root / 'usr/share/tilo/browser-runtime-publisher.hex'
            self.assertEqual(pin.read_text(), 'b' * 64 + '\n')
            self.assertEqual(pin.stat().st_mode & 0o777, 0o644)
            self.assertEqual(base.stat().st_mode & 0o777, 0o700)
            for directory in (root, *root.rglob('*')):
                if directory.is_dir():
                    self.assertEqual(directory.stat().st_mode & 0o777, 0o755)


if __name__ == '__main__':
    unittest.main()

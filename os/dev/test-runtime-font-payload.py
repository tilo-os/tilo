#!/usr/bin/env python3
"""The SDK's runtime fonts: the standalone catalog the signed payload ships, verified byte for
byte against the reviewed manifest. Apart from test-assemble-app-runtime.py so the two, each
over ten seconds, run side by side."""
import hashlib
import importlib.util
import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path

SOURCE = Path(__file__).with_name('assemble-app-runtime.py')
SPEC = importlib.util.spec_from_file_location('assemble_runtime', SOURCE)
ASSEMBLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ASSEMBLER)


class RuntimeFontPayload(unittest.TestCase):
    def test_sdk_catalog_is_the_verified_standalone_payload(self):
        repo = SOURCE.parents[2]
        with tempfile.TemporaryDirectory(prefix='runtime-fonts-') as name:
            fonts = ASSEMBLER.prepare_runtime_fonts(repo, Path(name))
            source = repo / 'crates/sdk/ui-core/assets/fonts/manifest.toml'
            self.assertEqual((fonts / 'manifest.toml').read_bytes(), source.read_bytes())
            catalogs = Path(name) / 'font-catalogs'
            catalog = ASSEMBLER.publish_runtime_fonts(repo, fonts, catalogs)
            self.assertEqual(catalog.name, hashlib.sha256(source.read_bytes()).hexdigest())
            self.assertEqual((catalog / 'manifest.toml').read_bytes(), source.read_bytes())
            manifest = tomllib.loads(source.read_text())
            for asset in manifest['font']:
                path = fonts / asset['file']
                if asset['scope'] != 'product':
                    self.assertFalse(path.exists(), asset['file'])
                    continue
                self.assertEqual(path.stat().st_size, asset['bytes'])
                self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), asset['sha256'])
                self.assertEqual((catalog / asset['file']).read_bytes(), path.read_bytes())
                self.assertIn(asset['license_spdx'], {'OFL-1.1', 'LPPL-1.3c'})
            target = fonts / 'NotoSansMath-Regular.ttf'
            changed = bytearray(target.read_bytes())
            changed[-1] ^= 1
            target.write_bytes(changed)
            result = subprocess.run(['python3', str(repo / 'tools/font-catalog.py'),
                'verify', str(fonts), '--scope', 'product'], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('SHA-256', result.stderr)
            with self.assertRaises(subprocess.CalledProcessError):
                ASSEMBLER.publish_runtime_fonts(repo, fonts, Path(name) / 'corrupt')
            self.assertFalse((Path(name) / 'corrupt').exists())
            (catalog / 'manifest.toml').write_text('schema=1\n')
            result = subprocess.run(['python3', str(repo / 'tools/font-catalog.py'),
                'verify', str(catalog), '--scope', 'product'], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('manifest differs', result.stderr)


if __name__ == '__main__':
    unittest.main()

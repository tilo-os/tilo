"""Exercise the real publisher's Signal identity admission without network or keys."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]


class SignalProviderPublication(unittest.TestCase):
    def test_runtime_packer_requires_payload_bound_loader_declaration(self):
        # Synthetic tools inspect the production packer's inputs; no identity
        # is created and no fixture package is published or admitted as trusted.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bundle = root / 'bundle'
            (bundle / 'lib/lib64').mkdir(parents=True)
            (bundle / 'lib/signal-provider').mkdir()
            (bundle / 'bundle.toml').write_text('version = "2026.10.1-signal+native.'
                                               + 'a' * 32 + '"\narch = "x86_64"\n')
            loader = bundle / 'lib/lib64/ld-linux-x86-64.so.2'
            loader.write_bytes(b'synthetic loader')
            loader.chmod(0o755)
            marker = bundle / 'lib/signal-provider/runtime-entrypoint.txt'
            binaries = root / 'bin'
            binaries.mkdir()
            runtime_cli = binaries / 'tilo-runtime-bundle'
            runtime_cli.write_text('#!' + sys.executable + '\n'
                                   'import sys\n'
                                   'assert sys.argv[1] in ("verify", "key")\n'
                                   'if sys.argv[1] == "key": print("f" * 64)\n')
            runtime_cli.chmod(0o755)
            package_cli = binaries / 'tilo-app-package'
            package_cli.write_text('#!' + sys.executable + '\n'
                                   'import os, pathlib, shutil, sys\n'
                                   'a = sys.argv\n'
                                   'if a[1] == "pack":\n'
                                   ' shutil.copyfile(a[a.index("--manifest")+1], os.environ["PACK_MANIFEST"])\n'
                                   ' pathlib.Path(a[a.index("--output")+1]).write_bytes(b"fixture")\n'
                                   'elif a[1] == "split":\n'
                                   ' p = pathlib.Path(a[a.index("--out")+1]); p.mkdir(exist_ok=True)\n'
                                   ' shutil.copyfile(a[2], p / pathlib.Path(a[2]).name)\n'
                                   'else: assert a[1] == "verify"\n')
            package_cli.chmod(0o755)
            captured = root / 'packed-manifest.toml'
            environment = dict(os.environ, PACK_MANIFEST=str(captured))
            for declaration in (None, 'lib/other\n', 'lib/lib64/ld-linux-x86-64.so.2\n'):
                with self.subTest(declaration=declaration):
                    marker.unlink(missing_ok=True)
                    captured.unlink(missing_ok=True)
                    if declaration is not None:
                        marker.write_text(declaration)
                    result = subprocess.run(['bash', str(ROOT / 'tools/apps/pack-runtime-package.sh'),
                                             str(bundle), 'so.tilo.runtime.signal', 'Signal runtime',
                                             '0.1.1', str(root / 'unused-key'), str(binaries), str(root / 'out')],
                                            env=environment, capture_output=True, text=True, timeout=10)
                    if declaration == 'lib/lib64/ld-linux-x86-64.so.2\n':
                        self.assertEqual(result.returncode, 0, result.stderr)
                        manifest = tomllib.loads(captured.read_text())
                        self.assertEqual(manifest['entrypoint'], {
                            'type': 'cli', 'bin': 'lib/lib64/ld-linux-x86-64.so.2'})
                        self.assertEqual(manifest['package']['kind'], 'runtime')
                    else:
                        self.assertNotEqual(result.returncode, 0)
                        self.assertFalse(captured.exists(), 'unbound declaration reached packing')

    def test_only_exact_native_signal_runtime_reaches_catalogue_lookup(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            package = root / 'package'
            package.mkdir()
            (package / 'fixture.tapp').write_bytes(b'synthetic package')
            binary = root / 'bin'
            binary.mkdir()
            cli = binary / 'package-cli'
            cli.write_text('#!' + sys.executable + '\n'
                           'import os, pathlib, sys\n'
                           'assert sys.argv[1] == "inspect"\n'
                           'print(pathlib.Path(os.environ["PUBLISH_MANIFEST"]).read_text())\n')
            cli.chmod(0o755)
            curl = binary / 'curl'
            curl.write_text('#!' + sys.executable + '\n'
                            'import os, pathlib, sys\n'
                            'args = sys.argv[1:]\n'
                            'assert "--request" not in args\n'
                            'assert "https://registry.invalid/entries" in args\n'
                            'pathlib.Path(args[args.index("--output") + 1]).write_text('\
                            'pathlib.Path(os.environ["PUBLISH_SNAPSHOT"]).read_text())\n'
                            'pathlib.Path(os.environ["PUBLISH_CALLS"]).write_text("catalogue read")\n')
            curl.chmod(0o755)
            manifest = root / 'manifest.toml'
            snapshot = root / 'snapshot.json'
            calls = root / 'calls'
            version = '0.1.1+g' + 'a' * 40
            exact = 'tilo-runtime/2026.10.1-signal+native.' + 'a' * 32
            environment = dict(os.environ, PATH=str(binary) + os.pathsep + os.environ['PATH'],
                               VERSION=version, TILO_PACKAGE_TOKEN='synthetic-fixture',
                               PACKAGE_API='https://packages.invalid', REGISTRY_URL='https://registry.invalid',
                               OWNER='fixture', REGISTRY_CLI=str(cli), REGISTRY_IDENTITY=str(root / 'unused-key'),
                               RUNNER_TEMP=str(root), PUBLISH_MANIFEST=str(manifest),
                               PUBLISH_SNAPSHOT=str(snapshot), PUBLISH_CALLS=str(calls))
            for runtime in (exact, 'tilo-runtime/2026.10.1-signal',
                            exact[:-1], exact.upper(), exact.replace('signal', 'other')):
                with self.subTest(runtime=runtime):
                    calls.unlink(missing_ok=True)
                    manifest.write_text('runtime = "' + runtime + '"\nversion = "' + version + '"\n')
                    snapshot.write_text(json.dumps({'entries': [{
                        'package_id': 'so.tilo.signal-provider', 'versions': [{
                            'version': version, 'target': {'runtime': runtime, 'architecture': 'x86_64'},
                        }],
                    }]}))
                    result = subprocess.run(['bash', str(ROOT / 'tools/apps/publish-runtime-package.sh'),
                                             str(package), 'x86_64', 'so.tilo.signal-provider',
                                             'fixture-provider', str(root / 'unused-metadata')],
                                            env=environment, capture_output=True, text=True, timeout=10)
                    if runtime == exact:
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertEqual(calls.read_text(), 'catalogue read')
                        self.assertIn('already offers ' + exact, result.stdout)
                    else:
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn('exact native build', result.stderr)
                        self.assertFalse(calls.exists(), 'invalid runtime reached a network action')


if __name__ == '__main__':
    unittest.main()

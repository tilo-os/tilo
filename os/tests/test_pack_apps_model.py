"""An image package carries the model module its app declares.

Every device holding a workspace's keys applies an installed app's actions from
the `model.wasm` its package carries (`crates/sdk/tilo-model-guest`,
`tilo-model-engine`). An app keeps one by keeping a guest crate beside it
(`apps/poker` -> `apps/poker-model-wasm`); the module itself is the app-models
check's output, which the image's packing must stage into the payload without
building it again. An app that declares a model and has no module built is not
shipped: nothing could apply its actions.
"""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / 'os/scripts/pack-apps.sh'
MODULE = b'the wasm module a device applies the app actions with'
APP_ID = 'so.tilo.poker'


class Packer:
    """A pack-apps.sh packing over a fixture tree, with the tools it calls."""

    def __init__(self, root):
        self.root = root
        self.repo = root / 'repo'
        self.bins = root / 'bin'
        self.store = root / 'store'
        self.scratch = root / 'scratch'
        self.packed = root / 'packed'
        self.models = root / 'models'
        self.repo.mkdir()
        self.bins.mkdir()
        self.store.mkdir()
        self.scratch.mkdir()
        self.models.mkdir()
        (self.store / 'registry.toml').write_text('[[entry]]\nid="existing"\n')
        (self.repo / 'os/scripts').mkdir(parents=True)
        (self.repo / 'tools').symlink_to(REPO / 'tools')
        app = self.repo / 'apps/poker'
        app.mkdir(parents=True)
        shutil.copy(REPO / 'apps/poker/tilo-app.toml', app / 'tilo-app.toml')
        shutil.copy(REPO / 'apps/poker/surface.json', app / 'surface.json')
        (self.repo / 'apps/poker-model-wasm').mkdir()
        (self.repo / 'apps/poker-model-wasm/Cargo.toml').write_text(
            '[package]\nname = "poker-model-wasm"\nversion = "0.1.0"\n')
        (self.bins / 'poker').write_text('#!/bin/sh\n')
        (self.bins / 'poker').chmod(0o755)
        bundle = root / 'bundle'
        bundle.write_text('''#!/bin/bash
set -eu
case "$2" in
 generate) printf synthetic-private-key > "$4" ;;
 pubkey) printf '%064d\\n' 1 ;;
 *) exit 18 ;;
esac
''')
        pack = root / 'pack'
        pack.write_text(f'''#!/bin/bash
set -eu
[ "$1" = pack ] || exit 0
while [ $# -gt 0 ]; do
  case "$1" in
    --payload) cp -a "$2" '{self.packed}'; shift ;;
    --output) printf tapp > "$2"; shift ;;
  esac
  shift
done
''')
        install = root / 'install'
        install.write_text(f'''#!/bin/sh
printf '[[entry]]\\nid="{APP_ID}"\\n' >> '{self.store}/registry.toml'
''')
        for tool in (bundle, pack, install):
            tool.chmod(0o755)
        self.bundle, self.package_bin, self.install = bundle, pack, install

    def with_module(self):
        (self.models / 'poker').mkdir()
        (self.models / 'poker/model.wasm').write_bytes(MODULE)
        return self

    def environment(self):
        return {'PATH': os.defpath, 'HOME': str(self.root), 'TMPDIR': str(self.scratch),
                'REPO': str(self.repo), 'STORE': str(self.store), 'BIN_DIR': str(self.bins),
                'APP_MODELS': str(self.models), 'BUNDLE_BIN': str(self.bundle),
                'PKG_BIN': str(self.package_bin), 'INSTALL_BIN': str(self.install),
                'SHIM_BIN': '/missing',
                'RUNTIME_ID': 'tilo-runtime/2026.07+native.' + '0' * 32}

    def pack(self, **overrides):
        environment = self.environment() | overrides
        child = subprocess.Popen(['bash', str(SCRIPT)], env=environment,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        stdout, stderr = child.communicate()
        return subprocess.CompletedProcess(child.args, child.returncode, stdout, stderr)


class ModelTests(unittest.TestCase):
    def test_a_source_built_image_app_ships_the_module_it_declares(self):
        with tempfile.TemporaryDirectory() as directory:
            packer = Packer(Path(directory)).with_module()
            result = packer.pack()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((packer.packed / 'model.wasm').read_bytes(), MODULE,
                             'the payload carries the module the app-models check built')

    def test_an_app_that_declares_a_model_without_one_built_is_not_shipped(self):
        with tempfile.TemporaryDirectory() as directory:
            result = Packer(Path(directory)).pack()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('declares a model', result.stderr)

    def test_a_prebuilt_package_of_a_model_app_without_its_module_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            packer = Packer(Path(directory))
            tapps = packer.root / 'tapps'
            tapps.mkdir()
            (tapps / 'poker.tapp').write_text('tapp')

            def prebuilt(listing):
                (packer.root / 'pack').write_text(f'''#!/bin/bash
set -eu
case "$1" in
 verify) exit 0 ;;
 inspect) cat <<'DETAILS'
id  = "{APP_ID}"
identity  = "ed25519:{'1' * 64}"
--- payload (1 files) ---
{listing}
DETAILS
         ;;
 *) exit 0 ;;
esac
''')
                return packer.pack(TAPP_DIR=str(tapps))

            refused = prebuilt('  bin/poker  (blake3:abc, 16 bytes)')
            self.assertNotEqual(refused.returncode, 0)
            self.assertIn('carries no model.wasm', refused.stderr)

            accepted = prebuilt('  model.wasm  (blake3:abc, 44 bytes)\n'
                                '  bin/poker  (blake3:def, 16 bytes)')
            self.assertEqual(accepted.returncode, 0, accepted.stderr)


if __name__ == '__main__':
    unittest.main()

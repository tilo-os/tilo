"""An image app built from source ships its public surface, as a release one does.

The shell decides a room's writes of an app against the surface its package
ships and names it in the app's own writes. An image package without it
holds every room write naming one, so the image's packing must stage it.
"""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / 'os/scripts/pack-apps.sh'


class SurfaceTests(unittest.TestCase):
    def test_a_source_built_image_app_ships_its_surface(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            app = root / 'repo/apps/chat'
            app.mkdir(parents=True)
            shutil.copy(REPO / 'apps/chat/tilo-app.toml', app / 'tilo-app.toml')
            shutil.copy(REPO / 'apps/chat/surface.toml', app / 'surface.toml')
            (root / 'repo/os/scripts').mkdir(parents=True)
            (root / 'repo/tools').symlink_to(REPO / 'tools')
            bins = root / 'bin'
            bins.mkdir()
            (bins / 'chat').write_text('#!/bin/sh\n')
            (bins / 'chat').chmod(0o755)
            store, scratch, packed = root / 'store', root / 'scratch', root / 'packed'
            store.mkdir()
            scratch.mkdir()
            (store / 'registry.toml').write_text('[[entry]]\nid="existing"\n')
            bundle = root / 'bundle'
            bundle.write_text('''#!/bin/bash
set -eu
case "$2" in
 generate) printf synthetic-private-key > "$4" ;;
 pubkey) printf '%064d\\n' 1 ;;
 *) exit 18 ;;
esac
''')
            # Packing records the payload it was handed.
            pack = root / 'pack'
            pack.write_text(f'''#!/bin/bash
set -eu
[ "$1" = pack ] || exit 0
while [ $# -gt 0 ]; do
  case "$1" in
    --payload) cp -a "$2" '{packed}'; shift ;;
    --output) printf tapp > "$2"; shift ;;
  esac
  shift
done
''')
            # Installing records the package in the store, as tilo-install does.
            install = root / 'install'
            install.write_text(f'''#!/bin/sh
printf '[[entry]]\\nid="so.tilo.chat"\\n' >> '{store}/registry.toml'
''')
            for tool in (bundle, pack, install):
                tool.chmod(0o755)
            environment = {'PATH': os.defpath, 'HOME': str(root), 'TMPDIR': str(scratch),
                           'REPO': str(root / 'repo'), 'STORE': str(store), 'BIN_DIR': str(bins),
                           'BUNDLE_BIN': str(bundle), 'PKG_BIN': str(pack),
                           'INSTALL_BIN': str(install), 'SHIM_BIN': '/missing',
                           'RUNTIME_ID': 'tilo-runtime/2026.07+native.' + '0' * 32}
            result = subprocess.run(['bash', str(SCRIPT)], env=environment,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((packed / 'surface.toml').read_bytes(),
                             (REPO / 'apps/chat/surface.toml').read_bytes(),
                             'the payload carries the committed surface')
            self.assertTrue((packed / 'bin/chat').is_file())


if __name__ == '__main__':
    unittest.main()

"""Command-construction/assembly tests; fake compiler output is not qualification."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[4]
ASSETS = ('compute-worker.mjs', 'compute-domains.mjs', 'compute-protocol.mjs',
          'pkg/compute/tilo_browser_compute.js', 'pkg/compute/tilo_browser_compute_bg.wasm')


class CanonicalPackaging(unittest.TestCase):
    def test_runtime_builder_includes_compute_from_locked_canonical_workspace(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            tools = temp / 'bin'
            tools.mkdir()
            scripts = {
                'cargo': '#!/bin/sh\nif [ "$1" = metadata ]; then printf \'{"target_directory":"%s"}\\n\' "$FAKE_TARGET"; else printf "%s\\n" "$*" >> "$FAKE_LOG"; fi\n',
                'wasm-bindgen': '#!/usr/bin/env python3\nimport os,sys\nfrom pathlib import Path\nif "--version" in sys.argv: print("wasm-bindgen 0.2.108");sys.exit()\na=sys.argv;out=Path(a[a.index("--out-dir")+1]);out.mkdir(parents=True,exist_ok=True);name=a[a.index("--out-name")+1]\n(out/(name+".js")).write_text("export default async()=>{};")\n(out/(name+"_bg.wasm")).write_bytes(bytes([0,97,115,109,1,0,0,0,5,5,1,1,18,128,32]) + (bytes([0,128,128,64])+bytes(1024*1024) if name=="tilo_browser_compute" else b""))\n',
                'npm': '#!/bin/sh\nexit 0\n',
                'wasm-opt': '#!/bin/sh\n[ \"$1\" != --version ] || echo wasm-opt-fixture\n',
                'node': '#!/usr/bin/env python3\nimport os,sys\nfrom pathlib import Path\nif len(sys.argv)>1 and sys.argv[1]=="build.mjs": Path(sys.argv[2]).write_text("export {};")\nelse: os.execv(os.environ["REAL_NODE"],[os.environ["REAL_NODE"],*sys.argv[1:]])\n',
            }
            for name, source in scripts.items():
                path = tools / name
                path.write_text(source)
                path.chmod(0o755)
            env = dict(os.environ, PATH=f'{tools}:{os.environ["PATH"]}',
                       FAKE_TARGET=str(temp / 'target'), FAKE_LOG=str(temp / 'cargo.log'),
                       REAL_NODE=shutil.which('node'), WASM_BINDGEN=str(tools / 'wasm-bindgen'))
            out = temp / 'bundle'
            subprocess.run(['bash', str(ROOT / 'crates/browser/tilo-browser-runtime/scripts/build.sh'),
                            str(out), '--profile', 'dev'], cwd=ROOT, env=env, check=True,
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            for name in ASSETS:
                self.assertTrue((out / name).is_file(), name)
            metadata = json.loads((out / 'build.json').read_text())
            for name in ASSETS:
                self.assertIn(name, metadata['files'])
            self.assertIn('--locked -p tilo-browser-compute', (temp / 'cargo.log').read_text())
            self.assertFalse((out / 'browser-page.mjs').exists())
            # Fake artifact is deliberately >1 MiB: accepted in dev, refused for release.
            self.assertGreater((out / ASSETS[-1]).stat().st_size, 1024 * 1024)
            release = temp / 'release'
            result = subprocess.run(['bash', str(ROOT / 'crates/browser/tilo-browser-runtime/scripts/build.sh'),
                                     str(release), '--profile', 'release'], cwd=ROOT, env=env,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b'1 MiB release compute budget', result.stderr)
            self.assertFalse((release / 'build.json').exists())


if __name__ == '__main__':
    unittest.main()

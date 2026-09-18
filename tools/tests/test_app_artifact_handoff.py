#!/usr/bin/env python3
"""Run the real shell packaging -> workflow upload -> collection assembly path.

Only compilation/ELF/cryptographic executables are fixtures. This proves artifact
layout and transport completeness, not native compilation or signature validity.
"""
import glob
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class AppArtifactHandoffTests(unittest.TestCase):
    def setUp(self):
        for tool in ("bash", "jq", "tar", "sha256sum", "python3"):
            self.assertIsNotNone(shutil.which(tool), f"{tool} is required for the artifact handoff test")
        self.temp = tempfile.TemporaryDirectory(prefix="tilo-app-handoff-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for relative in (
            "dist/apps/package-apps.sh", "dist/apps/assemble-collection.sh",
            "os/scripts/bake-runtime-bundle.sh", "tools/app_release.py",
            "tools/ci_select.py", "tools/assemble_app_collection.py",
            "apps/calculator/Cargo.toml", "apps/calculator/tilo-app.toml",
            "release/app-collections.toml",
        ):
            self.write(relative, (ROOT / relative).read_text())
        self.write("bin/cargo", "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FIXTURE_CARGO_LOG\"\n")
        self.write("bin/rustc", "#!/bin/sh\nprintf '%s\\n' \"$FIXTURE_STDLIB\"\n")
        self.write("bin/uname", "#!/bin/sh\nprintf '%s\\n' \"$FIXTURE_ARCH\"\n")
        self.write("stdlib/libstd-fixture.so", "fixture standard library")
        self.write("os/scripts/build-thin-apps.sh", """#!/bin/bash
set -euo pipefail
mkdir -p target/thin-app
printf 'fixture calculator payload' > target/thin-app/calculator
chmod +x target/thin-app/calculator
printf 'fixture runtime dylib' > target/thin-app/libruntime_dylib.so
""")
        self.write("os/scripts/verify-elf-hardening.sh", "#!/bin/bash\nset -euo pipefail\nfor f; do test -x \"$f\"; done\n")
        self.write("target/release/tilo-runtime-bundle", """#!/usr/bin/env python3
import pathlib, sys
a = sys.argv[1:]
if a[:2] == ['key', 'generate']:
    pathlib.Path(a[a.index('--out')+1]).write_text('PRIVATE_TEST_IDENTITY')
elif a[:2] == ['key', 'pubkey']:
    print('ab'*32)
elif a[0] == 'build':
    out = pathlib.Path(a[a.index('--out')+1]); out.mkdir(parents=True)
    (out/'bundle.toml').write_text('version="2026.07"\\nroot_hash="'+'cd'*32+'"\\n')
    (out/'lib').mkdir(); (out/'lib/runtime.so').write_bytes(b'runtime payload')
elif a[0] == 'verify':
    assert (pathlib.Path(a[1])/'bundle.toml').is_file()
else:
    raise SystemExit('unexpected runtime fixture invocation')
""")
        self.write("target/release/tilo-pkg", """#!/usr/bin/env python3
import pathlib, sys
a = sys.argv[1:]
if a[0] == 'pack':
    payload = pathlib.Path(a[a.index('--payload')+1])/'bin/calculator'
    pathlib.Path(a[a.index('--output')+1]).write_bytes(payload.read_bytes())
elif a[0] == 'verify':
    assert pathlib.Path(a[-1]).read_bytes() == b'fixture calculator payload'
else:
    raise SystemExit('unexpected qpkg fixture invocation')
""")
        self.env = dict(os.environ, PATH=str(self.root / "bin") + os.pathsep + os.environ["PATH"],
                        FIXTURE_CARGO_LOG=str(self.root / "cargo.log"),
                        FIXTURE_STDLIB=str(self.root / "stdlib"), TILO_RELEASE_APPS="calculator",
                        SOURCE_DATE_EPOCH="123", GITHUB_SHA="1" * 40)
        for name in ("CARGO_TARGET_DIR", "TILO_APP_RELEASE_OUT", "QPKG_PUBLISHER_IDENTITY_FILE",
                     "RUNTIME_BUNDLE_IDENTITY_FILE", "TRUST_LIST", "MAX_QPKG_BYTES"):
            self.env.pop(name, None)

    def write(self, relative, content):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8", newline="\n")
        path.chmod(0o755)

    def command(self, *command, env=None):
        result = subprocess.run(command, cwd=self.root, env=env or self.env,
                                text=True, capture_output=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def test_native_outputs_survive_actual_upload_selection_and_collection_assembly(self):
        for architecture in ("x86_64", "aarch64"):
            with self.subTest(architecture=architecture):
                self.env["FIXTURE_ARCH"] = architecture
                self.command("bash", "dist/apps/package-apps.sh")
                out = self.root / "dist/app-release"
                artifact = self.root / "download" / architecture
                artifact.mkdir(parents=True)
                # Read the actual upload patterns, including the common directory
                # prefix which upload-artifact strips from the transported paths.
                patterns = re.findall(r"^\s+dist/app-release/(\S+)\s*$",
                                      (ROOT / ".forgejo/workflows/release-apps.yml").read_text(), re.M)
                self.assertTrue(patterns, "workflow upload selection is missing")
                for pattern in patterns:
                    for name in glob.glob(str(out / pattern), recursive=True):
                        source = Path(name)
                        if source.is_file():
                            target = artifact / source.relative_to(out)
                            target.parent.mkdir(parents=True, exist_ok=True)
                            shutil.copyfile(source, target)
                self.assertTrue((artifact / "runtime/tilo-runtime/2026.07/bundle.toml").is_file(),
                                "workflow did not transport the produced runtime")
                self.assertEqual((artifact / "runtime/runtime-bundle-publisher.hex").read_text().strip(), "ab" * 32)
                sums = self.command("bash", "-c", 'cd "$1" && sha256sum --check --strict SHA256SUMS',
                                    "verify-download", str(artifact))
                self.assertIn("runtime-bundle-publisher.hex: OK", sums.stdout)
                for file in artifact.rglob("*"):
                    if file.is_file():
                        self.assertNotIn(b"PRIVATE_TEST_IDENTITY", file.read_bytes())
                collection = self.root / "dist" / f"collection-{architecture}"
                env = dict(self.env, TILO_APP_COLLECTION_PLAN=str(out / "plan.json"),
                           TILO_APP_ARCH=architecture, TILO_APP_QPKG_DIR=str(artifact / "qpkg"),
                           TILO_APP_RUNTIME_DIR=str(artifact / "runtime"),
                           TILO_APP_COLLECTION_OUT=str(collection))
                calls_before = (self.root / "cargo.log").read_text()
                self.command("bash", "dist/apps/assemble-collection.sh", env=env)
                self.assertEqual((self.root / "cargo.log").read_text(), calls_before,
                                 "collection assembly invoked Cargo")
                manifest = json.loads((collection / "index.json").read_text())
                self.assertEqual(manifest["runtime"]["publisher"], "ab" * 32)
                self.assertEqual(manifest["architecture"], architecture)
                self.command("bash", "-c", 'cd "$1" && sha256sum --check --strict SHA256SUMS',
                             "verify-collection", str(collection))
                archive = collection / f"tilo-apps-{architecture}.tar.gz"
                first = hashlib.sha256(archive.read_bytes()).hexdigest()
                self.command("bash", "dist/apps/assemble-collection.sh", env=env)
                self.assertEqual(hashlib.sha256(archive.read_bytes()).hexdigest(), first)
                with tarfile.open(out / f"tilo-apps-{architecture}.tar.gz") as bundle:
                    self.assertIn("runtime/runtime-bundle-publisher.hex", bundle.getnames())
                    self.assertNotIn("work", bundle.getnames())


if __name__ == "__main__":
    unittest.main()

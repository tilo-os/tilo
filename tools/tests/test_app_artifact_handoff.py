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
import tomllib
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
            "os/scripts/bake-runtime-bundle.sh", "tools/apps/app_release.py",
            "os/scripts/collect-runtime-dependencies.py",
            "os/scripts/verify-runtime-bundle-closure.sh",
            "os/scripts/stamp-runtime.sh", "tools/apps/pack-runtime-package.sh",
            "tools/ci/__init__.py", "tools/ci/workspace_graph.py", "tools/ci/runner_labels.py",
            "tools/apps/assemble_app_collection.py",
            "apps/calculator/Cargo.toml", "apps/calculator/tilo-app.toml",
            "apps/calculator/surface.toml",
            "crates/packaging/tilo-app-package/max-app-bytes",
            "tools/apps/dev_app_versions.py", "dist/apps/app-collections.toml", ".cargo/config.toml", "tools/apps/app_package_isa.py",
        ):
            self.write(relative, (ROOT / relative).read_text())
        self.write("bin/cargo", """#!/usr/bin/env python3
import json, os, pathlib, sys, tomllib
arch = os.environ['FIXTURE_ARCH']
config = tomllib.loads(pathlib.Path('.cargo/config.toml').read_text())
key = f'cfg(all(target_arch = "{arch}", target_os = "linux"))'
expected = config['target'][key]['rustflags']
actual = os.environ['CARGO_ENCODED_RUSTFLAGS'].split(chr(31))
assert actual == expected, (actual, expected)
with open(os.environ['FIXTURE_CARGO_LOG'], 'a') as log:
    log.write(json.dumps({'args': sys.argv[1:], 'flags': actual}) + '\\n')
""")
        self.write("bin/rustc", "#!/bin/sh\nprintf '%s\\n' \"$FIXTURE_STDLIB\"\n")
        self.write("tools/font-catalog.py", """#!/usr/bin/env python3
import os, pathlib, sys
a = sys.argv[1:]
assert a[-2:] == ['--scope', 'product']
if a[0] == 'prepare':
    if os.environ.get('FIXTURE_INVALID_FONTS'):
        raise SystemExit('invalid product font catalog')
    out = pathlib.Path(a[2]); out.mkdir(parents=True)
    (out/'manifest.toml').write_text('fixture product manifest')
    (out/'fixture.ttf').write_bytes(b'product font payload')
elif a[0] == 'verify':
    root = pathlib.Path(a[1])
    assert (root/'manifest.toml').read_text() == 'fixture product manifest'
    assert (root/'fixture.ttf').read_bytes() == b'product font payload'
else:
    raise SystemExit('unexpected font fixture invocation')
""")
        self.write("bin/uname", "#!/bin/sh\nprintf '%s\\n' \"$FIXTURE_ARCH\"\n")
        self.write("stdlib/libstd-fixture.so", "\x7fELF fixture standard library")
        self.write("native/libc.so.6", "\x7fELF fixture native dependency")
        self.write("keyboard/libxkbcommon.so.0", "\x7fELF fixture default keyboard library")
        self.write("bin/pkg-config", """#!/usr/bin/env python3
import pathlib, sys
assert sys.argv[1:] == ['--variable=libdir', 'xkbcommon']
print(pathlib.Path('keyboard').resolve())
""")
        self.write("bin/ldd", """#!/usr/bin/env python3
import pathlib, sys
dependency = pathlib.Path('native/libc.so.6').resolve()
for object in sys.argv[1:]:
    assert pathlib.Path(object).is_file()
    print(f'{object}:')
    print(f'libc.so.6 => {dependency} (0x1000)')
""")
        self.write("bin/readelf", """#!/usr/bin/env python3
import pathlib, sys
object = pathlib.Path(sys.argv[-1])
assert object.read_bytes().startswith(b'\\x7fELF')
if object.name != 'libc.so.6':
    print(' 0x0000000000000001 (NEEDED) Shared library: [libc.so.6]')
""")
        self.write("os/scripts/build-thin-apps.sh", """#!/bin/bash
set -euo pipefail
[[ "$BUILD_PACKAGES" == calculator ]]
[[ " $INCLUDE_PACKAGES " == *" calculator "* ]]
cargo build --locked --profile thin-app -p runtime-dylib -p calculator
mkdir -p target/thin-app
python3 - <<'ELF'
import os, pathlib
header = bytearray(64)
header[:6] = bytes([127, 69, 76, 70, 2, 1])
header[18:20] = (62 if os.environ['FIXTURE_ARCH'] == 'x86_64' else 183).to_bytes(2, 'little')
pathlib.Path('target/thin-app/calculator').write_bytes(header)
ELF
chmod +x target/thin-app/calculator
printf '\\177ELF fixture runtime dylib' > target/thin-app/libruntime_dylib.so
printf '\\177ELF fixture typst engine' > target/thin-app/libtilo_typst_engine.so
printf 'libtilo_typst_engine.so\\n' > target/thin-app/runtime-engines
""")
        self.write("os/scripts/verify-elf-hardening.sh", "#!/bin/bash\nset -euo pipefail\nfor f; do test -x \"$f\"; done\n")
        self.write("target/release/tilo-runtime-bundle", """#!/usr/bin/env python3
import pathlib, shutil, sys
a = sys.argv[1:]
if a[:2] == ['key', 'generate']:
    pathlib.Path(a[a.index('--out')+1]).write_text('PRIVATE_TEST_IDENTITY')
elif a[:2] == ['key', 'pubkey']:
    print('ab'*32)
elif a[0] == 'build':
    version = a[a.index('--release')+1] + '+native.' + 'ab'*16
    out = pathlib.Path(a[a.index('--out-root')+1]) / version; out.mkdir(parents=True)
    arch = a[a.index('--arch')+1]
    (out/'bundle.toml').write_text(f'version = "{version}"\\narch = "{arch}"\\nroot_hash = "'+'cd'*32+'"\\n')
    (out/'lib').mkdir(); (out/'lib/runtime.so').write_bytes(b'runtime payload')
    for index, argument in enumerate(a):
        if argument == '--include':
            source = pathlib.Path(a[index+1])
            if source.is_dir():
                shutil.copytree(source, out/'lib'/source.name)
            else:
                shutil.copyfile(source, out/'lib'/source.name)
    pathlib.Path(a[a.index('--version-out')+1]).write_text(version + '\\n')
elif a[0] == 'verify':
    assert (pathlib.Path(a[1])/'bundle.toml').is_file()
else:
    raise SystemExit('unexpected runtime fixture invocation')
""")
        self.write("target/release/tilo-app-package", """#!/usr/bin/env python3
import pathlib, sys, os, tomllib
a = sys.argv[1:]
runtime = 'tilo-runtime/2026.07+native.' + 'ab'*16
if a[0] == 'pack':
    manifest = tomllib.loads(pathlib.Path(a[a.index('--manifest')+1]).read_text())
    arch = os.environ['FIXTURE_ARCH']
    if manifest['package']['kind'] == 'runtime':
        assert manifest['package']['provides'] == runtime, manifest
        assert (pathlib.Path(a[a.index('--payload')+1])/'bundle.toml').is_file()
        pathlib.Path(a[a.index('--output')+1]).write_bytes(b'r' * 64)
    else:
        assert manifest['package']['isa'] == {arch: 'v3' if arch == 'x86_64' else 'armv8-a'}
        assert manifest['package']['runtime'] == runtime, manifest
        payload = pathlib.Path(a[a.index('--payload')+1])
        output = pathlib.Path(a[a.index('--output')+1])
        output.write_bytes((payload/'bin/calculator').read_bytes())
        # The surface the payload root carries, kept for the test to read.
        surface = payload/'surface.toml'
        if surface.is_file():
            kept = pathlib.Path(os.environ['FIXTURE_SURFACES'])
            kept.mkdir(exist_ok=True)
            (kept/(output.name + '.surface')).write_bytes(surface.read_bytes())
elif a[0] == 'split':
    tapp = pathlib.Path(a[1]); out = pathlib.Path(a[a.index('--out')+1])
    out.mkdir(parents=True, exist_ok=True)
    (out/tapp.name).write_bytes(b'thin')
    (out/('ef'*32 + '.blob')).write_bytes(b'runtime payload')
elif a[0] == 'verify':
    assert len(pathlib.Path(a[-1]).read_bytes()) == 64
elif a[0] == 'compress':
    pathlib.Path(a[a.index('--output')+1]).write_bytes(b'z' * 24)
else:
    raise SystemExit('unexpected tapp fixture invocation')
""")
        self.env = dict(os.environ, PATH=str(self.root / "bin") + os.pathsep + os.environ["PATH"],
                        FIXTURE_CARGO_LOG=str(self.root / "cargo.log"),
                        FIXTURE_STDLIB=str(self.root / "stdlib"), TILO_RELEASE_APPS="calculator",
                        FIXTURE_SURFACES=str(self.root / "packed-surfaces"),
                        SOURCE_DATE_EPOCH="123", GITHUB_SHA="1" * 40)
        for name in ("CARGO_TARGET_DIR", "TILO_APP_RELEASE_OUT", "TAPP_PUBLISHER_IDENTITY_FILE",
                     "RUNTIME_BUNDLE_IDENTITY_FILE", "TRUST_LIST", "MAX_TAPP_BYTES",
                     "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_TARGET"):
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

    def test_build_inputs_are_checked_before_pack_and_failures_name_the_app(self):
        with self.subTest(phase='pack failure preserves status and names the app'):
            self.doCleanups()
            self.setUp()
            self.env["FIXTURE_ARCH"] = "x86_64"
            self.write("target/release/tilo-app-package", """#!/bin/sh
if [ "$1" = pack ]; then
    echo 'application tapp is too large' >&2
    exit 37
fi
echo 'verify must not run' >&2
exit 99
""")
            result = subprocess.run(["bash", "dist/apps/package-apps.sh"],
                                    cwd=self.root, env=self.env, text=True,
                                    capture_output=True, timeout=60)
            self.assertEqual(result.returncode, 37, result.stdout + result.stderr)
            self.assertIn("packing app=calculator binary=", result.stdout)
            self.assertIn("application tapp is too large", result.stderr)
            self.assertIn("pack failed: app=calculator output=", result.stderr)
            self.assertIn("calculator-0.1.0-x86_64.tapp", result.stderr)
            self.assertIn("exit_status=37 binary_bytes=64 limit=8388608", result.stderr)
            self.assertNotIn("verify must not run", result.stderr)
            self.assertNotIn("packed app=", result.stdout)
        with self.subTest(phase='missing source isa is staged before pack on both architectures'):
            self.doCleanups()
            self.setUp()
            manifest = self.root / "apps/calculator/tilo-app.toml"
            manifest.write_text(re.sub(r'\[package\.isa\]\s*x86_64 = "v3"\s*',
                                       '', manifest.read_text()))
            for architecture in ("x86_64", "aarch64"):
                self.env["FIXTURE_ARCH"] = architecture
                self.command("bash", "dist/apps/package-apps.sh")
        with self.subTest(phase='wrong arch payload is rejected before pack'):
            self.doCleanups()
            self.setUp()
            self.env["FIXTURE_ARCH"] = "aarch64"
            self.write("os/scripts/build-thin-apps.sh",
                       (self.root / "os/scripts/build-thin-apps.sh").read_text().replace(
                           "62 if os.environ['FIXTURE_ARCH'] == 'x86_64' else 183", "62"))
            result = subprocess.run(["bash", "dist/apps/package-apps.sh"], cwd=self.root,
                                    env=self.env, text=True, capture_output=True, timeout=60)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("expected a 64-bit little-endian aarch64 ELF", result.stderr)
            self.assertFalse(list((self.root / "dist/app-release/tapp").glob("*.tapp")))
        with self.subTest(phase='unrecorded compiler override fails before build'):
            self.doCleanups()
            self.setUp()
            for key, value in (("RUSTFLAGS", "-C target-cpu=native"),
                               ("CARGO_ENCODED_RUSTFLAGS", "-C\x1ftarget-cpu=native"),
                               ("CARGO_BUILD_TARGET", "aarch64-unknown-linux-gnu")):
                with self.subTest(key=key):
                    env = dict(self.env, FIXTURE_ARCH="x86_64", **{key: value})
                    result = subprocess.run(["bash", "dist/apps/package-apps.sh"], cwd=self.root,
                                            env=env, text=True, capture_output=True, timeout=60)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(f"{key} overrides", result.stderr)
                    self.assertFalse((self.root / "cargo.log").exists())
        with self.subTest(phase='edited cpu and feature policy is rejected by actual helper'):
            self.doCleanups()
            self.setUp()
            config = self.root / ".cargo/config.toml"
            original = config.read_text()
            for arch, cpu in (("x86_64", "x86-64-v3"), ("aarch64", "generic")):
                for replacement in ('"-C", "target-cpu=native",',
                                    f'"-C", "target-cpu={cpu}", "-C", "target-feature=+aes",'):
                    with self.subTest(arch=arch, replacement=replacement):
                        config.write_text(original.replace(f'"-C", "target-cpu={cpu}",', replacement))
                        result = subprocess.run([
                            "python3", "tools/apps/app_package_isa.py", "flags", "--root", str(self.root),
                            "--arch", arch], cwd=self.root, env=self.env,
                            text=True, capture_output=True, timeout=10)
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn("unaudited", result.stderr)
                        self.assertEqual(result.stdout, "")

    def test_a_release_tapp_carries_the_apps_committed_surface(self):
        self.env["FIXTURE_ARCH"] = "x86_64"
        self.command("bash", "dist/apps/package-apps.sh")
        packed = list((self.root / "packed-surfaces").glob("calculator-*-x86_64.tapp.surface"))
        self.assertEqual(len(packed), 1, "the calculator tapp's payload has a surface.toml")
        self.assertEqual(packed[0].read_bytes(),
                         (ROOT / "apps/calculator/surface.toml").read_bytes())

    def test_native_outputs_keep_audited_flags_through_upload_and_assembly(self):
        with self.subTest(phase='encoded output retains every audited target flag'):
            self.doCleanups()
            self.setUp()
            config = tomllib.loads((self.root / ".cargo/config.toml").read_text())
            for arch in ("x86_64", "aarch64"):
                with self.subTest(arch=arch):
                    result = self.command("python3", "tools/apps/app_package_isa.py", "flags",
                                          "--root", str(self.root), "--arch", arch)
                    flags = result.stdout.removesuffix("\n").split("\x1f")
                    key = f'cfg(all(target_arch = "{arch}", target_os = "linux"))'
                    self.assertEqual(flags, config["target"][key]["rustflags"])
                    for required in ("force-frame-pointers=yes", "relro-level=full",
                                     "link-arg=-Wl,-z,noexecstack", "--remap-path-prefix=/build=."):
                        self.assertIn(required, flags)
        with self.subTest(phase='native outputs survive actual upload selection and collection assembly'):
            self.doCleanups()
            self.setUp()
            for architecture in ("x86_64", "aarch64"):
                with self.subTest(architecture=architecture):
                    self.env["FIXTURE_ARCH"] = architecture
                    result = self.command("bash", "dist/apps/package-apps.sh")
                    self.assertIn("packing app=calculator binary=", result.stdout)
                    self.assertIn("binary_bytes=64", result.stdout)
                    self.assertIn("packed app=calculator output=", result.stdout)
                    self.assertIn("tapp_bytes=64 limit=8388608", result.stdout)
                    calls = [json.loads(line) for line in (self.root / "cargo.log").read_text().splitlines()]
                    self.assertTrue(any("--release" in call["args"] for call in calls))
                    self.assertTrue(any("thin-app" in call["args"] for call in calls))
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
                    abi = "2026.07+native." + "ab" * 16
                    self.assertTrue((artifact / "runtime/tilo-runtime" / abi / "bundle.toml").is_file(),
                                    "workflow did not transport the produced runtime")
                    self.assertTrue((artifact / "runtime/tilo-runtime" / abi / "lib/libxkbcommon.so.0").is_file(),
                                    "a partial calculator release omitted the default SDK keyboard dependency")
                    self.assertTrue((artifact / "runtime/tilo-runtime" / abi / "lib/libtilo_typst_engine.so").is_file(),
                                    "a partial calculator release omitted a runtime engine")
                    self.assertTrue((artifact / f"runtime-package/runtime-0.1.0-{architecture}.tapp").is_file(),
                                    "workflow did not transport the runtime package")
                    self.assertTrue(list((artifact / "runtime-package").glob("*.blob")),
                                    "workflow did not transport the runtime's payload objects")
                    index = json.loads((out / "index.json").read_text())
                    self.assertEqual({app["runtime"] for app in index["apps"]}, {"tilo-runtime/" + abi})
                    self.assertEqual((artifact / "runtime/runtime-bundle-publisher.hex").read_text().strip(), "ab" * 32)
                    sums = self.command("bash", "-c", 'cd "$1" && sha256sum --check --strict SHA256SUMS',
                                        "verify-download", str(artifact))
                    self.assertIn("runtime-bundle-publisher.hex: OK", sums.stdout)
                    for file in artifact.rglob("*"):
                        if file.is_file():
                            self.assertNotIn(b"PRIVATE_TEST_IDENTITY", file.read_bytes())
                    collection = self.root / "dist" / f"collection-{architecture}"
                    env = dict(self.env, TILO_APP_COLLECTION_PLAN=str(out / "plan.json"),
                               TILO_APP_ARCH=architecture, TILO_APP_TAPP_DIR=str(artifact / "tapp"),
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

    def test_missing_signed_native_dependency_fails_before_app_pack(self):
        self.env["FIXTURE_ARCH"] = "x86_64"
        tool = self.root / "target/release/tilo-runtime-bundle"
        tool.write_text(tool.read_text().replace(
            "shutil.copyfile(source, out/'lib'/source.name)",
            "if source.name != 'libc.so.6': shutil.copyfile(source, out/'lib'/source.name)"))
        result = subprocess.run(["bash", "dist/apps/package-apps.sh"], cwd=self.root,
                                env=self.env, text=True, capture_output=True, timeout=60)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("signed runtime is missing libc.so.6", result.stderr)
        self.assertFalse(list((self.root / "dist/app-release/tapp").glob("*.tapp")))

    def test_missing_default_keyboard_library_fails_even_for_partial_selection(self):
        self.env["FIXTURE_ARCH"] = "x86_64"
        (self.root / "keyboard/libxkbcommon.so.0").unlink()
        result = subprocess.run(["bash", "dist/apps/package-apps.sh"], cwd=self.root,
                                env=self.env, text=True, capture_output=True, timeout=60)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("default SDK keyboard library is missing", result.stderr)
        self.assertFalse(list((self.root / "dist/app-release/tapp").glob("*.tapp")))

    def test_partial_release_carries_its_verified_sdk_fonts(self):
        for arch in ('x86_64', 'aarch64'):
            with self.subTest(architecture=arch):
                self.env['FIXTURE_ARCH'] = arch
                self.command('bash', 'dist/apps/package-apps.sh')
                fonts = next((self.root / 'dist/app-release/runtime/tilo-runtime').glob('*/lib/fonts'))
                self.assertEqual((fonts / 'fixture.ttf').read_bytes(), b'product font payload')
                with tarfile.open(self.root / f'dist/app-release/tilo-apps-{arch}.tar.gz') as archive:
                    self.assertTrue(any(name.endswith('/lib/fonts/fixture.ttf') for name in archive.getnames()))

    def test_invalid_product_fonts_stop_before_signing_app_packages(self):
        self.env.update(FIXTURE_ARCH='aarch64', FIXTURE_INVALID_FONTS='1')
        result = subprocess.run(['bash', 'dist/apps/package-apps.sh'], cwd=self.root,
                                env=self.env, text=True, capture_output=True, timeout=60)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('invalid product font catalog', result.stderr)
        self.assertFalse(list((self.root / 'dist/app-release/tapp').glob('*.tapp')))

    def test_dropped_product_fonts_stop_before_signing_app_packages(self):
        self.env['FIXTURE_ARCH'] = 'aarch64'
        tool = self.root / 'target/release/tilo-runtime-bundle'
        tool.write_text(tool.read_text().replace(
            "shutil.copytree(source, out/'lib'/source.name)", 'pass'))
        result = subprocess.run(['bash', 'dist/apps/package-apps.sh'], cwd=self.root,
                                env=self.env, text=True, capture_output=True, timeout=60)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('signed SDK is missing its verified product font catalog', result.stderr)
        self.assertFalse(list((self.root / 'dist/app-release/tapp').glob('*.tapp')))


if __name__ == "__main__":
    unittest.main()

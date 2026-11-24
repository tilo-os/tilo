"""An image installs the tilolang packages its app set names, and their runtime.

A tilolang app is a package under apps/tl that `tilolang build` compiles and
signs into a `.tapp`; it executes the tilolang runtime bundle
(crates/lang/tilolang-process/tilo-runtime.toml), baked beside the Tilo runtime by
tools/apps/tilolang_packages.sh and required by `tilo-install` before it installs
a package naming it. The image's packing must install the packages its app set
planned (`tools/apps/app_release.py`), through the same `tilo-install` a Rust
tapp goes through, and only those. The payload stage packs from the tree
`compile.sh --apps` hands it, so that tree carries the binaries both steps run.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / 'os/scripts/pack-apps.sh'
HELPER = REPO / 'tools/apps/tilolang_packages.sh'
RUNTIME_ID = 'tilo-runtime/2026.07+native.' + '0' * 32
TILOLANG_RUNTIME = 'tilolang/0.1.0+tilolang'
NOTES = 'so.tilo.notes'
DISCUSSION = 'so.tilo.lang.discussion'


class Packer:
    """A pack-apps.sh packing over a fixture tree, with the tools it calls."""

    def __init__(self, root, packages):
        self.root = root
        self.repo = root / 'repo'
        self.bins = root / 'bin'
        self.store = root / 'store'
        self.scratch = root / 'scratch'
        self.runtimes = root / 'runtimes'
        self.log = root / 'tilolang.log'
        self.repo.mkdir()
        self.bins.mkdir()
        self.store.mkdir()
        self.scratch.mkdir()
        self.runtimes.mkdir()
        (self.store / 'registry.toml').write_text('[[entry]]\nid="existing"\n')
        for name, app_id in packages.items():
            package = self.repo / 'apps/tl' / name
            package.mkdir(parents=True)
            (package / 'app.tl').write_text(f'app {app_id} "{name.title()}"\n')
            (package / 'app.ids').write_text('')
        self.plan = root / 'app-plan.json'
        self.plan.write_text(json.dumps({
            'tilolang': [
                {'app': f'tl-{name}', 'dir': f'apps/tl/{name}', 'id': app_id,
                 'version': '0.1.0', 'runtime': TILOLANG_RUNTIME}
                for name, app_id in packages.items()
            ],
        }))
        bundle = root / 'bundle'
        bundle.write_text('''#!/bin/bash
set -eu
case "$1" in
 key) case "$2" in
       generate) printf synthetic-private-key > "$4" ;;
       pubkey) printf '%064d\\n' 1 ;;
       *) exit 18 ;;
      esac ;;
 *) exit 18 ;;
esac
''')
        tilolang = root / 'tilolang'
        tilolang.write_text(f'''#!/bin/bash
set -eu
[ "$1" = build ] || exit 0
dir=$2
id=$(sed -n 's/^app \\([^ ]*\\).*/\\1/p' "$dir/app.tl" | head -1)
printf '%s\\n' "$*" >> '{self.log}'
while [ $# -gt 0 ]; do
  case "$1" in --out) out=$2; shift ;; esac
  shift
done
printf '%s\\n' "$id" > "$out/${{id##*.}}.tapp"
''')
        verify = root / 'verify'
        verify.write_text('#!/bin/sh\nexit 0\n')
        install = root / 'install'
        install.write_text(f'''#!/bin/sh
set -eu
# `tilo-install` resolves the package's tilolang runtime before it installs, so a
# package installed before its bundle exists is refused there.
test -f '{self.runtimes}/0.1.0+tilolang/bundle.toml' || exit 27
# An image build names the store it writes: `--offline-store DIR install TAPP`.
test "$1" = --offline-store && test "$3" = install || exit 28
store=$2
tapp=$4
id=$(cat "$tapp")
printf '[[entry]]\\nid="%s"\\n' "$id" >> "$store/registry.toml"
''')
        for tool in (bundle, tilolang, verify, install):
            tool.chmod(0o755)
        self.bundle, self.tilolang, self.verify, self.install = (
            bundle, tilolang, verify, install)

    def with_runtime(self):
        """The tilolang runtime bundle the runtime bake left beside the Tilo one."""
        bundle = self.runtimes / '0.1.0+tilolang'
        bundle.mkdir()
        (bundle / 'bundle.toml').write_text('signed\n')
        return self

    def environment(self, **overrides):
        environment = {
            'PATH': os.defpath, 'HOME': str(self.root), 'TMPDIR': str(self.scratch),
            'REPO': str(self.repo), 'STORE': str(self.store), 'BIN_DIR': str(self.bins),
            'APP_MODELS': str(self.root / 'models'), 'APP_PLAN': str(self.plan),
            'BUNDLE_BIN': str(self.bundle), 'PKG_BIN': str(self.verify),
            'INSTALL_BIN': str(self.install), 'SHIM_BIN': '/missing',
            'TILOLANG_BIN': str(self.tilolang),
            'RUNTIME_ROOT': str(self.runtimes), 'RUNTIME_ID': RUNTIME_ID,
        }
        environment.update(overrides)
        return environment

    def pack(self, **overrides):
        environment = self.environment(**overrides)
        child = subprocess.Popen(['bash', str(SCRIPT)], env=environment,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        stdout, stderr = child.communicate()
        return subprocess.CompletedProcess(child.args, child.returncode, stdout, stderr)


class TilolangPackageTests(unittest.TestCase):
    def test_the_named_packages_install_from_the_tilolang_runtime_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            packer = Packer(Path(directory),
                            {'notes': NOTES, 'discussion': DISCUSSION}).with_runtime()
            result = packer.pack()
            self.assertEqual(result.returncode, 0, result.stderr)
            registry = (packer.store / 'registry.toml').read_text()
            self.assertIn(NOTES, registry)
            self.assertIn(DISCUSSION, registry)
            # Each package is compiled from its sources with the packages beside
            # it, against the exact Tilo runtime build the rest of the image runs.
            build = packer.log.read_text()
            self.assertIn(f'--search {packer.repo}/apps/tl', build)
            self.assertIn(f'--tilo-runtime {RUNTIME_ID}', build)
            self.assertIn('--version 0.1.0', build)

    def test_a_package_whose_runtime_is_absent_does_not_install(self):
        # The runtime bake runs before the packing; without its bundle the
        # installer refuses the package, and the build fails rather than ship an
        # app that boots into "not installed".
        with tempfile.TemporaryDirectory() as directory:
            result = Packer(Path(directory), {'notes': NOTES}).pack()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('install failed', result.stderr)

    def test_a_package_outside_the_app_set_is_not_installed(self):
        with tempfile.TemporaryDirectory() as directory:
            packer = Packer(Path(directory), {'notes': NOTES}).with_runtime()
            # A package in the tree that the image's collection does not name.
            extra = packer.repo / 'apps/tl/mail'
            extra.mkdir()
            (extra / 'app.tl').write_text('app so.tilo.mail "Mail"\n')
            (extra / 'app.ids').write_text('')
            result = packer.pack()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn('mail', packer.log.read_text())
            self.assertNotIn('so.tilo.mail', (packer.store / 'registry.toml').read_text())

    def test_a_cohort_that_names_one_tilolang_package_installs_only_it(self):
        with tempfile.TemporaryDirectory() as directory:
            packer = Packer(Path(directory),
                            {'notes': NOTES, 'discussion': DISCUSSION}).with_runtime()
            result = packer.pack(COHORT_APPS='tl-notes')
            self.assertEqual(result.returncode, 0, result.stderr)
            registry = (packer.store / 'registry.toml').read_text()
            self.assertIn(NOTES, registry)
            self.assertNotIn(DISCUSSION, registry)


class RuntimeBundleTests(unittest.TestCase):
    def test_the_bundle_holds_the_runtime_and_its_host_and_nothing_else(self):
        # The release, the image and the developer scripts bake the same bundle
        # through this function (tools/apps/tilolang_packages.sh).
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bundle = root / 'bundle'
            log = root / 'bundle.log'
            bundle.write_text(f'''#!/bin/bash
set -eu
printf '%s\\n' "$*" > '{log}'
mkdir -p "$(python3 -c 'import sys; a=sys.argv[1:]; print(a[a.index("--out") + 1])' "$@")"
''')
            bundle.chmod(0o755)
            (root / 'tilolang-process').write_text('')
            out = root / 'runtimes/0.1.0+tilolang'
            child = subprocess.run(
                ['bash', '-c', 'set -euo pipefail; . "$1"; shift; tilolang_bake_runtime "$@"',
                 'bash', str(HELPER), str(bundle), str(root / 'tilolang-process'),
                 str(root / 'identity.json'), str(out), '0.1.0+tilolang'],
                capture_output=True, text=True)
            self.assertEqual(child.returncode, 0, child.stderr)
            invocation = log.read_text()
            self.assertIn('build --binaries-only', invocation)
            self.assertIn('--version 0.1.0+tilolang', invocation)
            self.assertIn(f'--include-bin {root}/tilolang-process', invocation)
            self.assertEqual(invocation.count('--include-bin'), 1)
            self.assertIn(f'--identity {root}/identity.json', invocation)
            self.assertIn(f'--out {out}', invocation)
            self.assertTrue(out.is_dir(), 'the bundle is written where the launcher looks')
            # The binary the bundle executes is executable, as the launcher runs it.
            self.assertTrue(os.access(root / 'tilolang-process', os.X_OK))
            self.assertNotIn('--arch', invocation)
            self.assertNotIn('--include ', invocation)

            # A Mac's runtime: a universal binary, and the libraries it loads beside it.
            libraries = root / 'gstreamer'
            libraries.mkdir()
            child = subprocess.run(
                ['bash', '-c', 'set -euo pipefail; . "$1"; shift; tilolang_bake_runtime "$@"',
                 'bash', str(HELPER), str(bundle), str(root / 'tilolang-process'),
                 str(root / 'identity.json'), str(out), '0.1.0+tilolang', 'aarch64+x86_64',
                 str(libraries)],
                capture_output=True, text=True)
            self.assertEqual(child.returncode, 0, child.stderr)
            invocation = log.read_text()
            self.assertIn('--arch aarch64+x86_64', invocation)
            self.assertIn(f'--include {libraries}', invocation)

    def test_the_runtime_version_is_the_one_the_manifest_provides(self):
        # The version every package names in `optional-runtimes` is the one the
        # manifest declares, so the bundle and the packages cannot disagree.
        child = subprocess.run(
            ['bash', '-c', 'set -euo pipefail; . "$1"; shift; tilolang_runtime_version "$1"',
             'bash', str(HELPER), str(REPO)],
            capture_output=True, text=True)
        self.assertEqual(child.returncode, 0, child.stderr)
        manifest = (REPO / 'crates/lang/tilolang-process/tilo-runtime.toml').read_text()
        self.assertIn(f'provides = "tilolang/{child.stdout.strip()}"', manifest)


class PayloadTreeTests(unittest.TestCase):
    """`compile.sh --apps`: the tree the payload stage bakes and packs from."""

    def compile_tree(self, root, tilolang_packages, with_tilolang=True):
        source = root / 'compiled'
        for name in ('libruntime_dylib.so', 'runtime-engines',
                     'tilo-runtime-bundle', 'tilo-app-package', 'tilo-install', 'tilo-cli-shim',
                     'thin-apps/tilolang-process'):
            (source / name).parent.mkdir(parents=True, exist_ok=True)
            (source / name).write_text('')
        for name in ('apps', 'language-packs'):
            (source / name).mkdir()
        runtime = source / 'crates/lang/tilolang-process/tilo-runtime.toml'
        runtime.parent.mkdir(parents=True)
        runtime.write_text((REPO / 'crates/lang/tilolang-process/tilo-runtime.toml').read_text())
        (source / 'app-plan.json').write_text(json.dumps({
            'tilolang': [{'app': 'tl-notes', 'dir': 'apps/tl/notes', 'id': NOTES,
                          'version': '0.1.0', 'runtime': TILOLANG_RUNTIME}]
        } if tilolang_packages else {'tilolang': []}))
        if with_tilolang:
            (source / 'tilolang').write_text('')
        return subprocess.run(
            ['bash', str(REPO / 'os/image/compile.sh'), '--apps', str(source),
             str(root / 'compiled-apps')],
            env={'PATH': os.defpath, 'HOME': str(root), 'TMPDIR': str(root)},
            capture_output=True, text=True)

    def test_the_tree_carries_the_binaries_the_bake_and_the_packing_run(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = self.compile_tree(root, tilolang_packages=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            for name in ('tilolang', 'thin-apps/tilolang-process', 'app-plan.json',
                         'crates/lang/tilolang-process/tilo-runtime.toml'):
                self.assertTrue((root / 'compiled-apps' / name).exists(), name)

    def test_a_plan_naming_tilolang_packages_without_the_compiler_is_refused(self):
        # The payload stage would bake the runtime and then fail to build a
        # package: the tree is checked where it is made.
        with tempfile.TemporaryDirectory() as directory:
            result = self.compile_tree(Path(directory), tilolang_packages=True,
                                       with_tilolang=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('has no tilolang', result.stderr)

    def test_a_tree_that_ships_no_tilolang_package_needs_no_compiler(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = self.compile_tree(root, tilolang_packages=False, with_tilolang=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse((root / 'compiled-apps' / 'tilolang').exists())


if __name__ == '__main__':
    unittest.main()

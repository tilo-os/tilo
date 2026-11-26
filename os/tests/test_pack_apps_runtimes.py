"""An image installs the language runtime packages its app set names.

An app may run a script on a runtime's interpreter (`[entrypoint] runtime =
"bun/1.4.5+bun"`, and Python the same way). The image bakes each named
runtime's package from its pinned upstream release
(tools/apps/runtime_packages.sh, os/Containerfile) and stages its full tapp;
os/scripts/pack-apps.sh installs the plan's runtimes into the image's store
through the same `tilo-install` an app is, with the bundle landing where the
launcher mounts it. Which runtimes are installed is read from the plan the
compile wrote, never a hand-kept list, so a plan and the image cannot disagree.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / 'os/scripts/pack-apps.sh'
RUNTIME_ID = 'tilo-runtime/2026.07+native.' + '0' * 32
BUN = 'bun/1.4.5+bun'
PYTHON = 'python/3.13.18+python'


class Packer:
    """A pack-apps.sh packing over a fixture tree with a plan of runtimes."""

    def __init__(self, root: Path, runtimes: list[str], staged: list[str]):
        self.root = root
        self.repo = root / 'repo'
        self.bins = root / 'bin'
        self.store = root / 'store'
        self.scratch = root / 'scratch'
        self.runtimes = root / 'runtimes'
        self.packages = root / 'language-runtime-packages'
        self.log = root / 'install.log'
        for directory in (self.repo / 'apps', self.bins, self.store, self.scratch,
                          self.runtimes, self.packages):
            directory.mkdir(parents=True, exist_ok=True)
        (self.store / 'registry.toml').write_text('[[entry]]\nid="existing"\n')
        for name in staged:
            staged_dir = self.packages / name
            staged_dir.mkdir(parents=True)
            (staged_dir / f'{name}.tapp').write_text('package\n')
        self.plan = root / 'app-plan.json'
        self.plan.write_text(json.dumps({
            'catalog': [], 'tilolang': [], 'runtimes': runtimes,
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
        verify = root / 'verify'
        verify.write_text('#!/bin/sh\nexit 0\n')
        # `tilo-install --offline-store STORE install TAPP --allow-self-signed --pinned`, with the
        # runtime's bundle placed where the image mounts it and the call recorded.
        install = root / 'install'
        install.write_text(f'''#!/bin/bash
set -eu
test "$1" = --offline-store && test "$3" = install || exit 28
store=$2
tapp=$4
printf '%s|%s|%s|%s\\n' "$tapp" "${{TILO_RUNTIME_BUNDLE_DIR:-}}" "$5" "$6" >> '{self.log}'
if [ -n "${{TILO_RUNTIME_BUNDLE_DIR:-}}" ]; then
  mkdir -p "$TILO_RUNTIME_BUNDLE_DIR/1.4.5+bun" "$TILO_RUNTIME_BUNDLE_DIR/3.13.18+python"
  printf 'signed\\n' > "$TILO_RUNTIME_BUNDLE_DIR/1.4.5+bun/bundle.toml"
  printf 'signed\\n' > "$TILO_RUNTIME_BUNDLE_DIR/3.13.18+python/bundle.toml"
fi
printf '[[entry]]\\nid="%s"\\n' "$(cat "$tapp")" >> "$store/registry.toml"
''')
        for tool in (bundle, verify, install):
            tool.chmod(0o755)
        self.bundle, self.verify, self.install = bundle, verify, install

    def pack(self, **overrides):
        environment = {
            'PATH': os.defpath, 'HOME': str(self.root), 'TMPDIR': str(self.scratch),
            'REPO': str(self.repo), 'STORE': str(self.store), 'BIN_DIR': str(self.bins),
            'APP_PLAN': str(self.plan),
            'LANGUAGE_RUNTIME_PACKAGES': str(self.packages),
            'BUNDLE_BIN': str(self.bundle), 'PKG_BIN': str(self.verify),
            'INSTALL_BIN': str(self.install), 'SHIM_BIN': '/missing',
            'RUNTIME_ROOT': str(self.runtimes), 'RUNTIME_ID': RUNTIME_ID,
            'REQUIRE_MIN': '0',
        }
        environment.update(overrides)
        child = subprocess.Popen(['bash', str(SCRIPT)], env=environment,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        stdout, stderr = child.communicate()
        return subprocess.CompletedProcess(child.args, child.returncode, stdout, stderr)

    def installed(self) -> list[tuple[str, str, str, str]]:
        """The install calls recorded: (tapp, runtime root, flag, flag)."""
        if not self.log.exists():
            return []
        return [tuple(line.split('|')) for line in self.log.read_text().splitlines()]


class RuntimePackageTests(unittest.TestCase):
    def test_the_runtimes_the_plan_names_install_with_their_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            packer = Packer(Path(directory), [BUN, PYTHON], ['bun', 'python'])
            result = packer.pack()
            self.assertEqual(result.returncode, 0, result.stderr)
            installed = {Path(tapp).name: (root, first, second)
                         for tapp, root, first, second in packer.installed()}
            self.assertEqual(set(installed), {'bun.tapp', 'python.tapp'})
            for root, first, second in installed.values():
                # The bundle goes where the launcher mounts it, and the package stays for good.
                self.assertEqual(root, str(packer.runtimes))
                self.assertEqual((first, second), ('--allow-self-signed', '--pinned'))
            self.assertTrue((packer.runtimes / '1.4.5+bun/bundle.toml').is_file())
            self.assertTrue((packer.runtimes / '3.13.18+python/bundle.toml').is_file())
            self.assertIn('installed runtime bun/1.4.5+bun', result.stderr)
            self.assertIn('installed runtime python/3.13.18+python', result.stderr)

    def test_a_plan_with_no_language_runtime_installs_none(self):
        with tempfile.TemporaryDirectory() as directory:
            packer = Packer(Path(directory), [], ['bun', 'python'])
            result = packer.pack()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(packer.installed(), [])
            self.assertNotIn('installed runtime', result.stderr)

    def test_a_runtime_the_image_did_not_stage_fails_the_packing(self):
        # The bundle bake and the packing read the same plan: a plan naming a
        # runtime whose package the image did not build is a failed image, not an
        # app that boots into "its runtime is missing".
        with tempfile.TemporaryDirectory() as directory:
            packer = Packer(Path(directory), [BUN], [])
            result = packer.pack()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('no bun runtime package staged', result.stderr)
            self.assertEqual(packer.installed(), [])


if __name__ == '__main__':
    unittest.main()

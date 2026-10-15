"""Exercise the production native-store helper without mounting host filesystems."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "os/dev/ci-native-podman-store.sh"


class NativePodmanStoreTests(unittest.TestCase):
    def setUp(self):
        scratch = Path(os.environ.get("CARGO_TARGET_DIR") or ROOT / "target") / "native-store-fixtures"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.work = Path(self.temporary.name)
        self.bin = self.work / "bin"
        self.bin.mkdir()
        self.environment = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                                RUNNER_TEMP=str(self.work), GITHUB_ENV=str(self.work / "env"),
                                GITHUB_RUN_ID="fixture", GITHUB_RUN_ATTEMPT="1",
                                TILO_PODMAN_STORE_SIZE="1M", TEST_UID="0")
        for variable in ("CONTAINERS_STORAGE_CONF", "TILO_ROOT_PODMAN_STORE", "TILO_ROOT_PODMAN_STORAGE_CONF"):
            self.environment.pop(variable, None)
        self.tool("id", 'echo "$TEST_UID"')
        self.tool("sudo", '[ "$1" != -n ] || shift; exec "$@"')
        self.tool("truncate", ':')
        self.tool("mkfs.ext4", 'echo format >> "$RUNNER_TEMP/operations"')
        self.tool("losetup", 'echo /dev/loop-fixture')
        self.tool("mount", 'touch "${@: -1}/.mounted"')
        self.tool("mountpoint", 'test -f "${@: -1}/.mounted"')
        self.tool("chown", ':')
        self.tool("podman", """python3 - <<'PY_STUB'
import json, os, pathlib, tomllib
with open(os.environ['CONTAINERS_STORAGE_CONF'], 'rb') as source:
    storage = tomllib.load(source)['storage']
graph = pathlib.Path(storage['graphroot'])
(graph / 'overlay').mkdir(parents=True, exist_ok=True)
(graph / 'overlay/.has-mount-program').write_text(os.environ.get('TEST_MOUNT_PROGRAM', 'false'))
print(json.dumps({'store': {'graphDriverName': 'overlay', 'graphRoot': str(graph),
                          'graphStatus': {'Backing Filesystem': os.environ.get('TEST_BACKING', 'extfs')}}}))
PY_STUB""")

    def tool(self, name, body):
        path = self.bin / name
        path.write_text("#!/usr/bin/env bash\nset -euo pipefail\n" + body + "\n")
        path.chmod(0o755)

    def run_helper(self):
        return subprocess.run(["bash", str(SCRIPT)], env=self.environment,
                              capture_output=True, text=True, timeout=10)

    def exported(self):
        return dict(line.split("=", 1) for line in (self.work / "env").read_text().splitlines())

    def test_root_shares_one_native_store_with_privileged_disk_helper(self):
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        root = self.work / "tilo-podman-fixture-1"
        exported = self.exported()
        self.assertEqual(exported['CONTAINERS_STORAGE_CONF'], str(root / 'storage.conf'))
        self.assertEqual(exported['TILO_ROOT_PODMAN_STORAGE_CONF'], exported['CONTAINERS_STORAGE_CONF'])
        self.assertEqual(exported['TILO_ROOT_PODMAN_STORE'], str(root))
        self.assertTrue((root / 'containers/storage/overlay/.has-mount-program').is_file())
        self.assertFalse((root / 'graph').exists())

    def test_rootless_store_does_not_cross_the_privileged_boundary(self):
        self.environment['TEST_UID'] = '1000'
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(set(self.exported()), {'CONTAINERS_STORAGE_CONF'})
        self.assertTrue((self.work / 'tilo-podman-fixture-1/graph/overlay').is_dir())

    def test_reused_store_is_verified_and_exports_handoff_without_reformatting(self):
        first = self.run_helper()
        second = self.run_helper()
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual((self.work / 'operations').read_text(), 'format\n')
        self.assertIn('TILO_ROOT_PODMAN_STORE', self.exported())
        self.environment['TEST_MOUNT_PROGRAM'] = 'true'
        third = self.run_helper()
        self.assertNotEqual(third.returncode, 0)
        self.assertIn('did not select native overlay', third.stderr)

    def test_explicit_privileged_store_choice_is_preserved(self):
        self.environment['TILO_ROOT_PODMAN_STORE'] = '/fixture/explicit-root-store'
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(set(self.exported()), {'CONTAINERS_STORAGE_CONF'})

    def test_non_native_backing_fails_before_publishing_environment(self):
        self.environment['TEST_BACKING'] = 'overlayfs'
        result = self.run_helper()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('did not adopt the ext4 overlay store', result.stderr)
        self.assertFalse((self.work / 'env').exists())


if __name__ == "__main__":
    unittest.main()

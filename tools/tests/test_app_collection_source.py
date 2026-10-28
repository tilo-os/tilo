"""Exercise the release preflight against real Git history and app input policy."""
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class CollectionSourceTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        for name in ["tools/ci/check_app_collection_source.sh", ".forgejo/workflows/release-apps.yml"]:
            dest = self.repo / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, dest)
        self.git("init", "-q")
        self.write("crates/sdk/ui-core/src/application.rs", "old save status")
        self.base = self.commit()

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.repo), *args], text=True).strip()

    def write(self, name, text):
        path = self.repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def commit(self):
        self.git("add", ".")
        # Identity is scoped to this fixture command, never configured on a worktree.
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "commit", "-qm", "fixture")
        return self.git("rev-parse", "HEAD")

    def check(self, source, target):
        return subprocess.run(["bash", str(self.repo / "tools/ci/check_app_collection_source.sh"),
                               source, target], capture_output=True, text=True)

    def test_shared_ui_fix_rejects_old_apps(self):
        self.write("crates/sdk/ui-core/src/application.rs", "saved after writing")
        target = self.commit()
        result = self.check(self.base, target)
        self.assertEqual(result.returncode, 1)
        self.assertIn("crates/sdk/ui-core/src/application.rs", result.stderr)
        self.assertIn("component swaps", result.stderr)
        self.assertEqual(self.check(target, target).returncode, 0)

    def test_os_only_change_reuses_compatible_apps(self):
        self.write("os/usr/libexec/tilo-session", "session fix")
        result = self.check(self.base, self.commit())
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_source_fails(self):
        self.assertEqual(self.check("", self.base).returncode, 1)
        self.assertEqual(self.check("a" * 40, self.base).returncode, 1)

    def test_unreadable_input_policy_fails(self):
        self.write(".forgejo/workflows/release-apps.yml", "no input policy")
        self.assertEqual(self.check(self.base, self.base).returncode, 1)


if __name__ == "__main__":
    unittest.main()

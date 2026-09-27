import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location(
    "slot_cache", Path(__file__).parents[1] / "ci/slot_cache.py"
)
slot_cache = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(slot_cache)

DAY = 86400


def git(workspace: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(workspace), *args], check=True, capture_output=True)


def checkout(workspace: Path, files: dict[str, str], mtime_ns: int) -> None:
    """Stand in for actions/checkout: fresh content, every file one new mtime."""
    for name, text in files.items():
        path = workspace / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        os.utime(path, ns=(mtime_ns, mtime_ns))
    git(workspace, "add", "-A")


def mtime(path: Path) -> int:
    return os.lstat(path).st_mtime_ns


class SlotCacheTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.workspace = self.tmp / "workspace"
        self.workspace.mkdir()
        git(self.workspace, "init", "-q")
        (self.workspace / ".git/info/exclude").write_text("/target/\n", encoding="utf-8")
        self.target = self.workspace / "target"
        self.target.mkdir()

    def test_mtimes_are_restored_only_for_sealed_unchanged_blobs(self) -> None:
        first, second, third = 10**18, 2 * 10**18, 3 * 10**18
        with self.subTest(phase="a first job has nothing to restore"):
            checkout(self.workspace, {"src/a.rs": "a", "src/b.rs": "b"}, first)
            restored, paths = slot_cache.restore_mtimes(self.workspace, self.target)
            self.assertEqual((restored, paths), (0, 3))  # two files and src/
            self.assertFalse((self.target / slot_cache.SEALED).exists())
            slot_cache.seal(mock.Mock(workspace=str(self.workspace)))
            self.assertTrue((self.target / slot_cache.SEALED).exists())
            self.assertFalse((self.target / slot_cache.PENDING).exists())

        with self.subTest(phase="unchanged blobs get the mtime artifacts were built after"):
            checkout(self.workspace, {"src/a.rs": "a", "src/b.rs": "b changed"}, second)
            restored, _ = slot_cache.restore_mtimes(self.workspace, self.target)
            self.assertEqual(restored, 2)  # src/a.rs and src/
            self.assertEqual(mtime(self.workspace / "src/a.rs"), first)
            self.assertEqual(mtime(self.workspace / "src/b.rs"), second)

        with self.subTest(phase="a job that never seals leaves nothing to trust"):
            self.assertFalse((self.target / slot_cache.SEALED).exists())
            checkout(self.workspace, {"src/a.rs": "a", "src/b.rs": "b changed"}, third)
            restored, _ = slot_cache.restore_mtimes(self.workspace, self.target)
            self.assertEqual(restored, 0)
            self.assertEqual(mtime(self.workspace / "src/a.rs"), third)

        with self.subTest(phase="a file a step rewrote after mount is not vouched for"):
            (self.workspace / "src/b.rs").write_text("generated", encoding="utf-8")
            os.utime(self.workspace / "src/b.rs", ns=(third + 10**9, third + 10**9))
            slot_cache.seal(mock.Mock(workspace=str(self.workspace)))
            sealed = slot_cache.load_manifest(self.target / slot_cache.SEALED)
            self.assertEqual(set(sealed), {"src/a.rs", "src/"})
            checkout(self.workspace, {"src/a.rs": "a", "src/b.rs": "b changed"}, 4 * 10**18)
            slot_cache.restore_mtimes(self.workspace, self.target)
            self.assertEqual(mtime(self.workspace / "src/a.rs"), third)
            self.assertEqual(mtime(self.workspace / "src/b.rs"), 4 * 10**18)

    def test_directory_mtimes_are_restored_while_their_tracked_entries_stay(self) -> None:
        first, second, third = 10**18, 2 * 10**18, 3 * 10**18

        def fresh_checkout(files: dict[str, str], stamp: int) -> None:
            for entry in self.workspace.iterdir():
                if entry.name not in (".git", "target"):
                    shutil.rmtree(entry)
            checkout(self.workspace, files, stamp)
            for directory in {Path(name).parent for name in files} - {Path(".")}:
                for parent in (directory, *directory.parents):
                    if parent != Path("."):
                        os.utime(self.workspace / parent, ns=(stamp, stamp))

        files = {"shader/a.wgsl": "a", "shader/shared/b.wgsl": "b"}
        with self.subTest(phase="a sealed job vouches for its directories"):
            fresh_checkout(files, first)
            slot_cache.restore_mtimes(self.workspace, self.target)
            slot_cache.seal(mock.Mock(workspace=str(self.workspace)))
            sealed = slot_cache.load_manifest(self.target / slot_cache.SEALED)
            self.assertIn("shader/", sealed)
            self.assertIn("shader/shared/", sealed)

        with self.subTest(phase="a new checkout with the same entries gets their mtimes back"):
            fresh_checkout(files, second)
            slot_cache.restore_mtimes(self.workspace, self.target)
            self.assertEqual(mtime(self.workspace / "shader"), first)
            self.assertEqual(mtime(self.workspace / "shader/shared"), first)
            slot_cache.seal(mock.Mock(workspace=str(self.workspace)))

        with self.subTest(phase="an added entry keeps the checkout's mtime"):
            fresh_checkout({**files, "shader/shared/c.wgsl": "c"}, third)
            slot_cache.restore_mtimes(self.workspace, self.target)
            self.assertEqual(mtime(self.workspace / "shader"), first)
            self.assertEqual(mtime(self.workspace / "shader/shared"), third)

        with self.subTest(phase="a file a step adds unseals its directory"):
            (self.workspace / "shader/generated.rs").write_text("x", encoding="utf-8")
            slot_cache.seal(mock.Mock(workspace=str(self.workspace)))
            sealed = slot_cache.load_manifest(self.target / slot_cache.SEALED)
            self.assertNotIn("shader/", sealed)
            self.assertIn("shader/shared/", sealed)

    def select(self, root: Path, key: str, now: float, *, used: int = 10, size: int = 1):
        with mock.patch.object(slot_cache, "used_percent", return_value=used), \
             mock.patch.object(slot_cache, "tree_bytes", return_value=size), \
             mock.patch.object(slot_cache, "empty_trash",
                               side_effect=lambda t: shutil.rmtree(t, ignore_errors=True)):
            return slot_cache.select_target(root, key, now)

    def test_target_selection_bounds_age_size_keys_and_job_state(self) -> None:
        root = self.tmp / "volume"
        now = 100 * DAY
        with self.subTest(phase="a new key starts empty and keeps only Cargo's entries"):
            target, state = self.select(root, "k1", now)
            self.assertEqual(state, "new")
            for entry in ("debug/deps", "nextest/ci", "ci", "tmp"):
                (target / entry).mkdir(parents=True)
            (target / "debug/deps/libx.rlib").write_text("x", encoding="utf-8")
            target, state = self.select(root, "k1", now + 60)
            self.assertEqual(state, "warm")
            self.assertTrue((target / "debug/deps/libx.rlib").exists())
            for entry in ("nextest", "ci", "tmp"):
                self.assertFalse((target / entry).exists(), entry)

        with self.subTest(phase="a week-old key directory is wiped"):
            target, state = self.select(root, "k1", now + 8 * DAY)
            self.assertEqual(state, "wiped")
            self.assertFalse((target / "debug").exists())
            self.assertEqual(json.loads((target / slot_cache.META).read_text())["created"],
                             now + 8 * DAY)

        with self.subTest(phase="an oversized key directory is wiped"):
            (target / "debug").mkdir()
            _, state = self.select(root, "k1", now + 8 * DAY + 60,
                                   size=slot_cache.MAX_TARGET_BYTES + 1)
            self.assertEqual(state, "wiped")

        with self.subTest(phase="only the most recently used other key survives"):
            self.select(root, "k2", now + 9 * DAY)
            self.select(root, "k3", now + 10 * DAY)
            keys = sorted(d.name for d in (root / "target").iterdir())
            self.assertEqual(keys, ["k2", "k3"])

        with self.subTest(phase="disk pressure keeps nothing but an empty current key"):
            (root / "target/k3/debug").mkdir()
            _, state = self.select(root, "k3", now + 10 * DAY + 60,
                                   used=slot_cache.PRESSURE_PERCENT)
            self.assertEqual(state, "wiped")
            self.assertEqual(sorted(d.name for d in (root / "target").iterdir()), ["k3"])
            self.assertFalse((root / "target/k3/debug").exists())

    def test_workflows_do_not_mount_a_runner_slot_cache(self) -> None:
        root = Path(__file__).resolve().parents[2]
        workflows = root / ".forgejo/workflows"
        for path in sorted(workflows.glob("*.y*ml")):
            source = path.read_text(encoding="utf-8")
            self.assertNotIn("tilo-slot-cache", source, path.name)
            self.assertNotIn("slot-cache:", source, path.name)
            self.assertNotIn("slot_cache.py", source, path.name)
        action = (root / ".forgejo/actions/prepare-rust-build/action.yml").read_text(encoding="utf-8")
        self.assertNotIn("slot_cache.py", action)
        self.assertNotIn("slot-cache", action)

    def test_cache_problems_build_cold_instead_of_failing(self) -> None:
        output = self.tmp / "output"
        with mock.patch.dict(os.environ, {"GITHUB_OUTPUT": str(output)}):
            with self.subTest(phase="no volume mounted"):
                code = slot_cache.main(["mount", "--trust", "untrusted",
                                        "--volume", str(self.tmp / "absent"),
                                        "--workspace", str(self.workspace)])
                self.assertEqual(code, 0)
                self.assertIn("enabled=false", output.read_text(encoding="utf-8"))
            with self.subTest(phase="an unexpected error"):
                output.unlink()
                with mock.patch.object(slot_cache.os.path, "ismount", side_effect=OSError("boom")):
                    code = slot_cache.main(["mount", "--trust", "trusted",
                                            "--workspace", str(self.workspace)])
                self.assertEqual(code, 0)
                self.assertIn("enabled=false", output.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()

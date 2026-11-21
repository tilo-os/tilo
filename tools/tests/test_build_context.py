"""The build context of a Containerfile holds the files it copies, at the build's clock."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
STAGE = ROOT / "os/dev/build-context.py"


class BuildContextTests(unittest.TestCase):
    def stage(self, containerfile, *files, script=None, epoch=1700000000, directory=None):
        # Kept until the test ends, so a test can read what was staged.
        directory = directory or self.enterContext(tempfile.TemporaryDirectory())
        repo = Path(directory) / "repo"
        repo.mkdir()
        (repo / "Containerfile").write_text(script or "")
        for name in files:
            path = repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(name)
        dest = Path(directory) / "dest"
        result = subprocess.run(
            [sys.executable, "-I", str(STAGE), str(repo / containerfile),
             "--repo", str(repo), "--dest", str(dest), "--epoch", str(epoch)],
            capture_output=True, text=True)
        staged = sorted(p.relative_to(dest).as_posix() for p in dest.rglob("*")) if dest.exists() else []
        return result, staged, dest

    def test_only_the_copied_files_are_in_the_context(self):
        result, staged, _ = self.stage(
            "Containerfile",
            "os/scripts/one.sh", "os/dev/two.py", "os/usr/lib/a/b.conf",
            "os/dev/a-test-nobody-copies.sh",
            script="""FROM scratch
COPY os/scripts/one.sh os/dev/two.py /bin/
COPY --chmod=0755 \\
  os/usr/lib/a /usr/lib/a
COPY --from=build /out /out
RUN --mount=type=bind,source=os/dev/two.py,target=/two cat /two
""")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(staged, ["os", "os/dev", "os/dev/two.py", "os/scripts", "os/scripts/one.sh",
                                  "os/usr", "os/usr/lib", "os/usr/lib/a", "os/usr/lib/a/b.conf"])

    def test_a_path_chosen_by_a_build_argument_is_every_choice(self):
        result, staged, _ = self.stage(
            "Containerfile",
            "os/hardware/firmware/one/fw.bin", "os/hardware/firmware/two/fw.bin",
            "os/hardware/profiles/one.json",
            script="FROM scratch\nCOPY os/hardware/firmware/${TILO_HARDWARE_PROFILE} /firmware/\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("os/hardware/firmware/one/fw.bin", staged)
        self.assertIn("os/hardware/firmware/two/fw.bin", staged)
        self.assertNotIn("os/hardware/profiles/one.json", staged)

    def test_the_files_are_dated_by_the_builds_clock(self):
        result, _, dest = self.stage(
            "Containerfile", "os/a.sh", "os/b/c.sh",
            script="FROM scratch\nCOPY os/a.sh os/b/c.sh /\n", epoch=1600000000)
        self.assertEqual(result.returncode, 0, result.stderr)
        for name in ("os/a.sh", "os/b/c.sh"):
            self.assertEqual(int(os.stat(dest / name).st_mtime), 1600000000, name)

    def test_a_copied_file_the_repository_lacks_fails(self):
        result, _, _ = self.stage("Containerfile", script="FROM scratch\nCOPY os/missing.sh /\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("os/missing.sh", result.stderr)

    def test_its_default_place_is_outside_the_repository(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory) / "repo"
            repo.mkdir()
            (repo / "Containerfile").write_text("FROM scratch\nCOPY a.sh /\n")
            (repo / "a.sh").write_text("a")
            result = subprocess.run(
                [sys.executable, "-I", str(STAGE), str(repo / "Containerfile"), "--repo", str(repo),
                 "--epoch", "1700000000"],
                capture_output=True, text=True, env={**os.environ, "TILO_CONTEXT_SCRATCH": directory})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(Path(result.stdout.strip()).parent, Path(directory))


if __name__ == "__main__":
    unittest.main()

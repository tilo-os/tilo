#!/usr/bin/env python3
"""Exercise the actual source archive and packaging copy boundary, without builds."""
import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest


CI = Path(__file__).resolve().parents[1]


class PackageSourceTransferTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "source with spaces"
        (self.source / "dist/ci").mkdir(parents=True)
        for name in ("run-package-job.sh", "archive-package-source.sh"):
            shutil.copyfile(CI / name, self.source / "dist/ci" / name)
        for name, contents in {
            ".git/HEAD": "ref: refs/heads/main\n",
            "Cargo.toml": "changed tracked source\n",
            "new source.rs": "untracked source\n",
            "os/external-assets/cache/input.tar.gz": "cached input\n",
            "crates/target/schema.txt": "nested source\n",
            "target/release/shell": "local binary must stay local\n",
            "dist/ci-output/previous-amd64/result.nar.gz": "prior distro artifact\n",
        }.items():
            path = self.source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(contents)
        (self.source / "source-link").symlink_to("new source.rs")

    def archive(self):
        return subprocess.run(
            ["bash", str(CI / "archive-package-source.sh"), str(self.source)],
            check=True, capture_output=True,
        ).stdout

    def test_archive_preserves_inputs_without_recursive_build_output(self):
        with tarfile.open(fileobj=io.BytesIO(self.archive())) as archive:
            names = set(archive.getnames())
            for name in (".git/HEAD", "Cargo.toml", "new source.rs",
                         "os/external-assets/cache/input.tar.gz", "crates/target/schema.txt"):
                member = "workspace/" + name
                self.assertIn(member, names)
                self.assertEqual(archive.extractfile(member).read(), (self.source / name).read_bytes())
            self.assertFalse(any(name == "workspace/target" or name.startswith("workspace/target/")
                                 or name.startswith("workspace/dist/ci-output") for name in names))
            self.assertEqual(archive.getmember("workspace/source-link").linkname, "new source.rs")
        self.assertEqual((self.source / "dist/ci-output/previous-amd64/result.nar.gz").read_text(),
                         "prior distro artifact\n")
        self.assertTrue((self.source / "target/release/shell").is_file())

    def test_missing_source_fails(self):
        result = subprocess.run(["bash", str(CI / "archive-package-source.sh"),
                                 str(self.root / "absent")], capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, b"")

    def test_growing_previous_artifacts_does_not_grow_transfer(self):
        before = self.archive()
        previous = self.source / "dist/ci-output/previous-amd64/result.nar.gz"
        target = self.source / "target/release/shell"
        for path in (previous, target):
            with path.open("ab") as stream:
                stream.write(b"x" * (8 * 1024 * 1024))
        self.assertEqual(self.archive(), before)
        self.assertGreater(previous.stat().st_size, 8 * 1024 * 1024)
        self.assertGreater(target.stat().st_size, 8 * 1024 * 1024)

    def run_packaging_boundary(self, failure=""):
        binaries = self.root / "bin"
        binaries.mkdir()
        docker = binaries / "docker"
        docker.write_text('''#!/usr/bin/env python3
import os, pathlib, sys
root = pathlib.Path(os.environ["TRANSFER_TEST_STATE"])
args = sys.argv[1:]
with (root / "calls").open("a") as calls:
    calls.write(" ".join(args) + "\\n")
if args[0] == "create":
    print("fixture-container")
elif args[:2] == ["cp", "-"]:
    assert args[2] == "fixture-container:/", args
    (root / "received.tar").write_bytes(sys.stdin.buffer.read())
    if os.environ.get("TRANSFER_TEST_FAILURE") == "docker":
        print("copy deliberately failed", file=sys.stderr)
        sys.exit(29)
elif args[0] == "cp":
    assert args[1] == "fixture-container:/artifacts/.", args
    (pathlib.Path(args[2]) / "new-package.deb").write_text("verified output\\n")
''')
        docker.chmod(0o755)
        if failure == "archive":
            (self.source / "dist/ci/archive-package-source.sh").write_text(
                "#!/usr/bin/env bash\necho archive-failure >&2\nexit 31\n")
        env = dict(os.environ, PATH=str(binaries) + os.pathsep + os.environ["PATH"],
                   SOURCE_DATE_EPOCH="1", GITHUB_SHA="a" * 40,
                   TRANSFER_TEST_STATE=str(self.root), TRANSFER_TEST_FAILURE=failure)
        result = subprocess.run(["bash", str(self.source / "dist/ci/run-package-job.sh"),
                                 "fixture", "fixture-image", "amd64"],
                                env=env, capture_output=True, text=True)
        return result, (self.root / "calls").read_text()

    def test_packaging_uses_filtered_stream_and_preserves_all_outputs(self):
        result, calls = self.run_packaging_boundary()
        self.assertEqual(result.returncode, 0, result.stderr)
        with tarfile.open(self.root / "received.tar") as archive:
            self.assertIn("workspace/Cargo.toml", archive.getnames())
            self.assertNotIn("workspace/target", archive.getnames())
            self.assertNotIn("workspace/dist/ci-output", archive.getnames())
        self.assertIn("start --attach fixture-container", calls)
        self.assertEqual((self.source / "dist/ci-output/fixture-amd64/new-package.deb").read_text(),
                         "verified output\n")
        self.assertTrue((self.source / "dist/ci-output/previous-amd64/result.nar.gz").is_file())

    def test_failed_archiver_prevents_build(self):
        result, calls = self.run_packaging_boundary("archive")
        self.assertEqual(result.returncode, 31, result.stderr)
        self.assertNotIn("start --attach", calls)
        diagnostic = (self.source / "dist/ci-output/fixture-amd64/CI_FAILURE").read_text()
        self.assertIn("stage=copy-source status=31", diagnostic)
        self.assertIn("archive-failure", diagnostic)

    def test_failed_docker_copy_prevents_build(self):
        result, calls = self.run_packaging_boundary("docker")
        self.assertEqual(result.returncode, 29, result.stderr)
        self.assertNotIn("start --attach", calls)
        self.assertIn("stage=copy-source status=29",
                      (self.source / "dist/ci-output/fixture-amd64/CI_FAILURE").read_text())


if __name__ == "__main__":
    unittest.main()

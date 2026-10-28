"""tools/ci/t-git-index.sh: the throwaway Git index a t check of the whole tree runs with."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "ci/t-git-index.sh"


class GitIndexTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.tree = Path(temporary.name) / "tree"
        (self.tree / "src").mkdir(parents=True)
        (self.tree / "ignored").mkdir()
        (self.tree / ".gitignore").write_text("ignored/\n")
        (self.tree / "README").write_text("text\n")
        (self.tree / "src/lib.rs").write_bytes(b"fn main() {}\r\n")
        (self.tree / "ignored/out.bin").write_bytes(b"\x00")
        self.report = Path(temporary.name) / "report"
        self.env = {key: value for key, value in os.environ.items()
                    if key not in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE")}
        self.env["TMPDIR"] = temporary.name
        self.env["GIT_CEILING_DIRECTORIES"] = temporary.name

    def run_with_index(self, command: str) -> subprocess.CompletedProcess:
        return subprocess.run(["bash", str(SCRIPT), "bash", "-c", command], cwd=self.tree, env=self.env,
                              capture_output=True, text=True, check=False)

    def test_the_index_holds_the_files_present_with_readable_objects(self):
        result = self.run_with_index(
            f'{{ git ls-files; git cat-file -p :README; echo "$GIT_DIR"; }} > "{self.report}"')
        self.assertEqual(result.returncode, 0, result.stderr)
        lines = self.report.read_text().splitlines()
        self.assertEqual(lines[:4], [".gitignore", "README", "src/lib.rs", "text"])
        self.assertFalse(Path(lines[-1]).exists(), "the throwaway repository outlives the command")

    def test_the_commands_exit_status_is_the_scripts(self):
        result = self.run_with_index(f'echo "$GIT_DIR" > "{self.report}"; exit 3')
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertFalse(Path(self.report.read_text().strip()).exists())

    def test_inside_a_repository_it_runs_the_command_in_that_repository(self):
        subprocess.run(["git", "init", "-q"], cwd=self.tree, env=self.env, check=True)
        result = self.run_with_index(f'git rev-parse --absolute-git-dir > "{self.report}"')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(Path(self.report.read_text().strip()), (self.tree / ".git").resolve())


if __name__ == "__main__":
    unittest.main()

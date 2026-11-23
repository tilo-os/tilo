"""The words-that-go check refuses the names a commit adds."""
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/policy"))
import check_words_that_go as words  # noqa: E402
import judged  # noqa: E402

# A name the table rejects, composed so this file itself names none of it.
STAND_IN = "Mail" + "Stand" + "In"


def git(cwd, *args):
    subprocess.run(["git", "-C", str(cwd), *args], check=True, capture_output=True)


class StagedWordsThatGoTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        git(self.root, "init", "-q")
        git(self.root, "config", "user.name", "test")
        git(self.root, "config", "user.email", "test@example.invalid")

    def tearDown(self):
        self.temporary.cleanup()

    def commit(self, files):
        for name, text in files.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "-m", "state")

    def stage(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        git(self.root, "add", name)

    def problems(self, held=()):
        """The staged check's verdict, told what the tree already holds."""
        return words.staged_problems(self.root, words.load_rows(), set(held))

    def test_a_commit_that_adds_a_flagged_name_is_refused(self):
        self.commit({"src/lib.rs": "fn main() {}\n"})
        self.stage("src/lib.rs", f"fn main() {{}}\nstruct {STAND_IN};\n")
        problems = self.problems()
        self.assertTrue(
            any(STAND_IN in problem for problem in problems), problems
        )

    def test_a_commit_that_only_calls_a_name_the_tree_holds_passes(self):
        self.commit({"src/mail_double.rs": f"struct {STAND_IN};\n"})
        self.stage("src/lib.rs", f"fn main() {{ let _ = {STAND_IN}; }}\n")
        self.assertEqual(self.problems(held=[STAND_IN]), [])

    def test_a_commit_that_moves_a_flagged_name_between_files_passes(self):
        self.commit({"src/mail_double.rs": f"struct {STAND_IN};\n"})
        (self.root / "src/mail_double.rs").unlink()
        self.stage("src/mail.rs", f"struct {STAND_IN};\n")
        git(self.root, "add", "-A")
        self.assertEqual(self.problems(held=[STAND_IN]), [])

    def test_a_commit_costs_no_walk_over_the_tree(self):
        """A commit of many files still reads one listing and one blob batch, never HEAD."""
        files = {f"src/part{index:02}.rs": f"pub fn part{index}() -> u32 {{ {index} }}\n"
                 for index in range(50)}
        self.commit({"src/lib.rs": "fn main() {}\n"})
        for name, text in files.items():
            self.stage(name, text)
        self.stage("src/lib.rs", f"fn main() {{}}\nstruct {STAND_IN};\n")
        calls = []
        originals = (words.git, judged.git)

        def recording(original):
            def call(root, *args, **kwargs):
                calls.append(args[0])
                return original(root, *args, **kwargs)
            return call

        words.git, judged.git = recording(originals[0]), recording(originals[1])
        try:
            problems = self.problems()
        finally:
            words.git, judged.git = originals
        self.assertTrue(any(STAND_IN in problem for problem in problems), problems)
        self.assertNotIn("grep", calls)
        self.assertLessEqual(len(calls), 3, calls)


if __name__ == "__main__":
    unittest.main()

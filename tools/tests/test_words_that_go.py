"""The words-that-go check refuses the names a commit adds."""
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/policy"))
import check_words_that_go as words  # noqa: E402

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

    def test_a_commit_that_adds_a_flagged_name_is_refused(self):
        self.commit({"src/lib.rs": "fn main() {}\n"})
        self.stage("src/lib.rs", f"fn main() {{}}\nstruct {STAND_IN};\n")
        problems = words.staged_problems(self.root, words.load_rows())
        self.assertTrue(
            any(STAND_IN in problem for problem in problems), problems
        )

    def asked_of_the_tree(self):
        """The staged check's verdict, and every set of names it looked up in the tree."""
        asked = []
        in_head = words.in_head

        def recording(root, names):
            asked.append(set(names))
            return in_head(root, names)

        words.in_head = recording
        try:
            problems = words.staged_problems(self.root, words.load_rows())
        finally:
            words.in_head = in_head
        return problems, [names for names in asked if names]

    def test_the_tree_is_asked_only_about_a_name_the_table_could_reject(self):
        self.commit({"src/lib.rs": "fn main() {}\n"})
        ordinary = "fn main() { let total = 1; if total > 0 { return; } }\n"
        self.stage("src/lib.rs", ordinary)
        self.assertEqual(self.asked_of_the_tree(), ([], []))
        self.stage("src/lib.rs", f"{ordinary}struct {STAND_IN};\n")
        problems, asked = self.asked_of_the_tree()
        self.assertTrue(any(STAND_IN in problem for problem in problems), problems)
        self.assertEqual(asked, [{STAND_IN}])

    def test_a_commit_that_only_calls_a_name_the_tree_holds_passes(self):
        self.commit({"src/mail_double.rs": f"struct {STAND_IN};\n"})
        self.stage("src/lib.rs", f"fn main() {{ let _ = {STAND_IN}; }}\n")
        problems = words.staged_problems(self.root, words.load_rows())
        self.assertEqual(problems, [])

    def test_a_commit_that_moves_a_flagged_name_between_files_passes(self):
        self.commit({"src/mail_double.rs": f"struct {STAND_IN};\n"})
        (self.root / "src/mail_double.rs").unlink()
        self.stage("src/mail.rs", f"struct {STAND_IN};\n")
        git(self.root, "add", "-A")
        problems = words.staged_problems(self.root, words.load_rows())
        self.assertEqual(problems, [])


if __name__ == "__main__":
    unittest.main()

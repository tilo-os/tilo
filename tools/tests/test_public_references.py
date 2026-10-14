import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_public_references.py"
SPEC = importlib.util.spec_from_file_location("public_references", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(contract)

# Built from parts so this file passes the check it exercises.
KEY = "TIL" + "-512"


class PublicReferenceTests(unittest.TestCase):
    def test_issue_keys_links_and_tracker_documents_are_internal(self):
        for text in (
            "fix the focus loss (" + KEY + ")",
            "fn focus_survives_" + KEY.lower().replace("-", "") + "() {}",
            'label = "' + KEY.lower() + '-after"',
            "see https://linear" + ".app/tilo/issue/x",
            "targets come from the Linear" + " document",
            "spec at https://www.notion" + ".so/tilo/abc",
            "thread: https://tilo.slack" + ".com/archives/C01/p1",
        ):
            with self.subTest(text=text):
                self.assertIsNotNone(contract.internal_reference(text))

    def test_public_and_unrelated_text_is_allowed(self):
        for text in (
            "fixed in #3986",
            "util-2 and until-3 are not keys",
            "Linear projection of the die layout",
            "linear interpolation",
            "STILL-4",
            "slack.com is a chat service",
        ):
            with self.subTest(text=text):
                self.assertIsNone(contract.internal_reference(text))

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "-q")

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.root), *args])

    def write(self, name, text):
        (self.root / name).write_text(text)

    def test_tracked_files_are_audited_and_untracked_files_ignored(self):
        self.write("clean.md", "Fixed in #3986.\n")
        self.write("notes.md", "first line\nfixes " + KEY + "\n")
        self.write("untracked.md", KEY + "\n")
        self.git("add", "clean.md", "notes.md")
        self.assertEqual(contract.audit(self.root), ["notes.md:2: " + KEY])

    def test_staged_mode_reads_the_index_not_the_worktree(self):
        self.write("notes.md", "fixes " + KEY + "\n")
        self.git("add", "notes.md")
        self.write("notes.md", "fixed the focus loss\n")
        self.assertEqual(contract.audit(self.root, staged=True), ["notes.md:1: " + KEY])
        self.git("add", "notes.md")
        (self.root / "notes.md").write_text(KEY + "\n")
        self.assertEqual(contract.audit(self.root, staged=True), [])


if __name__ == "__main__":
    unittest.main()

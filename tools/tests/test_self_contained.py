import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_self_contained.py"
SPEC = importlib.util.spec_from_file_location("self_contained", SCRIPT)
check = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check)

# Spelled in pieces so this file does not trip the check it tests.
ID = "TI" + "L-123"
TRACKER = "https://linear" + ".app/acme/issue/" + ID
NOTION = "https://www.notion" + ".so/page"
THREAD = "https://acme.slack" + ".com/archives/C1/p2"


class PatternTests(unittest.TestCase):
    def test_ids_and_private_hosts_are_found(self):
        for text in (ID, "fix it (" + ID + ")", "branch bob/" + ID.lower() + "-x", TRACKER, NOTION, THREAD):
            with self.subTest(text=text):
                self.assertIsNotNone(check.find_reference(text))

    def test_ordinary_text_is_not_flagged(self):
        for text in ("until-1", "Curve::Linear.apply(0.5)", "fuss-12", "https://example.org/linear", "TIL without a number"):
            with self.subTest(text=text):
                self.assertIsNone(check.find_reference(text))


class RepositoryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "-q", "-b", "main")

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.root), *args], capture_output=True, check=True)

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        self.git("add", "--", name)

    def test_tree_scan_reports_hits_but_exempts_vendor(self):
        self.write("src/a.rs", "// clean\n")
        self.write("src/b.rs", "// see " + TRACKER + "\n")
        self.write("vendor/upstream/c.rs", "// " + ID + "\n")
        hits = list(check.tree_hits(self.root))
        self.assertEqual(len(hits), 1)
        self.assertTrue(hits[0].startswith("src/b.rs:"))

    def test_staged_scan_checks_only_added_lines(self):
        self.write("src/a.rs", "// " + ID + " already there\n")
        self.git("-c", "user.name=t", "-c", "user.email=t@example.org", "-c", "commit.gpgsign=false", "commit", "-qm", "chore: seed")
        self.write("src/a.rs", "// " + ID + " already there\n// fine\n")
        self.assertEqual(list(check.staged_hits(self.root)), [])
        self.write("src/a.rs", "// " + ID + " already there\n// fine\n// " + NOTION + "\n")
        hits = list(check.staged_hits(self.root))
        self.assertEqual(len(hits), 1)
        self.assertTrue(hits[0].startswith("src/a.rs:3:"))


if __name__ == "__main__":
    unittest.main()

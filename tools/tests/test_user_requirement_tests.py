from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/policy/check_user_requirement_tests.py"

PINNED = '''\
fn helper() -> u32 {
    let s = "a } brace in a string { and a '{'";
    assert!(s.len() > 0);
    1
}

// USER REQUIREMENT (2026-10-18): "it pushes the apps next to it, it does
// not resize them"
#[test]
#[allow(
    clippy::float_cmp,
    reason = "exact"
)]
fn only_the_grabbed_window_changes() {
    let widths = drag();
    assert_eq!(widths, [0.0, 40.0, 0.0], "only the middle one");
    assert_widths(&widths, [0.0, 40.0, 0.0]);
}

#[test]
fn unpinned() {
    assert_eq!(1, 1);
}
'''


class UserRequirementTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "-q", "-b", "main")
        for key, value in (("user.name", "Test"), ("user.email", "test@example.org"), ("commit.gpgsign", "false")):
            self.git("config", key, value)
        self.commit({"src/resize.rs": PINNED}, "test: pin the resize requirement")
        self.base = self.git("rev-parse", "HEAD").stdout.strip()

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.root), *args], capture_output=True, text=True, check=True)

    def commit(self, files, message):
        for name, text in files.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            if text is None:
                path.unlink()
            else:
                path.write_text(text)
        self.git("add", "-A")
        self.git("commit", "-qm", message)

    def check(self):
        return subprocess.run(
            ["python3", str(SCRIPT), self.base], cwd=self.root, capture_output=True, text=True
        )

    def edit(self, old, new, message="fix: change it"):
        self.commit({"src/resize.rs": PINNED.replace(old, new)}, message)

    def test_an_untouched_pinned_test_passes(self):
        self.edit("assert_eq!(1, 1);", "assert_eq!(2, 2);", "test: change an unpinned test")
        self.assertEqual(self.check().returncode, 0, self.check().stderr)

    def test_reformatting_an_assertion_passes(self):
        self.edit(
            'assert_eq!(widths, [0.0, 40.0, 0.0], "only the middle one");',
            'assert_eq!(\n        widths,\n        [0.0, 40.0, 0.0],\n        "only the middle one"\n    );',
        )
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_changing_an_assertion_fails(self):
        self.edit("[0.0, 40.0, 0.0], \"only", "[40.0, 0.0, 0.0], \"only")
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("only_the_grabbed_window_changes", result.stderr)
        self.assertIn("assertions changed", result.stderr)

    def test_removing_an_assertion_helper_call_fails(self):
        self.edit("    assert_widths(&widths, [0.0, 40.0, 0.0]);\n", "")
        self.assertEqual(self.check().returncode, 1)

    def test_renaming_or_deleting_the_test_fails(self):
        self.edit("fn only_the_grabbed_window_changes", "fn renamed")
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("removed or renamed", result.stderr)

    def test_dropping_the_marker_fails(self):
        self.edit('// USER REQUIREMENT (2026-10-18): "it pushes', '// "it pushes')
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("lost its USER REQUIREMENT marker", result.stderr)

    def test_moving_the_test_to_another_file_in_the_same_commit_passes(self):
        start = PINNED.index("// USER REQUIREMENT")
        end = PINNED.index("#[test]\nfn unpinned")
        self.commit(
            {"src/resize.rs": PINNED[:start] + PINNED[end:], "src/edges.rs": PINNED[start:end]},
            "refactor: move the resize tests",
        )
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_the_users_ok_in_a_trailer_lets_the_change_through(self):
        self.edit(
            "[0.0, 40.0, 0.0], \"only",
            "[40.0, 0.0, 0.0], \"only",
            "fix: give the seam back to the left window\n\nUser-approved: the user asked for this on 2026-10-19",
        )
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()

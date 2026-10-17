import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import MagicMock, patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/policy"))

from added_line_policy import (  # noqa: E402
    SECRET_PATTERNS, added_line_issues, added_lines, object_reader, scan_all, scan_staged,
)

# Built from parts so this file passes the scan it exercises.
SAMPLES = {
    "AWS access key": "AKIA" + "QZ7RXW2MPLK4TNVB",
    "GitHub token": "ghp_" + "a1" * 18,
    "GitLab token": "glpat-" + "b2" * 10,
    "Slack token": "xoxb-" + "1234567890",
    "Stripe key": "sk_live_" + "c3" * 12,
    "Anthropic key": "sk-ant-" + "d4" * 10,
    "OpenAI key": "sk-" + "e5" * 24,
    "Google API key": "AIza" + "f" * 35,
    "private key block": "-----BEGIN " + "RSA PRIVATE KEY-----",
    "PuTTY private key": "PuTTY-User-" + "Key-File-2: ssh-rsa",
    "JWT": "eyJ" + "g" * 12 + ".eyJ" + "h" * 12 + ".sig",
    "npm token": "npm_" + "i" * 36,
    "password in a URL": "https://deploy:" + "hunter22" + "@registry.invalid/",
}
CONFLICT = "<" * 7 + " HEAD"
DBG = "    " + "dbg" + "!(value);"


def run(*args, cwd, check=True):
    return subprocess.run(list(args), cwd=cwd, capture_output=True, text=True, check=check)


class AddedLineIssuesTests(unittest.TestCase):
    def test_one_hit_per_credential_pattern_without_printing_it(self):
        self.assertEqual({name for name, _ in SECRET_PATTERNS}, set(SAMPLES))
        for name, secret in SAMPLES.items():
            with self.subTest(name=name):
                issues = added_line_issues("config.toml", [(3, "token = " + secret)])
                self.assertIn("config.toml:3: possible " + name, "\n".join(issues))
                self.assertNotIn(secret, "\n".join(issues))

    def test_same_line_allow_word_admits_a_fixture(self):
        for secret in SAMPLES.values():
            with self.subTest(secret=secret[:6]):
                self.assertEqual(added_line_issues("a.md", [(1, secret + "  # example")]), [])
                self.assertEqual(added_line_issues("a.md", [(1, "PLACEHOLDER " + secret)]), [])

    def test_conflict_markers_fire_in_every_text_file(self):
        for marker in (CONFLICT, "=" * 7, ">" * 7 + " feature"):
            for path in ("a.rs", "docs/a.md", "vendor/x/a.c"):
                with self.subTest(marker=marker, path=path):
                    self.assertEqual(added_line_issues(path, [(9, marker)]), [path + ":9: merge-conflict marker"])
        self.assertEqual(added_line_issues("a.md", [(1, "=" * 8), (2, "<" * 7 + "x")]), [])

    def test_dbg_fires_only_in_own_rust_sources(self):
        self.assertEqual(added_line_issues("crates/a/src/lib.rs", [(4, DBG)]), ["crates/a/src/lib.rs:4: dbg!"])
        self.assertEqual(added_line_issues("vendor/crate/src/lib.rs", [(4, DBG)]), [])
        self.assertEqual(added_line_issues("docs/debugging.md", [(4, DBG)]), [])
        self.assertEqual(added_line_issues("a.rs", [(4, "    my_" + "dbg" + "!(value);")]), [])

    def test_secrets_only_skips_markers_and_dbg(self):
        self.assertEqual(added_line_issues("a.rs", [(1, CONFLICT), (2, DBG)], secrets_only=True), [])

    def test_existing_line_does_not_fire_again(self):
        old = ("fn main() {}\n" + DBG + "\n").encode()
        new = ("fn main() {}\n" + DBG + "\nfn other() {}\n").encode()
        self.assertEqual(added_lines(new, [old]), [(3, "fn other() {}")])
        self.assertEqual(added_line_issues("a.rs", added_lines(new, [old])), [])
        # A merge: a line from either parent is not added by the merge.
        self.assertEqual(added_lines(b"left\nright\nboth\n", [b"left\n", b"right\n"]), [(3, "both")])
        self.assertEqual(added_lines(b"binary\0" + CONFLICT.encode(), []), [])


class ObjectReaderTests(unittest.TestCase):
    def process(self, response, returncode=0):
        process = MagicMock()
        process.__enter__.return_value = process
        process.stdin = io.BytesIO()
        process.stdout = io.BytesIO(response)
        process.wait.return_value = returncode
        return process

    def test_failed_responses_cannot_pass_a_full_scan(self):
        for response, returncode in (
            (b"abc blob 20\nclean\n", 0),
            (b"abc blob 5\nclean", 0),
            (b"abc blob 5\nclean!", 0),
            (b"abc blob -1\n", 0),
            (b"abc blob 5\nclean\n", 1),
        ):
            with self.subTest(response=response, returncode=returncode):
                process = self.process(response, returncode)
                with patch("added_line_policy.subprocess.Popen", return_value=process), \
                     patch("added_line_policy.git", return_value=b"100644 abc 0\tnotes.md\0"):
                    with self.assertRaises(ValueError):
                        scan_all(ROOT, secrets_only=False)

    def test_required_index_blobs_cannot_be_missing_or_non_blobs(self):
        for response in (b"abc missing\n", b"abc tree 0\n\n"):
            for scan in (scan_all, scan_staged):
                with self.subTest(response=response, scan=scan.__name__):
                    process = self.process(response)
                    listing = (b"100644 abc 0\tnotes.md\0" if scan is scan_all
                               else b":000000 100644 000 abc A\0notes.md\0")
                    with patch("added_line_policy.subprocess.Popen", return_value=process), \
                         patch("added_line_policy.staged_parents", return_value=["HEAD"]), \
                         patch("added_line_policy.git", side_effect=lambda root, command, *args:
                               b"" if command == "ls-tree" else listing):
                        with self.assertRaises(ValueError):
                            scan(ROOT, secrets_only=False)

    def test_parent_tree_blob_cannot_be_unreadable(self):
        process = self.process(b"abc blob 5\nclean\nold missing\n")
        with patch("added_line_policy.subprocess.Popen", return_value=process), \
             patch("added_line_policy.staged_parents", return_value=["HEAD"]), \
             patch("added_line_policy.git", side_effect=lambda root, command, *args:
                   b"100644 blob old\tnotes.md\0" if command == "ls-tree"
                   else b":100644 100644 old abc M\0notes.md\0"):
            with self.assertRaisesRegex(ValueError, "cannot inspect parent blob"):
                scan_staged(ROOT, secrets_only=False)

    def test_missing_parent_and_non_blob_remain_optional(self):
        process = self.process(b"HEAD:absent missing\nabc tree 0\n\nabc blob 5\nclean\n")
        with patch("added_line_policy.subprocess.Popen", return_value=process):
            with object_reader(ROOT) as read:
                self.assertIsNone(read("HEAD:absent"))
                self.assertIsNone(read("abc"))
                self.assertEqual(read("abc"), b"clean")


class StagedScanTests(unittest.TestCase):
    """The hook's `--staged` mode and scripts/scan-secrets.sh against a real index."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        run("git", "init", "-q", "-b", "main", cwd=self.root)
        for key, value in (("user.name", "Test"), ("user.email", "test@example.org"), ("commit.gpgsign", "false")):
            run("git", "config", key, value, cwd=self.root)
        self.write("src/lib.rs", "fn main() {}\n" + DBG + "\n")
        run("git", "add", "--", ".", cwd=self.root)
        run("git", "commit", "--no-verify", "-qm", "test: base", cwd=self.root)

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        run("git", "add", "--", name, cwd=self.root)

    def scan(self, *args):
        return run(sys.executable, str(ROOT / "tools/policy/added_line_policy.py"), "--root", str(self.root),
                   *args, cwd=self.root, check=False)

    def test_clean_change_and_existing_dbg_pass(self):
        self.write("src/lib.rs", "fn main() {}\n" + DBG + "\nfn more() {}\n")
        self.write("vendor/up/src/lib.rs", DBG + "\n")
        self.write("notes.md", DBG + "\n")
        result = self.scan("--staged")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_each_rule_fails_the_staged_change(self):
        cases = {
            "keys.env": ("AWS=" + SAMPLES["AWS access key"] + "\n", "possible AWS access key"),
            "merge.md": ("text\n" + CONFLICT + "\n", "merge-conflict marker"),
            "src/new.rs": (DBG + "\n", "dbg!"),
        }
        for name, (text, expected) in cases.items():
            with self.subTest(name=name):
                self.write(name, text)
                result = self.scan("--staged")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(name + ":", result.stderr)
                self.assertIn(expected, result.stderr)
                self.assertIn("only exception", result.stderr)
                run("git", "rm", "-q", "--cached", "--", name, cwd=self.root)

    def test_newline_and_non_utf8_paths_are_scanned_and_keep_parent_lines(self):
        for name in ("src/line\nbreak.rs", "src/raw-\udcff.rs"):
            with self.subTest(name=repr(name)):
                self.write(name, DBG + "\n")
                result = self.scan("--staged")
                self.assertNotEqual(result.returncode, 0, result.stderr)
                self.assertIn("dbg!", result.stderr)
                run("git", "commit", "--no-verify", "-qm", "test: unusual path", cwd=self.root)
                self.write(name, DBG + "\nfn clean() {}\n")
                result = self.scan("--staged")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.write(name, DBG + "\nfn clean() {}\n" + CONFLICT + "\n")
                result = self.scan("--staged")
                self.assertNotEqual(result.returncode, 0, result.stderr)
                self.assertIn("merge-conflict marker", result.stderr)
                run("git", "commit", "--no-verify", "-qm", "test: changed unusual path", cwd=self.root)

    def test_scan_secrets_script_reports_only_credentials(self):
        script = ROOT / "scripts/scan-secrets.sh"
        self.write("src/new.rs", DBG + "\n" + CONFLICT + "\n")
        self.assertEqual(run("bash", str(script), cwd=self.root, check=False).returncode, 0)
        self.write("keys.env", SAMPLES["GitHub token"] + "\n")
        result = run("bash", str(script), cwd=self.root, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("keys.env:1: possible GitHub token", result.stderr)
        run("git", "commit", "--no-verify", "-qm", "test: commit token", cwd=self.root)
        self.assertEqual(run("bash", str(script), cwd=self.root, check=False).returncode, 0)
        self.assertNotEqual(run("bash", str(script), "--all", cwd=self.root, check=False).returncode, 0)


if __name__ == "__main__":
    unittest.main()

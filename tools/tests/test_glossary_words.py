import contextlib
import importlib.util
import io
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_glossary_words.py"
SPEC = importlib.util.spec_from_file_location("check_glossary_words", SCRIPT)
words = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = words
SPEC.loader.exec_module(words)

GLOSSARY = """# Glossary

## Not used

Old names: room, channel (except Chat's channels), host.

Vague metaphors: door, gate. Name the thing.

The check reads this list.
"""

ALLOWED = """
[[allow]]
reason = "Chat's channels."
words = ["channel"]
paths = ["apps/chat/*"]

[[allow]]
reason = "A test identifier."
words = ["host"]
identifiers = ["host_key"]
"""


class MatcherTests(unittest.TestCase):
    def test_reads_the_lists_without_their_exceptions_or_prose(self):
        self.assertEqual(words.not_used(GLOSSARY), ["room", "channel", "host", "door", "gate"])

    def test_finds_whole_words_and_identifier_parts(self):
        matcher = words.Matcher(["room", "gate"])
        found = lambda text: [word for _, _, word in matcher.find(text)]
        self.assertEqual(found("room_id RoomId ChatRoom ROOM_ID rooms"), ["room"] * 5)
        self.assertEqual(found("bathroom gateway navigate delegate GATEWAY Roomba"), [])
        self.assertEqual(found("gates GateKeeper"), ["gate", "gate"])

    def test_judges_only_the_key_of_a_toml_line(self):
        self.assertEqual(words.toml_key('room = "host"'), "room ")
        self.assertEqual(words.toml_key("[rooms.host]"), "[rooms.host]")
        self.assertEqual(words.toml_key('  "room door",'), "")


    def test_a_rule_naming_identifiers_and_paths_allows_them_only_together(self):
        allowed = words.Allowed(
            '[[allow]]\nreason = "r"\nwords = ["host"]\nidentifiers = ["host"]\npaths = ["a/*"]\n'
        )
        self.assertTrue(allowed("host", "a/x.rs", "host"))
        self.assertFalse(allowed("host", "b/x.rs", "host"))
        self.assertFalse(allowed("host", "a/x.rs", "host_key"))


class TreeTests(unittest.TestCase):
    def repository(self, baseline: str) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        files = {
            words.GLOSSARY: GLOSSARY,
            words.ALLOWED: ALLOWED,
            words.BASELINE: baseline,
            "apps/chat/src/lib.rs": "struct Channel;\n",
            "crates/a/src/lib.rs": "// The host and its host_key.\nfn open_room() {}\n",
            "dist/apple/Sources/A.swift": "let entry = 1\n",
            "tools/a.py": "entry = 1\n",
            "vendor/b/b.js": "const door = 1;\n",
            "web/c.min.js": "const door = 1;\n",
        }
        for name, text in files.items():
            (root / name).parent.mkdir(parents=True, exist_ok=True)
            (root / name).write_text(text)
        self.git(root, "init", "-q")
        self.git(root, "add", ".")
        self.git(root, "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false",
                 "commit", "-q", "-m", "init")
        self.root = root
        old = words.ROOT
        words.ROOT = root
        self.addCleanup(setattr, words, "ROOT", old)
        return root

    @staticmethod
    def git(root: Path, *args: str) -> None:
        subprocess.run(["git", *args], cwd=root, check=True, capture_output=True)

    def commit_unjudged(self, root: Path) -> None:
        """A use nothing judged: committed with no hook, as a commit made before
        its base held the check arrives once it is rebased onto it."""
        (root / "crates/b/src").mkdir(parents=True)
        (root / "crates/b/src/lib.rs").write_text("fn other_room() {}\n")
        self.git(root, "add", ".")
        self.git(root, "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false",
                 "commit", "-q", "-m", "unjudged")
        code, out = self.run_mode(words.judge_tree)
        self.assertEqual(code, 1)
        self.assertIn("'room': 2 uses, 1 allowed", out)

    def staged_baseline(self, root: Path) -> str:
        return subprocess.run(["git", "show", f":{words.BASELINE}"], cwd=root,
                              capture_output=True, text=True).stdout

    def run_mode(self, function) -> tuple[int, str]:
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            code = function()
        return code, err.getvalue()

    def test_counts_against_the_baseline_and_skips_what_is_allowed(self):
        self.repository(words.BASELINE_HEADER + "room 1\nhost 1\n")
        self.assertEqual(self.run_mode(words.judge_tree), (0, ""))
        (self.root / "crates/a/src/lib.rs").write_text("fn open_room() {}\nfn door() {}\n")
        code, out = self.run_mode(words.judge_tree)
        self.assertEqual(code, 1)
        self.assertIn("'door' is not used", out)
        self.assertIn("crates/a/src/lib.rs:2: fn door() {}", out)
        self.assertIn("'host': 0 uses, the baseline still allows 1", out)

    def test_a_commit_may_not_add_uses_and_lowers_the_count_it_removes(self):
        root = self.repository(words.BASELINE_HEADER + "room 1\nhost 1\n")
        (root / "crates/a/src/lib.rs").write_text("fn open_room() {}\nfn other_room() {}\n")
        self.git(root, "add", ".")
        code, out = self.run_mode(words.judge_staged)
        self.assertEqual(code, 1)
        self.assertIn("crates/a/src/lib.rs:2: fn other_room() {}", out)
        self.assertNotIn("open_room", out)

        (root / "crates/a/src/lib.rs").write_text("fn open_room() {}\n")
        self.git(root, "add", ".")
        self.assertEqual(self.run_mode(words.judge_staged)[0], 0)
        staged = subprocess.run(["git", "show", f":{words.BASELINE}"], cwd=root,
                                capture_output=True, text=True).stdout
        self.assertEqual(staged, words.BASELINE_HEADER + "room 1\n")

    def test_a_commit_that_removes_uses_writes_no_count_upward(self):
        root = self.repository(words.BASELINE_HEADER + "room 1\nhost 1\n")
        self.commit_unjudged(root)
        # Removing the one use of another word lowers that count and leaves the
        # unjudged use above its own, for the tree to keep naming.
        (root / "crates/a/src/lib.rs").write_text("fn open_room() {}\n")
        self.git(root, "add", ".")
        self.assertEqual(self.run_mode(words.judge_staged)[0], 0)
        self.assertEqual(self.staged_baseline(root), words.BASELINE_HEADER + "room 1\n")
        code, out = self.run_mode(words.judge_tree)
        self.assertEqual(code, 1)
        self.assertIn("'room': 2 uses, 1 allowed", out)

    def test_a_change_of_what_counts_raises_a_count_by_that_change_alone(self):
        root = self.repository(words.BASELINE_HEADER + "room 1\nhost 1\n")
        self.commit_unjudged(root)
        # `host_key` is no longer an allowed use, so it counts from this commit on;
        # the use nothing judged is not written into the baseline with it.
        (root / words.ALLOWED).write_text(ALLOWED.replace('"host_key"', '"other_key"'))
        self.git(root, "add", ".")
        self.assertEqual(self.run_mode(words.judge_staged)[0], 0)
        self.assertEqual(self.staged_baseline(root),
                         words.BASELINE_HEADER + "room 1\nhost 2\n")

    def test_a_staged_baseline_lowers_a_count_by_hand_and_raises_none(self):
        root = self.repository(words.BASELINE_HEADER + "room 1\nhost 1\n")
        self.commit_unjudged(root)
        (root / words.BASELINE).write_text(words.BASELINE_HEADER + "room 2\n")
        self.git(root, "add", ".")
        self.assertEqual(self.run_mode(words.judge_staged)[0], 0)
        self.assertEqual(self.staged_baseline(root), words.BASELINE_HEADER + "room 1\n")

    def test_judges_tilos_own_sources_in_every_language(self):
        root = self.repository(words.BASELINE_HEADER + "room 1\nhost 1\n")
        (root / "dist/apple/Sources/A.swift").write_text("let doorOffer = 1\n")
        (root / "tools/a.py").write_text("GATE = 1\n")
        code, out = self.run_mode(words.judge_tree)
        self.assertEqual(code, 1)
        self.assertIn("dist/apple/Sources/A.swift:1: let doorOffer = 1", out)
        self.assertIn("tools/a.py:1: GATE = 1", out)
        self.assertNotIn("vendor/", out)
        self.assertNotIn(".min.js", out)

    def test_the_tree_names_the_uses_added_since_the_baseline(self):
        root = self.repository(words.BASELINE_HEADER + "room 1\nhost 1\n")
        self.commit_unjudged(root)
        code, out = self.run_mode(words.judge_tree)
        self.assertEqual(code, 1)
        self.assertIn("crates/b/src/lib.rs:1: fn other_room() {}", out)
        self.assertNotIn("open_room", out)

    def test_widening_the_check_raises_a_count_by_what_it_newly_counts(self):
        root = self.repository(words.BASELINE_HEADER + "room 1\nhost 1\n")
        source = SCRIPT.read_text()
        (root / words.SCRIPT).parent.mkdir(parents=True, exist_ok=True)
        (root / words.SCRIPT).write_text(source.replace('"*.py", ', ""))
        (root / "tools/a.py").write_text("room = 1\n")
        self.git(root, "add", ".")
        self.git(root, "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false",
                 "commit", "-q", "-m", "a check that skips Python")
        (root / words.SCRIPT).write_text(source)
        self.git(root, "add", ".")
        self.assertEqual(self.run_mode(words.judge_staged)[0], 0)
        self.assertEqual(self.staged_baseline(root), words.BASELINE_HEADER + "room 2\nhost 1\n")


if __name__ == "__main__":
    unittest.main()

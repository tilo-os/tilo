import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from git_fixture import isolated_git_environment

TOOLS = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "check_format_migrations", TOOLS / "policy/check_format_migrations.py"
)
formats = importlib.util.module_from_spec(spec)
sys.modules["check_format_migrations"] = formats
spec.loader.exec_module(formats)


def declaration(oldest):
    return f'''
pub(crate) const STATE: tilo_files::native::migration::Format =
    tilo_files::native::migration::Format {{
        name: "desktop state",
        version: STATE_VERSION,
        oldest: {oldest},
        steps: &[one_to_two],
    }};
'''


class FormatMigrationTests(unittest.TestCase):
    def git(self, root, *args):
        subprocess.run(["git", *args], cwd=root, check=True, capture_output=True,
                       env=isolated_git_environment())

    def test_the_oldest_version_is_a_number(self):
        self.assertEqual(
            formats.declarations({"a.rs": declaration(1)}), {"desktop state": ("a.rs", "1")}
        )
        with tempfile.TemporaryDirectory() as root:
            path = Path(root, "crates/x/src/lib.rs")
            path.parent.mkdir(parents=True)
            path.write_text(declaration("FIRST"))
            self.assertEqual(len(formats.check(Path(root))), 1)
            path.write_text(declaration(1))
            self.assertEqual(formats.check(Path(root)), [])

    def test_the_chain_of_steps_only_grows(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            self.git(root, "init", "-q", "-b", "main")
            self.git(root, "config", "user.email", "t@example.com")
            self.git(root, "config", "user.name", "t")
            path = root / "crates/x/src/lib.rs"
            path.parent.mkdir(parents=True)
            path.write_text(declaration(1))
            self.git(root, "add", ".")
            self.git(root, "commit", "-qm", "base")
            self.assertEqual(formats.against("main", root), [])
            path.write_text(declaration(2))
            self.assertIn("no longer brings up version 1", formats.against("main", root)[0])
            path.write_text("")
            self.assertIn("no longer declared", formats.against("main", root)[0])

    def test_a_declaration_moved_to_a_new_file_is_found_there(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            self.git(root, "init", "-q", "-b", "main")
            self.git(root, "config", "user.email", "t@example.com")
            self.git(root, "config", "user.name", "t")
            old = root / "crates/x/src/lib.rs"
            old.parent.mkdir(parents=True)
            old.write_text(declaration(1))
            other = root / "crates/x/src/other.rs"
            other.write_text("// nothing\n")
            self.git(root, "add", ".")
            self.git(root, "commit", "-qm", "base")
            # Into a file git doesn't track yet.
            old.write_text("")
            new = root / "crates/x/src/state.rs"
            new.write_text(declaration(1))
            self.assertEqual(formats.against("main", root), [])
            new.write_text(declaration(2))
            self.assertIn("no longer brings up version 1", formats.against("main", root)[0])
            # Into a tracked file, committed.
            new.unlink()
            other.write_text(declaration(1))
            self.git(root, "commit", "-qam", "moved")
            self.assertEqual(formats.against("main~1", root), [])

    def test_the_repository_holds_to_it(self):
        self.assertEqual(formats.check(), [])
        self.assertIn("desktop state", formats.declarations(formats.sources(formats.ROOT)))


if __name__ == "__main__":
    unittest.main()

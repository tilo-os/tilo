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


GOLDEN = "golden/fixture-state/v2.hex"
READS_GOLDEN = f'#[test]\nfn kept() {{ assert_golden_bytes(STATE.name, include_str!("../{GOLDEN}"), r, w); }}\n'


def keep_golden(crate, reads=True):
    """The golden bytes of the fixture's version 2 in `crate`, and a test
    that reads them unless `reads` is false."""
    (crate / "Cargo.toml").write_text("[package]\n")
    (crate / GOLDEN).parent.mkdir(parents=True, exist_ok=True)
    (crate / GOLDEN).write_text("02 6b\n")
    if reads:
        (crate / "tests").mkdir(exist_ok=True)
        (crate / "tests/golden.rs").write_text(READS_GOLDEN)


def declaration(oldest):
    return f'''
pub(crate) const STATE_VERSION: u32 = 2;
pub(crate) const STATE: tilo_files::native::migration::Format =
    tilo_files::native::migration::Format {{
        name: "fixture state",
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
            formats.declarations({"a.rs": declaration(1)}), {"fixture state": ("a.rs", "1")}
        )
        with tempfile.TemporaryDirectory() as root:
            path = Path(root, "crates/x/src/lib.rs")
            path.parent.mkdir(parents=True)
            keep_golden(Path(root, "crates/x"))
            loader = "fn open(p: &Path, b: Vec<u8>, v: u32) { STATE.bring_up(p, b, v); }\n"
            path.write_text(declaration("FIRST") + loader)
            self.assertEqual(len(formats.check(Path(root))), 1)
            path.write_text(declaration(1) + loader)
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

    def loaded(self, files):
        """What `check` finds in a crate of `files` (path under src -> text)."""
        with tempfile.TemporaryDirectory() as root:
            crate = Path(root, "crates/x")
            (crate / "src").mkdir(parents=True)
            keep_golden(crate)
            for name, text in files.items():
                (crate / "src" / name).write_text(text)
            return formats.check(Path(root))

    def test_a_loader_brings_every_stored_version_up(self):
        reads = "fn open(p: &Path, b: Vec<u8>) { let stored = version(&b);\n" \
                "    let b = STATE\n        .bring_up(p, b, stored)?; }\n"
        self.assertEqual(self.loaded({"lib.rs": declaration(1) + reads}), [])
        # From another file of the crate, by the declaring module's name.
        self.assertEqual(
            self.loaded({
                "state.rs": declaration(1),
                "lib.rs": "mod state;\nfn open(p: &Path, b: Vec<u8>, v: u32) {\n"
                          "    state::STATE.bring_up(p, b, v); }\n",
            }),
            [],
        )
        # A test of the steps alone is no loader.
        tested = declaration(1) + "#[cfg(test)]\nmod tests {\n" \
            "    fn t() { STATE.bring_up(p, b, stored); } }\n"
        self.assertIn("passes the version it read", self.loaded({"lib.rs": tested})[0])

    def test_a_loader_that_assumes_one_version_or_keeps_nothing_fails(self):
        # The first version's file brought up by number, a later one read as
        # it is: what refused a relay's whole state.
        first_only = declaration(1) + "fn open(b: Vec<u8>) { STATE.migrate(b, 1); }\n"
        found = self.loaded({"lib.rs": first_only})
        self.assertEqual(len(found), 2, found)
        self.assertIn("`migrate`", found[0])
        self.assertIn("passes the version it read", found[1])
        literal = declaration(1) + "fn open(p: &Path, b: Vec<u8>) { STATE.bring_up(p, b, 1); }\n"
        self.assertIn("passes the version it read", self.loaded({"lib.rs": literal})[0])
        moved = declaration(1) + "fn open(o: &Path, p: &Path, b: Vec<u8>, v: u32) {\n" \
            "    STATE.bring_up_moved(o, p, b, 1); STATE.bring_up(p, b, v); }\n"
        self.assertEqual(self.loaded({"lib.rs": moved}), [])

    def test_each_version_keeps_the_bytes_it_wrote_and_a_test_reads_them(self):
        loader = "fn open(p: &Path, b: Vec<u8>, v: u32) { STATE.bring_up(p, b, v); }\n"
        with tempfile.TemporaryDirectory() as root:
            crate = Path(root, "crates/x")
            (crate / "src").mkdir(parents=True)
            (crate / "src/lib.rs").write_text(declaration(1) + loader)
            (crate / "Cargo.toml").write_text("[package]\n")
            found = formats.check(Path(root))
            self.assertEqual(len(found), 1, found)
            self.assertIn("keeps no golden bytes at crates/x/golden/fixture-state/v2.hex", found[0])
            keep_golden(crate, reads=False)
            self.assertIn("no test of its crate reads", formats.check(Path(root))[0])
            keep_golden(crate)
            self.assertEqual(formats.check(Path(root)), [])
            # A version that is no number of the crate cannot name its bytes.
            (crate / "src/lib.rs").write_text(
                declaration(1).replace("const STATE_VERSION: u32 = 2;", "") + loader
            )
            self.assertIn("write the number", formats.check(Path(root))[0])

    def test_golden_bytes_a_version_wrote_never_change(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            self.git(root, "init", "-q", "-b", "main")
            self.git(root, "config", "user.email", "t@example.com")
            self.git(root, "config", "user.name", "t")
            crate = root / "crates/x"
            (crate / "src").mkdir(parents=True)
            (crate / "src/lib.rs").write_text(declaration(1))
            keep_golden(crate)
            self.git(root, "add", ".")
            self.git(root, "commit", "-qm", "base")
            self.assertEqual(formats.against("main", root), [])
            # The layout changed and the bytes were written again under the
            # same version: what a device's certificate went through.
            (crate / GOLDEN).write_text("02 6f\n")
            found = formats.against("main", root)
            self.assertEqual(len(found), 1, found)
            self.assertIn("golden bytes changed", found[0])
            (crate / GOLDEN).unlink()
            self.assertIn("are gone", formats.against("main", root)[0])
            # A new version keeps its own bytes beside the old ones.
            (crate / GOLDEN).write_text("02 6b\n")
            (crate / "golden/fixture-state/v3.hex").write_text("03\n")
            self.assertEqual(formats.against("main", root), [])

    def test_the_repository_holds_to_it(self):
        self.assertEqual(formats.check(), [])
        declared = formats.declarations(formats.sources(formats.ROOT))
        # The type is named through an import as often as in full, so a
        # declaration written as `Format { .. }` is found too.
        self.assertIn("sign-in record", declared)
        self.assertIn("room wait list", declared)
        self.assertIn("device certificate", declared)


if __name__ == "__main__":
    unittest.main()

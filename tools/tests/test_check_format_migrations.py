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


def declaration(oldest, version=2):
    return f'''
pub(crate) const STATE_VERSION: u32 = {version};
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

    def test_a_format_may_bring_up_fewer_versions(self):
        # A layout change deletes the step that read the one before it, and a
        # format may go with its steps: neither is a regression.
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
            self.assertEqual(formats.against("main", root), [])
            path.write_text("")
            self.assertEqual(formats.against("main", root), [])

    def test_a_format_brings_up_the_version_main_writes(self):
        # Every device that updates holds what main wrote: a new layout that
        # refuses it leaves the device without what it kept.
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            self.git(root, "init", "-q", "-b", "main")
            self.git(root, "config", "user.email", "t@example.com")
            self.git(root, "config", "user.name", "t")
            path = root / "crates/x/src/lib.rs"
            path.parent.mkdir(parents=True)
            path.write_text(declaration(2))
            self.git(root, "add", ".")
            self.git(root, "commit", "-qm", "base")
            path.write_text(declaration(3, version=3))
            found = formats.against("main", root)
            self.assertEqual(len(found), 1, found)
            self.assertIn("no longer brings up version 2", found[0])
            path.write_text(declaration(2, version=3))
            self.assertEqual(formats.against("main", root), [])

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
        # it is: what refused a server's whole state.
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
            # A version this build still declares keeps its bytes: a missing
            # file is no way to change them.
            (crate / GOLDEN).unlink()
            self.assertIn("are gone", formats.against("main", root)[0])
            # A layout change is a new version: its own bytes, and the one
            # before it goes with its step.
            (crate / "src/lib.rs").write_text(
                declaration(2).replace("const STATE_VERSION: u32 = 2;", "const STATE_VERSION: u32 = 3;")
            )
            (crate / "golden/fixture-state/v3.hex").write_text("03\n")
            self.assertEqual(formats.against("main", root), [])

    def test_a_type_stored_beside_a_format_changes_only_with_its_version(self):
        loader = "fn open(p: &Path, b: Vec<u8>, v: u32) { STATE.bring_up(p, b, v); }\n"
        entry = "#[derive(Serialize, Deserialize)]\nstruct Entry {\n    revision: u64,\n    fields: Vec<u8>,\n}\n"
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            crate = root / "crates/x"
            (crate / "src").mkdir(parents=True)
            (crate / "src/lib.rs").write_text(declaration(1) + loader)
            (crate / "src/journal.rs").write_text(entry)
            keep_golden(crate)
            (root / "tools/policy").mkdir(parents=True)
            layouts = root / "tools/policy/stored_layouts.toml"
            digest, missing = formats.layout_digest(
                ["crates/x/src/journal.rs:Entry"], formats.definitions_for(root)
            )
            self.assertEqual(missing, [])
            layouts.write_text(
                f'[[layout]]\nformat = "fixture state"\nversion = 2\ndigest = "{digest}"\n'
                'what = "a journal"\ntypes = ["crates/x/src/journal.rs:Entry"]\n'
            )
            self.assertEqual(formats.check(root), [])
            # Comments and doc text are no layout.
            (crate / "src/journal.rs").write_text("/// The entry.\n" + entry.replace("u64,", "u64, // its revision"))
            self.assertEqual(formats.check(root), [])
            # A field added in the middle, as a device's journal took one: the
            # bytes an earlier build wrote read as something else.
            (crate / "src/journal.rs").write_text(entry.replace("u64,", "u64,\n    unanswered: Vec<u8>,"))
            found = formats.check(root)
            self.assertEqual(len(found), 1, found)
            self.assertIn("changed while the format is still version 2", found[0])
            # A new version of the format, pinned, is the change.
            (crate / "src/lib.rs").write_text(
                declaration(2).replace("const STATE_VERSION: u32 = 2;", "const STATE_VERSION: u32 = 3;")
                + loader
            )
            (crate / "golden/fixture-state/v3.hex").write_text("03\n")
            (crate / "tests/golden3.rs").write_text(READS_GOLDEN.replace("v2.hex", "v3.hex"))
            self.assertIn("pinned only up to version 2", formats.check(root)[0])
            self.assertEqual(formats.pin(root), [])
            self.assertEqual(formats.check(root), [])
            self.assertIn("version = 3", layouts.read_text())

    def test_a_stored_type_moved_to_another_crate_keeps_its_digest(self):
        entry = "#[derive(Serialize, Deserialize)]\nstruct Entry {\n    revision: u64,\n}\n"
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            for crate in ("crates/collab/x", "crates/core/x"):
                (root / crate / "src").mkdir(parents=True)
                (root / crate / "src/journal.rs").write_text(entry)
            before, _ = formats.layout_digest(
                ["crates/collab/x/src/journal.rs:Entry"], formats.definitions_for(root)
            )
            after, _ = formats.layout_digest(
                ["crates/core/x/src/journal.rs:Entry"], formats.definitions_for(root)
            )
            self.assertIsNotNone(before)
            self.assertEqual(before, after)

    def test_a_stored_layout_a_version_kept_never_changes(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            self.git(root, "init", "-q", "-b", "main")
            self.git(root, "config", "user.email", "t@example.com")
            self.git(root, "config", "user.name", "t")
            crate = root / "crates/x"
            (crate / "src").mkdir(parents=True)
            (crate / "src/lib.rs").write_text(declaration(1))
            keep_golden(crate)
            (root / "tools/policy").mkdir(parents=True)
            layouts = root / "tools/policy/stored_layouts.toml"
            listed = '[[layout]]\nformat = "fixture state"\nversion = 2\ndigest = "{}"\nwhat = "a journal"\ntypes = []\n'
            layouts.write_text(listed.format("aaaa"))
            self.git(root, "add", ".")
            self.git(root, "commit", "-qm", "base")
            self.assertEqual(formats.against("main", root), [])
            layouts.write_text(listed.format("bbbb"))
            found = formats.against("main", root)
            self.assertEqual(len(found), 1, found)
            self.assertIn("version 2 stored changed", found[0])

    def stored(self, files, layouts):
        """A tree of `files` (path under crates/x/src -> text), with a
        declaration that passes the rest of the check, and `layouts` written
        as `tools/policy/stored_layouts.toml`."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        crate = root / "crates/x"
        (crate / "src").mkdir(parents=True)
        keep_golden(crate)
        (crate / "src/lib.rs").write_text(
            declaration(1) + "fn open(p: &Path, b: Vec<u8>, v: u32) { STATE.bring_up(p, b, v); }\n"
        )
        for name, text in files.items():
            (crate / "src" / name).write_text(text)
        (root / "tools/policy").mkdir(parents=True)
        (root / "tools/policy/stored_layouts.toml").write_text(layouts)
        return root

    def pinned(self, root, entry, format="fixture state", version=2):
        """`entry` listed under `format` at `version`, with its digest."""
        digest, missing = formats.layout_digest([entry], formats.definitions_for(root))
        self.assertEqual(missing, [])
        return (
            f'[[layout]]\nformat = "{format}"\nversion = {version}\ndigest = "{digest}"\n'
            f'what = "a fixture"\ntypes = ["{entry}"]\n'
        )

    def test_a_derivation_and_its_domain_constant_change_only_with_a_version(self):
        # A derived id is stored in every sealed document: a change to how it
        # is derived reads as another document's key. Neither its function nor
        # the domain it is derived with was a type the check could list.
        derive = (
            'pub const DOMAIN: &[u8] = b"tilo-fixture-v1\\0";\n\n'
            "pub fn artifact(document: &str) -> String {\n"
            "    let mut out = String::from_utf8_lossy(DOMAIN).into_owned();\n"
            "    out.push_str(document);\n    out\n}\n"
        )
        root = self.stored({"derive.rs": derive}, "")
        layouts = root / "tools/policy/stored_layouts.toml"
        listed = [
            "crates/x/src/derive.rs:artifact",
            "crates/x/src/derive.rs:DOMAIN",
        ]
        digest, missing = formats.layout_digest(listed, formats.definitions_for(root))
        self.assertEqual(missing, [])
        types = "".join(f'  "{entry}",\n' for entry in listed)
        layouts.write_text(
            '[[layout]]\nformat = "fixture state"\nversion = 2\n'
            f'digest = "{digest}"\nwhat = "a fixture"\ntypes = [\n{types}]\n'
        )
        self.assertEqual(formats.check(root), [])
        # The domain's bytes are part of every derived id.
        (root / "crates/x/src/derive.rs").write_text(derive.replace("v1", "v2"))
        found = formats.check(root)
        self.assertEqual(len(found), 1, found)
        self.assertIn("still version 2", found[0])
        self.assertIn("derive.rs:artifact", found[0])
        # And so is the body that derives it.
        (root / "crates/x/src/derive.rs").write_text(derive.replace("out\n}", "out.clone()\n}"))
        found = formats.check(root)
        self.assertEqual(len(found), 1, found)
        self.assertIn("still version 2", found[0])

    def test_a_format_versioned_by_a_constant_needs_a_step_to_move(self):
        # The durable state names its version in a plain constant and refuses
        # any other: a layout change is a new constant, with a step that reads
        # what the old one wrote.
        state = (
            "pub const DURABLE_STATE_VERSION: u32 = 2;\n\n"
            "#[derive(serde::Serialize, serde::Deserialize)]\n"
            "pub struct State {\n    pub revision: u64,\n}\n"
        )
        layouts = (
            '[[format]]\nname = "fixture durable"\n'
            'version = "crates/x/src/state.rs:DURABLE_STATE_VERSION"\n\n'
        )
        root = self.stored({"state.rs": state}, layouts)
        file = root / "tools/policy/stored_layouts.toml"
        digest, missing = formats.layout_digest(
            ["crates/x/src/state.rs:State"], formats.definitions_for(root)
        )
        self.assertEqual(missing, [])
        file.write_text(
            layouts
            + '[[layout]]\nformat = "fixture durable"\nversion = 2\n'
            f'digest = "{digest}"\nwhat = "a durable state"\n'
            'types = ["crates/x/src/state.rs:State"]\n'
        )
        self.assertEqual(formats.check(root), [])
        (root / "crates/x/src/state.rs").write_text(
            state.replace("pub revision: u64,", "pub revision: u64,\n    pub pending: Vec<u8>,")
        )
        found = formats.check(root)
        self.assertEqual(len(found), 1, found)
        self.assertIn("refuses a state of another version", found[0])
        self.assertIn("step that brings up", found[0])
        # Moving the constant is the version change, and its layout is pinned.
        (root / "crates/x/src/state.rs").write_text(
            state.replace("DURABLE_STATE_VERSION: u32 = 2", "DURABLE_STATE_VERSION: u32 = 3")
        )
        (root / "crates/x/golden/fixture-state/v3.hex").write_text("03\n")
        (root / "crates/x/tests/golden3.rs").write_text(
            READS_GOLDEN.replace("v2.hex", "v3.hex")
        )
        found = formats.check(root)
        self.assertTrue(any("pinned only up to version 2" in p for p in found), found)
        self.assertEqual(formats.pin(root), [])
        self.assertEqual(formats.check(root), [])
        # A constant that is not there names no version.
        (root / "tools/policy/stored_layouts.toml").write_text(
            layouts.replace("STATE_VERSION", "MISSING_VERSION")
        )
        self.assertIn("declares `version", formats.check(root)[0])

    def test_a_format_with_no_version_fails_on_any_change(self):
        # Frames a store writes without a header: a later build cannot tell
        # one layout from another, so any change is a change of meaning.
        frames = (
            "#[derive(serde::Serialize, serde::Deserialize)]\n"
            "pub enum Frames {\n    One(u64),\n    Two,\n}\n"
        )
        root = self.stored({"frame.rs": frames}, "")
        file = root / "tools/policy/stored_layouts.toml"
        digest, missing = formats.layout_digest(
            ["crates/x/src/frame.rs:Frames"], formats.definitions_for(root)
        )
        self.assertEqual(missing, [])
        file.write_text(
            '[[format]]\nname = "fixture frames"\nversion = "none"\n\n'
            '[[layout]]\nformat = "fixture frames"\n'
            f'digest = "{digest}"\nwhat = "frames"\n'
            'types = ["crates/x/src/frame.rs:Frames"]\n'
        )
        self.assertEqual(formats.check(root), [])
        (root / "crates/x/src/frame.rs").write_text(frames.replace("One(u64)", "One(u64, u8)"))
        found = formats.check(root)
        self.assertEqual(len(found), 1, found)
        self.assertIn("has no version", found[0])
        self.assertIn("Give the format a version", found[0])
        self.assertIn("step that brings up", found[0])

    def test_a_listed_type_covers_the_types_it_names(self):
        # A field added inside a type the listed one names reads the bytes an
        # earlier build wrote as something else, as much as one in the root.
        root = self.stored(
            {
                "types.rs": "pub struct Root {\n    pub inner: Inner,\n}\n",
                "inner.rs": "pub struct Inner {\n    pub count: u64,\n}\n",
            },
            "",
        )
        file = root / "tools/policy/stored_layouts.toml"
        file.write_text(self.pinned(root, "crates/x/src/types.rs:Root"))
        self.assertEqual(formats.check(root), [])
        (root / "crates/x/src/inner.rs").write_text(
            "pub struct Inner {\n    pub count: u64,\n    pub other: u8,\n}\n"
        )
        found = formats.check(root)
        self.assertEqual(len(found), 1, found)
        self.assertIn("still version 2", found[0])
        self.assertIn("types.rs:Root", found[0])

    def test_a_name_is_resolved_in_its_crate_first(self):
        # Two crates may name a type; the one in the referencing file's crate
        # is the one it means, and the other is no part of its layout.
        root = self.stored(
            {
                "types.rs": "pub struct Root {\n    pub inner: Inner,\n}\n",
                "inner.rs": "pub struct Inner {\n    pub count: u64,\n}\n",
            },
            "",
        )
        other = root / "crates/y/src"
        other.mkdir(parents=True)
        (other / "inner.rs").write_text("pub struct Inner {\n    pub elsewhere: u8,\n}\n")
        file = root / "tools/policy/stored_layouts.toml"
        file.write_text(self.pinned(root, "crates/x/src/types.rs:Root"))
        self.assertEqual(formats.check(root), [])
        (other / "inner.rs").write_text("pub struct Inner {\n    pub elsewhere: u16,\n}\n")
        self.assertEqual(formats.check(root), [])

    def test_an_ambiguous_name_reaches_no_definition(self):
        # A name two crates use is no type the root means; a field added to
        # either is not a change of the root's bytes.
        root = self.stored(
            {"types.rs": "pub struct Root {\n    pub inner: Inner,\n}\n"}, ""
        )
        for crate in ("crates/y", "crates/z"):
            (root / crate / "src").mkdir(parents=True)
            (root / crate / "src/inner.rs").write_text(
                "pub struct Inner {\n    pub elsewhere: u8,\n}\n"
            )
        file = root / "tools/policy/stored_layouts.toml"
        file.write_text(self.pinned(root, "crates/x/src/types.rs:Root"))
        self.assertEqual(formats.check(root), [])
        (root / "crates/y/src/inner.rs").write_text(
            "pub struct Inner {\n    pub elsewhere: u16,\n}\n"
        )
        self.assertEqual(formats.check(root), [])



if __name__ == "__main__":
    unittest.main()

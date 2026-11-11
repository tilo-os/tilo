import importlib.util
import subprocess
import sys
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path

from git_fixture import isolated_git_environment

TOOLS = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "check_wire_enums", TOOLS / "policy/check_wire_enums.py"
)
wire = importlib.util.module_from_spec(spec)
sys.modules["check_wire_enums"] = wire
spec.loader.exec_module(wire)


def lifetime(*variants, serde=""):
    body = "\n".join(f"    /// One.\n    {variant}," for variant in variants)
    attribute = f"#[serde({serde})]\n" if serde else ""
    return (
        "#[derive(Clone, Debug, Serialize, Deserialize)]\n"
        f"{attribute}pub enum Lifetime {{\n{body}\n}}\n"
    )


class WireEnumTests(unittest.TestCase):
    def git(self, root, *args):
        subprocess.run(["git", *args], cwd=root, check=True, capture_output=True,
                       env=isolated_git_environment())

    @contextmanager
    def repository(self, before, after, breaks=None):
        """A repository whose main has `before`; `after` is the working tree's."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.git(root, "init", "-q", "-b", "main")
            self.git(root, "config", "user.email", "t@example.com")
            self.git(root, "config", "user.name", "t")
            path = root / "crates/x/src/lib.rs"
            path.parent.mkdir(parents=True)
            path.write_text(before)
            self.git(root, "add", ".")
            self.git(root, "commit", "-qm", "base")
            path.write_text(after)
            if breaks:
                listed = root / "tools/policy/wire_enum_breaks.toml"
                listed.parent.mkdir(parents=True)
                listed.write_text(breaks)
            yield root

    def judged(self, before, after, breaks=None):
        with self.repository(before, after, breaks) as root:
            return wire.against("main", root)

    def staged_change(self, before, after, breaks=None):
        with self.repository(before, after, breaks) as root:
            self.git(root, "add", "-A")
            return wire.staged(root)

    def test_variants_and_their_fields_are_read(self):
        found = wire.wire_enums(
            "#[derive(Serialize)]\n#[serde(rename_all = \"snake_case\")]\npub enum E {\n"
            "    #[serde(rename = \"a\")]\n    A { x: Vec<(u8, u8)> },\n    B(String),\n"
            "    /// C, then D.\n    C,\n}\nenum Plain { X }\n"
        )
        self.assertEqual(list(found), ["E"])
        self.assertEqual(found["E"].variants, ("A", "B", "C"))

    def test_a_variant_added_at_the_end_is_fine(self):
        self.assertEqual(
            self.judged(lifetime("Once", "Task"), lifetime("Once", "Task", "UntilRevoked")), []
        )

    def test_a_removed_variant_fails(self):
        problems = self.judged(lifetime("Once", "Task", "Until"), lifetime("Once", "Until"))
        self.assertEqual(len(problems), 1)
        self.assertIn("no longer has Task", problems[0])

    def test_a_variant_the_base_gained_after_head_branched_is_no_removal(self):
        with self.repository(lifetime("Once"), lifetime("Once")) as root:
            self.git(root, "checkout", "-q", "-b", "behind")
            self.git(root, "checkout", "-q", "main")
            (root / "crates/x/src/lib.rs").write_text(lifetime("Once", "Task"))
            self.git(root, "commit", "-qam", "gains Task")
            self.git(root, "checkout", "-q", "behind")
            self.assertEqual(wire.against("main", root), [])

    def test_a_variant_moved_or_inserted_before_others_fails(self):
        self.assertEqual(
            len(self.judged(lifetime("Once", "Until"), lifetime("Until", "Once"))), 1
        )
        self.assertEqual(
            len(self.judged(lifetime("Once", "Until"), lifetime("Once", "Session", "Until"))), 1
        )

    def test_name_based_encodings_may_reorder_but_not_remove(self):
        tagged = 'tag = "kind"'
        self.assertEqual(
            self.judged(lifetime("Once", "Until", serde=tagged),
                        lifetime("Until", "Once", serde=tagged)),
            [],
        )
        self.assertEqual(
            len(self.judged(lifetime("Once", "Until", serde=tagged),
                            lifetime("Until", serde=tagged))),
            1,
        )

    def test_a_variant_retired_in_place_keeps_its_position(self):
        self.assertEqual(
            self.judged(lifetime("Once", "Task", "Until"),
                        lifetime("Once", "RetiredTask", "Until")),
            [],
        )

    def test_an_enum_read_and_written_through_another_type_is_judged_by_it(self):
        both = 'from = "Stored", into = "Stored"'
        self.assertEqual(
            self.judged(lifetime("Once", "Task", "Until"),
                        lifetime("Once", "Until", serde=both)),
            [],
        )
        # Reading through another type still writes the enum's own positions.
        self.assertEqual(
            len(self.judged(lifetime("Once", "Task", "Until"),
                            lifetime("Once", "Until", serde='from = "Stored"'))),
            1,
        )

    def test_a_break_named_with_its_reason_passes(self):
        breaks = (
            '[[break]]\nfile = "crates/x/src/lib.rs"\nenum = "Lifetime"\n'
            'reason = "never stored"\n'
        )
        self.assertEqual(
            self.judged(lifetime("Once", "Task"), lifetime("Once"), breaks), []
        )

    def test_a_removal_staged_fails(self):
        problems = self.staged_change(
            lifetime("Once", "Task", "Until"), lifetime("Once", "Until")
        )
        self.assertEqual(len(problems), 1)
        self.assertIn("no longer has Task", problems[0])

    def test_an_addition_staged_passes(self):
        self.assertEqual(
            self.staged_change(lifetime("Once"), lifetime("Once", "Until")), []
        )

    def test_only_the_index_is_judged(self):
        with self.repository(lifetime("Once", "Task"), lifetime("Once")) as root:
            self.assertEqual(wire.staged(root), [])

    def test_a_file_renamed_within_its_crate_keeps_its_enums(self):
        with self.repository(lifetime("Once", "Task"), lifetime("Once", "Task")) as root:
            self.git(root, "mv", "crates/x/src/lib.rs", "crates/x/src/lifetime.rs")
            (root / "crates/x/src/lib.rs").write_text("mod lifetime;\n")
            self.git(root, "add", "-A")
            self.assertEqual(wire.staged(root), [])


if __name__ == "__main__":
    unittest.main()

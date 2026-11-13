import importlib.util
import sys
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path

TOOLS = Path(__file__).resolve().parents[1]
POLICY = TOOLS / "policy"
sys.path.insert(0, str(POLICY))
from check_interfaces import InterfaceError, load_spec  # noqa: E402

spec = importlib.util.spec_from_file_location(
    "check_wire_attributes", POLICY / "check_wire_attributes.py"
)
attributes = importlib.util.module_from_spec(spec)
sys.modules["check_wire_attributes"] = attributes
spec.loader.exec_module(attributes)

INTERFACE = '''name = "demo"
version = 1
version_const = "crates/demo/wire/src/lib.rs:DEMO"
encoding = "{encoding}"
sources = ["crates/demo/wire/src/lib.rs"]
roots = [{roots}]
{extra}
[opaque]

[schema]
'''

SOURCE = '''#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {{
    pub id: String,
    {attribute}
    pub name: String,
}}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {{
    pub rows: Vec<Row>,
}}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loose {{
    pub id: String,
    {attribute}
    pub name: String,
}}
'''


def row(attribute):
    """The fixture's source with `attribute` written above the row's `name`."""
    return SOURCE.format(attribute=attribute)


class WireAttributeTests(unittest.TestCase):
    def tree(self, source, encoding="postcard", roots='"Frame"', extra=""):
        """A tree whose one postcard interface carries the fixture's types."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        source_path = root / "crates/demo/wire/src/lib.rs"
        source_path.parent.mkdir(parents=True)
        source_path.write_text(source)
        manifest = root / "crates/demo/wire/interface.toml"
        manifest.write_text(
            INTERFACE.format(encoding=encoding, roots=roots, extra=extra)
        )
        return root

    def judged(self, source, **kwargs):
        return attributes.problems(self.tree(source, **kwargs))

    def test_each_way_to_leave_a_field_out_of_a_carried_type_is_refused(self):
        for attribute, named in (
            ('#[serde(default, skip_serializing_if = "Option::is_none")]', "skip_serializing_if"),
            ("#[serde(skip)]", "skip"),
            ("#[serde(skip_serializing)]", "skip_serializing"),
            ("#[serde(skip_deserializing)]", "skip_deserializing"),
            ('#[serde(flatten)]', "flatten"),
        ):
            with self.subTest(attribute=attribute):
                found = self.judged(row(attribute))
                self.assertEqual(len(found), 1, found)
                self.assertIn("Row.name", found[0])
                self.assertIn(named, found[0])

    def test_a_serde_default_changes_nothing_that_is_written(self):
        self.assertEqual(self.judged(row("#[serde(default)]")), [])
        self.assertEqual(
            self.judged(row('#[serde(default, rename = "label", with = "crate::text")]')), []
        )

    def test_a_carried_enum_is_not_told_where_its_variant_is(self):
        for attribute in ("#[serde(untagged)]", '#[serde(tag = "kind")]', '#[serde(content = "value", tag = "kind")]'):
            with self.subTest(attribute=attribute):
                source = (
                    "#[derive(Clone, Serialize, Deserialize)]\n"
                    f"{attribute}\n"
                    "pub enum Choice {\n    One,\n    Two { name: String },\n}\n"
                    + SOURCE.format(attribute="#[serde(default)]").replace(
                        "pub struct Frame {\n    pub rows: Vec<Row>,\n}",
                        "pub struct Frame {\n    pub choice: Choice,\n}",
                    )
                )
                found = self.judged(source)
                self.assertEqual(len(found), 1, found)
                self.assertIn("Choice", found[0])

    def test_a_row_a_frame_carries_whole_is_refused_when_it_is_named(self):
        """The collaboration's own roster: a row a frame carries is judged by name."""
        source = '''#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<Vec<u8>>,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    pub people: Vec<Person>,
}
'''
        found = self.judged(source)
        self.assertEqual(len(found), 1, found)
        self.assertIn("Person.avatar", found[0])

    def test_a_type_the_frames_do_not_carry_is_left_alone(self):
        """`Loose` shares the file with the carried `Row` and is no field of a frame."""
        found = self.judged(row('#[serde(skip_serializing_if = "Option::is_none")]'))
        self.assertEqual(len(found), 1, found)
        self.assertNotIn("Loose", found[0])

    def test_a_json_message_is_left_alone(self):
        skipped = row('#[serde(skip_serializing_if = "Option::is_none")]')
        self.assertEqual(self.judged(skipped, encoding="json"), [])

    def test_the_json_roots_of_a_compact_interface_are_left_alone(self):
        source = SOURCE.format(attribute="#[serde(default)]") + '''
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum Settings {
    Reads,
    Writes { name: String },
}
'''
        roots = '"Frame", "Settings"'
        self.assertEqual(
            self.judged(source, roots=roots, extra='json_roots = ["Settings"]'), []
        )
        self.assertNotEqual(self.judged(source, roots=roots), [])

    def test_json_roots_must_be_roots(self):
        root = self.tree(SOURCE.format(attribute="#[serde(default)]"), extra='json_roots = ["Settings"]')
        unreadable = []
        self.assertEqual(attributes.problems(root, unreadable), [])
        self.assertEqual(len(unreadable), 1, unreadable)
        self.assertIn("json_roots", unreadable[0])

    def test_an_unreadable_declaration_is_named_and_its_wire_left_out(self):
        root = self.tree(row('#[serde(skip_serializing_if = "Option::is_none")]'), roots='"Nowhere"')
        unreadable = []
        self.assertEqual(attributes.problems(root, unreadable), [])
        self.assertEqual(len(unreadable), 1, unreadable)
        self.assertIn("Nowhere", unreadable[0])


class SpecTests(unittest.TestCase):
    def manifest(self, text):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        path = Path(temporary.name) / "interface.toml"
        path.write_text(text)
        return path

    def test_json_roots_are_read(self):
        directory = Path(self.manifest(INTERFACE.format(
            encoding="postcard", roots='"Frame", "Settings"', extra='json_roots = ["Settings"]'
        )).parent)
        spec, _ = load_spec(directory / "interface.toml")
        self.assertEqual(spec.json_roots, ["Settings"])
        self.assertEqual(replace(spec, roots=spec.json_roots).roots, ["Settings"])

    def test_a_json_root_that_is_no_root_is_refused(self):
        path = self.manifest(INTERFACE.format(
            encoding="postcard", roots='"Frame"', extra='json_roots = ["Settings"]'
        ))
        with self.assertRaises(InterfaceError):
            load_spec(path)


if __name__ == "__main__":
    unittest.main()

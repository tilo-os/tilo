import importlib.util
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

from git_fixture import isolated_git_environment

TOOLS = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "check_interfaces", TOOLS / "policy/check_interfaces.py"
)
interfaces = importlib.util.module_from_spec(spec)
sys.modules["check_interfaces"] = interfaces
spec.loader.exec_module(interfaces)

SOURCE = '''
//! Wire types.
use serde::{Deserialize, Serialize};

pub const PROTOCOL: u32 = 3;

/// A message, with a `}` in its doc comment.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Hello { version: u32, #[serde(default)] name: Option<String> },
    Frame(Frame),
    #[serde(rename = "bye")]
    GoodBye,
    #[serde(skip)]
    Internal(u8),
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct Frame {
    pub payload_bytes: Vec<u8>,
    #[serde(with = "serde_bytes")]
    signature: [u8; 64],
    kind: crate::kinds::Kind<'static>,
}

pub type Kind<'a> = Shape;


impl Frame {
    pub fn new() -> Self { todo!("{ not a type }") }
}

pub const fn ahead(theirs: u32) -> bool { theirs > PROTOCOL }

#[derive(Serialize, Deserialize)]
pub enum Shape { Round, SemanticV1 }

#[cfg(test)]
mod tests {
    pub struct Frame { old: u8 }
}
'''


def schema_from(source, roots=("Message",), encoding="json", opaque=None):
    with tempfile.TemporaryDirectory() as root:
        root = Path(root)
        (root / "wire.rs").write_text(source)
        spec = interfaces.Spec(
            name="test", version=3, version_const="wire.rs:PROTOCOL", encoding=encoding,
            sources=["wire.rs"], roots=list(roots), opaque=opaque or {},
            serves=False, path=root / "interface.toml",
        )
        return interfaces.schema_of(spec, root), interfaces.code_version(spec, root)


class SchemaTests(unittest.TestCase):
    def test_the_schema_is_what_the_wire_carries(self):
        schema, version = schema_from(SOURCE)
        self.assertEqual(version, 3)
        self.assertEqual(schema, {
            "Frame": "struct deny_unknown_fields",
            "Frame.kind": "Kind",
            "Frame.payload-bytes": "Vec<u8>",
            "Frame.signature": "[u8; 64] with=serde_bytes",
            "Kind": "alias Shape",
            "Message": "enum internal tag=type",
            "Message::bye": "unit",
            "Message::frame": "tuple(Frame)",
            "Message::hello": "struct",
            "Message::hello.name": "Option<String> optional",
            "Message::hello.version": "u32",
            "Shape": "enum external",
            "Shape::Round": "unit",
            "Shape::SemanticV1": "unit",
        })

    def test_postcard_records_the_order_that_is_its_encoding(self):
        schema, _ = schema_from(SOURCE, encoding="postcard")
        self.assertEqual(schema["Message::frame"], "#1 tuple(Frame)")
        self.assertEqual(schema["Frame.signature"], "#1 [u8; 64] with=serde_bytes")
        self.assertNotIn("optional", schema["Message::hello.name"])

    def test_a_type_nothing_defines_must_be_named_opaque(self):
        source = "pub const PROTOCOL: u32 = 1;\npub struct Root { at: chrono::DateTime<Utc> }"
        with self.assertRaisesRegex(interfaces.InterfaceError, "`DateTime`.*`Utc`"):
            schema_from(source, roots=("Root",))
        schema, _ = schema_from(
            source, roots=("Root",), opaque={"DateTime": "a time", "Utc": "its zone"}
        )
        self.assertEqual(schema["Root.at"], "DateTime<Utc>")

    def test_a_file_listed_twice_is_named_as_listed_twice(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "interface.toml"
            path.write_text(textwrap.dedent('''
                name = "test"
                version = 1
                version_const = "wire.rs:PROTOCOL"
                sources = ["wire.rs", "other.rs", "wire.rs"]
                roots = ["Message"]
            '''))
            with self.assertRaisesRegex(
                interfaces.InterfaceError, r"`sources` lists wire\.rs more than once"
            ):
                interfaces.load_spec(path)

    def test_serde_renames_like_serde(self):
        self.assertEqual(interfaces.renamed("MediaPositionV1", "kebab-case", variant=True),
                         "media-position-v1")
        self.assertEqual(interfaces.renamed("HTTPServer", "snake_case", variant=True),
                         "h_t_t_p_server")
        self.assertEqual(interfaces.renamed("splash_budget", "kebab-case"), "splash-budget")
        self.assertEqual(interfaces.renamed("splash_budget", "camelCase"), "splashBudget")


class CompatibilityTests(unittest.TestCase):
    BEFORE = {
        "S": "struct",
        "S.a": "u32",
        "E": "enum internal tag=type",
        "E::one": "unit",
    }

    def test_an_added_optional_field_is_compatible_in_json(self):
        now = dict(self.BEFORE, **{"S.b": "Option<u8> optional"})
        self.assertEqual(interfaces.incompatible(self.BEFORE, now, "json"), [])

    def test_a_new_type_is_judged_where_it_is_used(self):
        now = dict(self.BEFORE, **{"S.b": "Option<T> optional", "T": "struct", "T.x": "u8"})
        self.assertEqual(interfaces.incompatible(self.BEFORE, now, "json"), [])

    def test_every_other_change_breaks(self):
        cases = {
            "required field": dict(self.BEFORE, **{"S.b": "u8"}),
            "new message": dict(self.BEFORE, **{"E::two": "unit"}),
            "type changed": dict(self.BEFORE, **{"S.a": "u64"}),
            "field gone": {k: v for k, v in self.BEFORE.items() if k != "S.a"},
        }
        for case, now in cases.items():
            with self.subTest(case):
                self.assertTrue(interfaces.incompatible(self.BEFORE, now, "json"))
        strict = dict(self.BEFORE, **{"S": "struct deny_unknown_fields"})
        self.assertTrue(interfaces.incompatible(
            strict, dict(strict, **{"S.b": "Option<u8> optional"}), "json"))
        self.assertTrue(interfaces.incompatible(
            self.BEFORE, dict(self.BEFORE, **{"S.b": "#1 Option<u8>"}), "postcard"))

    def test_a_postcard_field_under_another_name_keeps_the_wire(self):
        before = {"S": "struct", "S.old": "#0 u32", "S.kept": "#1 Uuid"}
        renamed = {"S": "struct", "S.new": "#0 u32", "S.kept": "#1 Uuid"}
        self.assertEqual(interfaces.incompatible(before, renamed, "postcard"), [])
        self.assertTrue(interfaces.incompatible(before, renamed, "json"))
        moved = {"S": "struct", "S.new": "#1 u32", "S.kept": "#0 Uuid"}
        self.assertTrue(interfaces.incompatible(before, moved, "postcard"))
        retyped = {"S": "struct", "S.new": "#0 u64", "S.kept": "#1 Uuid"}
        self.assertTrue(interfaces.incompatible(before, retyped, "postcard"))


class AgainstMainTests(unittest.TestCase):
    def git(self, root, *args):
        subprocess.run(["git", *args], cwd=root, check=True, capture_output=True,
                       env=isolated_git_environment())

    SPEC = (
        'name = "demo"\nversion = 0\nversion_const = "crates/demo/wire/src/lib.rs:V"\n'
        'encoding = "json"\nsources = ["crates/demo/wire/src/lib.rs"]\nroots = ["S"]\n'
    )

    def source(self, version, fields):
        return f"pub const V: u32 = {version};\npub struct S {{ {fields} }}\n"

    def test_an_incompatible_change_needs_a_new_version(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            self.git(root, "init", "-q", "-b", "main")
            self.git(root, "config", "user.email", "t@example.com")
            self.git(root, "config", "user.name", "t")
            spec = root / "crates/demo/wire/interface.toml"
            code = root / "crates/demo/wire/src/lib.rs"
            code.parent.mkdir(parents=True)
            spec.write_text(self.SPEC)
            code.write_text(self.source(1, "a: u32"))
            self.git(root, "add", ".")
            self.git(root, "commit", "-qm", "base")

            code.write_text(self.source(1, "a: u32, b: u8"))
            problems = interfaces.against("main", root)
            self.assertEqual(len(problems), 1)
            self.assertIn("`S.b` is new", problems[0])

            code.write_text(self.source(2, "a: u32, b: u8"))
            self.assertEqual(interfaces.against("main", root), [])

            code.write_text(self.source(1, "a: u32, b: Option<u8>"))
            self.assertEqual(interfaces.against("main", root), [])

            # Its number moved to another crate: still compared, by name.
            moved = root / "crates/demo/other/interface.toml"
            moved_code = root / "crates/demo/other/src/lib.rs"
            moved_code.parent.mkdir(parents=True)
            moved.write_text(self.SPEC.replace("demo/wire", "demo/other"))
            moved_code.write_text(self.source(1, "a: u64"))
            spec.unlink()
            self.assertIn("`S.a` is now `u64`", interfaces.against("main", root)[0])

    def test_members_moved_to_another_interface_are_judged_there(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            self.git(root, "init", "-q", "-b", "main")
            self.git(root, "config", "user.email", "t@example.com")
            self.git(root, "config", "user.name", "t")
            spec = root / "crates/demo/wire/interface.toml"
            code = root / "crates/demo/wire/src/lib.rs"
            code.parent.mkdir(parents=True)
            spec.write_text(self.SPEC.replace('roots = ["S"]', 'roots = ["S", "T"]'))
            code.write_text(self.source(1, "a: u32") + "pub struct T { b: u8 }\n")
            self.git(root, "add", ".")
            self.git(root, "commit", "-qm", "base")

            # T leaves for an interface of its own: the one it left is unchanged.
            code.write_text(self.source(1, "a: u32"))
            spec.write_text(self.SPEC)
            other = root / "crates/demo/other/interface.toml"
            other_code = root / "crates/demo/other/src/lib.rs"
            other_code.parent.mkdir(parents=True)
            other.write_text(
                'name = "other"\nversion = 0\nversion_const = "crates/demo/other/src/lib.rs:V"\n'
                'encoding = "json"\nsources = ["crates/demo/other/src/lib.rs"]\nroots = ["T"]\n'
            )
            other_code.write_text("pub const V: u32 = 1;\npub struct T { b: u16 }\n")
            self.assertEqual(interfaces.against("main", root), [])

            # Gone from both is still gone.
            other.unlink()
            problems = interfaces.against("main", root)
            self.assertEqual(len(problems), 1)
            self.assertIn("`T` is gone", problems[0])


class RepositoryTests(unittest.TestCase):
    def test_every_committed_interface_matches_its_code(self):
        self.assertEqual(interfaces.check(), [])

    def test_the_six_interfaces_each_have_one_number(self):
        names = sorted(
            interfaces.load_spec(path)[0].name for path in interfaces.interface_files()
        )
        self.assertEqual(names, ["app-host", "messaging", "package", "peer", "runtime", "shell-ui"])


class EnvRuntimeFormatTests(unittest.TestCase):
    def test_the_env_runtime_declares_the_package_format_it_is_sealed_in(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            definition = root / interfaces.ENV_RUNTIME
            definition.parent.mkdir(parents=True)
            definition.write_text("# env-runtime-version: 1.0.0\n# package-format: 2\n")
            self.assertEqual(interfaces.env_runtime_format(2, root), [])
            # A new package format needs a new runtime: the old one names the
            # old format and fails the check until it is declared again.
            self.assertEqual(len(interfaces.env_runtime_format(3, root)), 1)
            definition.write_text("# env-runtime-version: 1.0.0\n")
            self.assertEqual(len(interfaces.env_runtime_format(2, root)), 1)


if __name__ == "__main__":
    unittest.main()
